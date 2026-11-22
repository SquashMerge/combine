//! The layout sync: debounce, self-write detection, conflicts, invalid files and read-only
//! projects.

mod layout_support;

use combined::core::{
    ConnectionId, FieldPath, LayoutEffect, LayoutOp, LayoutRead, LayoutTiming, LayoutWritten,
    MutateError, SplitKey, WatchError, LAYOUT_MAX_BYTES,
};
use combined::model::{LayoutRevision, Millis, SpaceLayout, SplitNode};
use layout_support::*;
use serde_json::{json, Value};

fn two_tiles() -> SpaceLayout {
    layout(split(
        0.5,
        tile("a", &["p1", "p2", "p3"], Some("p2")),
        tile("b", &["p4"], Some("p4")),
    ))
}

fn with_ratio(layout: &SpaceLayout, ratio: f64) -> SpaceLayout {
    let mut layout = layout.clone();
    if let SplitNode::Split(split) = &mut layout.root {
        split.ratio = ratio;
    }
    layout
}

fn root_ratio(layout: &SpaceLayout) -> f64 {
    match &layout.root {
        SplitNode::Split(split) => split.ratio,
        SplitNode::Tile(_) => panic!("the root is a tile"),
    }
}

fn find_tile<'a>(node: &'a SplitNode, id: &str) -> Option<&'a combined::model::Tile> {
    match node {
        SplitNode::Tile(tile) => (tile.id.as_str() == id).then_some(tile),
        SplitNode::Split(split) => find_tile(&split.a, id).or_else(|| find_tile(&split.b, id)),
    }
}

fn active_of(layout: &SpaceLayout, id: &str) -> Option<String> {
    find_tile(&layout.root, id)?
        .active
        .as_ref()
        .map(|panel| panel.as_str().to_string())
}

fn with_active(layout: &SpaceLayout, id: &str, panel: &str) -> SpaceLayout {
    let mut layout = layout.clone();
    fn walk(node: &mut SplitNode, id: &str, panel: &str) {
        match node {
            SplitNode::Tile(tile) if tile.id.as_str() == id => tile.active = Some(p(panel)),
            SplitNode::Tile(_) => {}
            SplitNode::Split(split) => {
                walk(&mut split.a, id, panel);
                walk(&mut split.b, id, panel);
            }
        }
    }
    walk(&mut layout.root, id, panel);
    layout
}

fn write_effects(effects: &[LayoutEffect]) -> Vec<(String, usize)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            LayoutEffect::Write {
                space, op_count, ..
            } => Some((space.0.clone(), *op_count)),
            _ => None,
        })
        .collect()
}

fn alternate(n: u64) -> Vec<LayoutOp> {
    vec![set_active(
        "a",
        if n.is_multiple_of(2) { "p3" } else { "p1" },
    )]
}

// AC2: debounce

#[test]
fn burst_of_mutations_produces_one_write() {
    let mut sim = Sim::open(&two_tiles());
    for n in 0..3 {
        sim.mutate(alternate(n), Millis(n * 100)).unwrap();
    }
    sim.layouts.tick(Millis(699));
    assert!(sim.layouts.take_effects().is_empty());
    sim.layouts.tick(Millis(700));
    let effects = sim.layouts.take_effects();
    assert_eq!(write_effects(&effects), [("main".to_string(), 3)]);
    sim.layouts.tick(Millis(10_000));
    assert!(sim.layouts.take_effects().is_empty());
}

#[test]
fn trailing_deadline_moves_with_each_mutation() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(500)));
    sim.mutate(alternate(1), Millis(300)).unwrap();
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(800)));
}

#[test]
fn max_wait_forces_write_during_continuous_burst() {
    let mut sim = Sim::open(&two_tiles());
    for n in 0..5 {
        sim.mutate(alternate(n), Millis(n * 400)).unwrap();
        sim.layouts.tick(Millis(n * 400));
        assert!(sim.layouts.take_effects().is_empty());
    }
    sim.mutate(alternate(5), Millis(2_000 - 1)).unwrap();
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(2_000)));
    sim.layouts.tick(Millis(2_000));
    let effects = sim.layouts.take_effects();
    assert_eq!(write_effects(&effects), [("main".to_string(), 6)]);
}

#[test]
fn watch_switch_flushes_immediately() {
    let mut sim = Sim::open(&two_tiles());
    sim.disk.insert(space("other"), bytes_of(&two_tiles()));
    sim.mutate(alternate(0), Millis(0)).unwrap();
    sim.layouts
        .watch(MAIN, space("other"), 2, true, Millis(10))
        .unwrap();
    let effects = sim.layouts.take_effects();
    assert_eq!(write_effects(&effects), [("main".to_string(), 1)]);
    assert!(effects.contains(&LayoutEffect::Read {
        space: space("other")
    }));
    assert_eq!(sim.layouts.next_deadline(), None);
}

#[test]
fn unwatch_flushes_immediately() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    sim.layouts.unwatch(MAIN, Millis(10));
    let effects = sim.layouts.take_effects();
    assert_eq!(write_effects(&effects), [("main".to_string(), 1)]);
}

#[test]
fn disconnect_flushes_immediately() {
    let mut sim = Sim::open(&two_tiles());
    sim.watch(ConnectionId(2), "main", Millis(0));
    sim.mutate(alternate(0), Millis(0)).unwrap();
    sim.layouts.unwatch(MAIN, Millis(10));
    let effects = sim.layouts.take_effects();
    assert_eq!(write_effects(&effects), [("main".to_string(), 1)]);
    assert_eq!(sim.layouts.subscribers(&space("main")), [ConnectionId(2)]);
}

#[test]
fn flush_all_writes_every_pending_space() {
    let mut sim = Sim::open(&two_tiles());
    sim.disk.insert(space("other"), bytes_of(&two_tiles()));
    sim.watch(ConnectionId(2), "other", Millis(0));
    sim.mutate(alternate(0), Millis(0)).unwrap();
    let base = sim.revision("other");
    sim.layouts
        .mutate(
            ConnectionId(2),
            &space("other"),
            1,
            base,
            alternate(0),
            Millis(0),
        )
        .unwrap();
    sim.layouts.flush_all(Millis(1));
    let mut writes = write_effects(&sim.layouts.take_effects());
    writes.sort();
    assert_eq!(writes, [("main".to_string(), 1), ("other".to_string(), 1)]);
}

#[test]
fn no_write_until_first_mutation() {
    let mut sim = Sim::open(&two_tiles());
    assert_eq!(sim.layouts.next_deadline(), None);
    sim.layouts.tick(Millis(100_000));
    sim.layouts.flush_all(Millis(100_000));
    sim.layouts.unwatch(MAIN, Millis(100_000));
    assert!(sim.layouts.take_effects().is_empty());
}

#[test]
fn tick_leaves_no_deadline_at_or_before_now() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    sim.layouts.tick(Millis(500));
    assert_eq!(write_effects(&sim.layouts.take_effects()).len(), 1);
    assert_eq!(sim.layouts.next_deadline(), None);

    sim.write_outcomes
        .push_back(LayoutWritten::Failed("busy".to_string()));
    sim.settle(Millis(500));
    let now = Millis(500);
    assert!(sim
        .layouts
        .next_deadline()
        .is_none_or(|deadline| deadline > now));
    sim.mutate(alternate(1), Millis(600)).unwrap();
    sim.layouts.tick(Millis(1_100));
    sim.layouts.take_effects();
    assert!(sim
        .layouts
        .next_deadline()
        .is_none_or(|deadline| deadline > Millis(1_100)));
}

#[test]
fn next_deadline_none_while_invalid_file_suppresses_writes() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    assert!(sim.layouts.next_deadline().is_some());
    sim.outside_write(b"{ nope".to_vec(), Millis(100));
    assert_eq!(sim.layouts.next_deadline(), None);
    sim.mutate(alternate(1), Millis(200)).unwrap();
    assert_eq!(sim.layouts.next_deadline(), None);
}

#[test]
fn custom_timing_is_honoured() {
    let mut sim = Sim::new(LayoutTiming {
        debounce_ms: 10,
        max_wait_ms: 30,
        retry_ms: 100,
    });
    sim.disk.insert(space("main"), bytes_of(&two_tiles()));
    sim.watch(MAIN, "main", Millis(0));
    sim.mutate(alternate(0), Millis(0)).unwrap();
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(10)));
    sim.mutate(alternate(1), Millis(8)).unwrap();
    sim.mutate(alternate(2), Millis(16)).unwrap();
    sim.mutate(alternate(3), Millis(24)).unwrap();
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(30)));
    sim.layouts.tick(Millis(30));
    sim.write_outcomes
        .push_back(LayoutWritten::Failed("busy".to_string()));
    sim.settle(Millis(30));
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(130)));
}

// AC3: self-writes and outside reloads

#[test]
fn own_write_change_causes_no_reload() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    sim.layouts.tick(Millis(500));
    sim.settle(Millis(500));
    assert_eq!(sim.written(), 1);
    sim.layouts.take_changed();
    sim.layouts.take_replies();
    let revision = sim.revision("main");
    let status = sim.layouts.status(&space("main")).unwrap();

    sim.layouts.apply_change(&space("main"));
    sim.settle(Millis(510));

    assert_eq!(sim.revision("main"), revision);
    assert_eq!(sim.layouts.status(&space("main")).unwrap(), status);
    assert!(sim.layouts.take_changed().is_empty());
    assert!(sim.layouts.take_effects().is_empty());
}

#[test]
fn identical_outside_reload_still_bumps_revision() {
    let mut sim = Sim::open(&two_tiles());
    let revision = sim.revision("main");
    let mut bytes = bytes_of(&two_tiles());
    bytes.push(b'\n');
    sim.outside_write(bytes, Millis(10));
    assert_eq!(sim.revision("main"), LayoutRevision(revision.0 + 1));
    assert_eq!(sim.layouts.take_changed(), [space("main")]);
    assert_eq!(sim.current("main"), two_tiles());
}

#[test]
fn outside_edit_with_new_hash_reloads() {
    let mut sim = Sim::open(&two_tiles());
    let revision = sim.revision("main");
    let edited = with_ratio(&two_tiles(), 0.3);
    sim.outside_write(bytes_of(&edited), Millis(10));
    assert_eq!(sim.revision("main"), LayoutRevision(revision.0 + 1));
    assert_eq!(sim.current("main"), edited);
    assert_eq!(sim.layouts.take_changed(), [space("main")]);
}

#[test]
fn outside_revert_to_earlier_daemon_write_reloads() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![set_active("a", "p3")], Millis(0)).unwrap();
    sim.layouts.flush_all(Millis(1));
    sim.settle(Millis(1));
    sim.mutate(vec![set_active("a", "p1")], Millis(2)).unwrap();
    sim.layouts.flush_all(Millis(3));
    sim.settle(Millis(3));
    assert_eq!(sim.written(), 2);
    let first = sim.writes[0].1.clone();
    let revision = sim.revision("main");

    sim.outside_write(first, Millis(10));

    assert_eq!(sim.revision("main"), LayoutRevision(revision.0 + 1));
    assert_eq!(active_of(&sim.current("main"), "a").as_deref(), Some("p3"));
}

#[test]
fn invalid_then_revert_to_last_good_bytes_reloads_and_resumes_writes() {
    let mut sim = Sim::open(&two_tiles());
    let good = bytes_of(&two_tiles());
    sim.outside_write(b"[]".to_vec(), Millis(10));
    assert!(sim.layouts.status(&space("main")).unwrap().error.is_some());
    let revision = sim.revision("main");

    sim.outside_write(good, Millis(20));

    assert_eq!(sim.revision("main"), LayoutRevision(revision.0 + 1));
    assert_eq!(sim.layouts.status(&space("main")).unwrap().error, None);
    sim.mutate(alternate(0), Millis(30)).unwrap();
    sim.layouts.tick(Millis(530));
    assert_eq!(write_effects(&sim.layouts.take_effects()).len(), 1);
}

#[test]
fn missing_with_no_known_hash_is_ignored() {
    let mut sim = Sim::new(LayoutTiming::default());
    sim.watch(MAIN, "main", Millis(0));
    assert_eq!(sim.revision("main"), LayoutRevision(0));
    sim.layouts.apply_change(&space("main"));
    sim.settle(Millis(10));
    assert_eq!(sim.revision("main"), LayoutRevision(0));
    assert!(sim.layouts.take_changed().is_empty());
}

// AC4: conflicts

#[test]
fn outside_edit_disjoint_fields_reapplies_pending() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![set_active("a", "p3")], Millis(0)).unwrap();
    let deadline = sim.layouts.next_deadline();

    sim.outside_write(bytes_of(&with_ratio(&two_tiles(), 0.3)), Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.dropped, None);
    assert_eq!(root_ratio(&status.layout), 0.3);
    assert_eq!(active_of(&status.layout, "a").as_deref(), Some("p3"));
    assert_eq!(sim.layouts.next_deadline(), deadline);
    sim.layouts.tick(Millis(500));
    assert_eq!(
        write_effects(&sim.layouts.take_effects()),
        [("main".to_string(), 1)]
    );
}

#[test]
fn outside_edit_overlapping_fields_drops_and_reports() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![set_active("a", "p3")], Millis(0)).unwrap();
    let outside = with_active(&two_tiles(), "a", "p1");

    sim.outside_write(bytes_of(&outside), Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, outside);
    let dropped = status.dropped.unwrap();
    assert_eq!(dropped.op_count, 1);
    assert!(dropped.fields.contains(&FieldPath::TileActive(t("a"))));
    assert_eq!(sim.layouts.next_deadline(), None);
}

#[test]
fn outside_structure_change_drops_pending_split() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(
        vec![LayoutOp::SplitTile {
            tile: t("b"),
            dir: combined::model::SplitDir::V,
            new_tile: t("c"),
            ratio: 0.5,
            panel: None,
        }],
        Millis(0),
    )
    .unwrap();
    let outside = layout(tile("a", &["p1", "p2", "p3"], Some("p2")));

    sim.outside_write(bytes_of(&outside), Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, outside);
    assert!(status
        .dropped
        .unwrap()
        .fields
        .contains(&FieldPath::Structure));
}

#[test]
fn reapply_precondition_failure_drops() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![set_active("a", "p3")], Millis(0)).unwrap();
    let outside = layout(split(
        0.5,
        tile("a", &["p1", "p2"], Some("p2")),
        tile("b", &["p4"], Some("p4")),
    ));

    sim.outside_write(bytes_of(&outside), Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, outside);
    assert_eq!(status.dropped.unwrap().op_count, 1);
}

#[test]
fn reapply_invalid_result_drops() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![set_ratio(&["a", "b"], 0.123456789012345)], Millis(0))
        .unwrap();
    let mut outside = two_tiles();
    outside.extra.insert("pad".to_string(), json!(""));
    let overhead = bytes_of(&outside).len();
    outside.extra.insert(
        "pad".to_string(),
        json!("x".repeat(LAYOUT_MAX_BYTES - overhead)),
    );
    let bytes = bytes_of(&outside);
    assert_eq!(bytes.len(), LAYOUT_MAX_BYTES);

    sim.outside_write(bytes, Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, outside);
    assert_eq!(status.error, None);
    assert_eq!(status.dropped.unwrap().op_count, 1);
}

#[test]
fn rebase_recomputes_touched_and_origins() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(
        vec![
            LayoutOp::SplitTile {
                tile: t("a"),
                dir: combined::model::SplitDir::V,
                new_tile: t("c"),
                ratio: 0.5,
                panel: None,
            },
            set_ratio(&["a", "b", "c"], 0.4),
        ],
        Millis(0),
    )
    .unwrap();
    // A disjoint outside edit: only b's active panel changes.
    let outside = layout(split(
        0.5,
        tile("a", &["p1", "p2", "p3"], Some("p2")),
        tile("b", &["p4", "p5"], Some("p4")),
    ));
    sim.outside_write(bytes_of(&outside), Millis(10));
    assert_eq!(sim.layouts.status(&space("main")).unwrap().dropped, None);

    // The origin map was rebuilt, so the split keeps its origin {a, b}.
    sim.mutate(vec![set_ratio(&["a", "b", "c"], 0.35)], Millis(20))
        .unwrap();
    sim.outside_write(bytes_of(&with_ratio(&outside, 0.7)), Millis(30));

    let dropped = sim.layouts.status(&space("main")).unwrap().dropped.unwrap();
    let key = SplitKey([t("a"), t("b")].into_iter().collect());
    assert!(dropped.fields.contains(&FieldPath::Ratio(key)));
    assert_eq!(dropped.op_count, 3);
}

fn three_tiles_closable() -> SpaceLayout {
    layout(split(
        0.5,
        split(0.5, tile("t1", &[], None), tile("t2", &[], None)),
        tile("t3", &[], None),
    ))
}

#[test]
fn pending_close_then_set_ratio_vs_outside_ratio_on_same_split_drops() {
    let mut sim = Sim::open(&three_tiles_closable());
    sim.mutate(vec![close("t3"), set_ratio(&["t1", "t2"], 0.3)], Millis(0))
        .unwrap();
    let mut outside = three_tiles_closable();
    if let SplitNode::Split(root) = &mut outside.root {
        if let SplitNode::Split(inner) = &mut *root.a {
            inner.ratio = 0.8;
        }
    }

    sim.outside_write(bytes_of(&outside), Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, outside);
    let key = SplitKey([t("t1"), t("t2")].into_iter().collect());
    assert!(status
        .dropped
        .unwrap()
        .fields
        .contains(&FieldPath::Ratio(key)));
}

#[test]
fn pending_close_then_set_ratio_on_ancestor_vs_outside_ancestor_ratio_drops() {
    let start = layout(split(
        0.5,
        tile("t1", &[], None),
        split(0.5, tile("t2", &[], None), tile("t3", &[], None)),
    ));
    let mut sim = Sim::open(&start);
    sim.mutate(vec![close("t3"), set_ratio(&["t1", "t2"], 0.3)], Millis(0))
        .unwrap();

    sim.outside_write(bytes_of(&with_ratio(&start, 0.9)), Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(root_ratio(&status.layout), 0.9);
    let key = SplitKey([t("t1"), t("t2"), t("t3")].into_iter().collect());
    assert!(status
        .dropped
        .unwrap()
        .fields
        .contains(&FieldPath::Ratio(key)));
}

#[test]
fn pending_close_vs_outside_panel_added_to_closed_tile_drops() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![close("b")], Millis(0)).unwrap();
    let outside = layout(split(
        0.5,
        tile("a", &["p1", "p2", "p3"], Some("p2")),
        tile("b", &["p4", "p9"], Some("p4")),
    ));

    sim.outside_write(bytes_of(&outside), Millis(100));

    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, outside);
    assert!(status
        .dropped
        .unwrap()
        .fields
        .contains(&FieldPath::TilePanels(t("b"))));
}

#[test]
fn dropped_report_clears_on_next_mutation() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![set_active("a", "p3")], Millis(0)).unwrap();
    sim.outside_write(bytes_of(&with_active(&two_tiles(), "a", "p1")), Millis(10));
    assert!(sim
        .layouts
        .status(&space("main"))
        .unwrap()
        .dropped
        .is_some());
    sim.mutate(vec![set_active("a", "p2")], Millis(20)).unwrap();
    assert_eq!(sim.layouts.status(&space("main")).unwrap().dropped, None);

    sim.outside_write(bytes_of(&with_active(&two_tiles(), "a", "p3")), Millis(30));
    assert!(sim
        .layouts
        .status(&space("main"))
        .unwrap()
        .dropped
        .is_some());
    sim.outside_write(bytes_of(&with_ratio(&two_tiles(), 0.2)), Millis(40));
    assert_eq!(sim.layouts.status(&space("main")).unwrap().dropped, None);
}

// AC5: invalid files

#[test]
fn invalid_json_keeps_last_good_and_reports() {
    let mut sim = Sim::open(&two_tiles());
    let revision = sim.revision("main");
    sim.outside_write(b"{ not json".to_vec(), Millis(10));
    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, two_tiles());
    assert!(status
        .error
        .unwrap()
        .starts_with("layout.json can't be used: "));
    assert_eq!(status.layout_revision, LayoutRevision(revision.0 + 1));
    assert_eq!(sim.layouts.take_changed(), [space("main")]);
}

#[test]
fn invalid_error_says_changes_unsaved() {
    let mut sim = Sim::open(&two_tiles());
    sim.outside_write(b"[]".to_vec(), Millis(10));
    let error = sim.layouts.status(&space("main")).unwrap().error.unwrap();
    assert!(error.contains(
        "Showing the last good layout. Changes made now aren't saved, and are lost if the \
         daemon restarts, until layout.json is fixed."
    ));
}

#[test]
fn unsupported_version_keeps_last_good() {
    let mut sim = Sim::open(&two_tiles());
    let mut value: Value = serde_json::from_slice(&bytes_of(&two_tiles())).unwrap();
    value["version"] = json!(2);
    sim.outside_write(serde_json::to_vec(&value).unwrap(), Millis(10));
    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, two_tiles());
    assert!(status.error.unwrap().contains("isn't supported"));
}

#[test]
fn invalid_file_never_written_after_later_mutation() {
    let mut sim = Sim::open(&two_tiles());
    sim.outside_write(b"garbage".to_vec(), Millis(10));
    sim.mutate(alternate(0), Millis(20)).unwrap();
    sim.layouts.tick(Millis(100_000));
    sim.layouts.flush_all(Millis(100_000));
    sim.layouts.unwatch(MAIN, Millis(100_000));
    sim.settle(Millis(100_000));
    assert_eq!(sim.written(), 0);
    assert_eq!(sim.disk[&space("main")], b"garbage".to_vec());
}

#[test]
fn valid_fix_resumes_writes_under_conflict_policy() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(vec![set_active("a", "p3")], Millis(0)).unwrap();
    sim.outside_write(b"garbage".to_vec(), Millis(100));
    sim.layouts.tick(Millis(10_000));
    assert!(sim.layouts.take_effects().is_empty());

    sim.outside_write(bytes_of(&with_ratio(&two_tiles(), 0.3)), Millis(20_000));

    assert_eq!(sim.layouts.status(&space("main")).unwrap().error, None);
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(20_500)));
    sim.layouts.tick(Millis(20_500));
    sim.settle(Millis(20_500));
    let written = &sim.writes[0].1;
    let current = sim.current("main");
    assert_eq!(written, &bytes_of(&current));
    assert_eq!(root_ratio(&current), 0.3);
    assert_eq!(active_of(&current, "a").as_deref(), Some("p3"));

    // A fix that conflicts with the held batch drops it instead.
    sim.mutate(vec![set_active("a", "p1")], Millis(21_000))
        .unwrap();
    sim.outside_write(b"garbage".to_vec(), Millis(21_100));
    sim.outside_write(bytes_of(&with_active(&current, "a", "p2")), Millis(21_200));
    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.dropped.unwrap().op_count, 1);
    assert_eq!(sim.layouts.next_deadline(), None);
}

// AC6: read-only

#[test]
fn read_only_write_outcome_marks_space() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    let revision = sim.revision("main");
    sim.write_outcomes
        .push_back(LayoutWritten::ReadOnly("permission denied".to_string()));
    sim.layouts.tick(Millis(500));
    sim.settle(Millis(500));
    let status = sim.layouts.status(&space("main")).unwrap();
    assert!(status.read_only);
    assert_eq!(
        status.error.as_deref(),
        Some(
            "Can't write .workspace/ (permission denied). Changes aren't saved, and are lost \
             if the daemon restarts."
        )
    );
    assert_eq!(status.layout_revision, LayoutRevision(revision.0 + 1));

    sim.layouts.tick(Millis(5_500));
    sim.settle(Millis(5_500));
    let status = sim.layouts.status(&space("main")).unwrap();
    assert!(!status.read_only);
    assert_eq!(status.error, None);
}

#[test]
fn read_only_without_fallback_keeps_pending_and_retries() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    sim.write_outcomes
        .push_back(LayoutWritten::ReadOnly("permission denied".to_string()));
    sim.layouts.tick(Millis(500));
    sim.settle(Millis(500));
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(5_500)));
    sim.layouts.tick(Millis(5_499));
    assert!(sim.layouts.take_effects().is_empty());
    sim.layouts.tick(Millis(5_500));
    assert_eq!(
        write_effects(&sim.layouts.take_effects()),
        [("main".to_string(), 1)]
    );
}

// AC7: restarts

#[test]
fn restart_over_same_disk_restores_identical_layout() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(
        vec![set_active("a", "p3"), set_ratio(&["a", "b"], 0.25)],
        Millis(0),
    )
    .unwrap();
    sim.layouts.flush_all(Millis(1));
    sim.settle(Millis(1));
    let before = sim.current("main");

    let mut restarted = Sim::new(LayoutTiming::default());
    restarted.disk = sim.disk.clone();
    restarted.watch(MAIN, "main", Millis(0));

    assert_eq!(restarted.current("main"), before);
}

#[test]
fn start_treats_disk_as_authoritative() {
    let mut sim = Sim::open(&two_tiles());
    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, two_tiles());
    assert_eq!(status.layout_revision, LayoutRevision(0));
    assert_eq!(status.dropped, None);
    assert_eq!(sim.layouts.next_deadline(), None);
    sim.layouts.flush_all(Millis(1));
    assert!(sim.layouts.take_effects().is_empty());

    let mut missing = Sim::new(LayoutTiming::default());
    missing.watch(MAIN, "main", Millis(0));
    assert_eq!(missing.current("main"), combined::core::default_layout());
}

// Protocol errors

#[test]
fn mutate_with_stale_base_rejected() {
    let mut sim = Sim::open(&two_tiles());
    let revision = sim.revision("main");
    let stale = LayoutRevision(revision.0 + 1);
    assert_eq!(
        sim.layouts
            .mutate(MAIN, &space("main"), 1, stale, alternate(0), Millis(0)),
        Err(MutateError::Stale)
    );
    sim.mutate(alternate(0), Millis(0)).unwrap();
    assert_eq!(
        sim.layouts
            .mutate(MAIN, &space("main"), 1, revision, alternate(1), Millis(1)),
        Err(MutateError::Stale)
    );
    assert_eq!(
        sim.layouts.status(&space("main")).unwrap().layout_revision,
        LayoutRevision(1)
    );
}

#[test]
fn watch_unknown_space_rejected() {
    let mut sim = Sim::new(LayoutTiming::default());
    assert_eq!(
        sim.layouts
            .watch(MAIN, space("nowhere"), 1, false, Millis(0)),
        Err(WatchError::UnknownSpace)
    );
    assert!(sim.layouts.take_effects().is_empty());
    assert!(sim.layouts.subscribers(&space("nowhere")).is_empty());
    assert!(sim.layouts.take_replies().is_empty());
}

#[test]
fn mutate_without_watch_rejected() {
    let mut sim = Sim::open(&two_tiles());
    let base = sim.revision("main");
    assert_eq!(
        sim.layouts.mutate(
            ConnectionId(2),
            &space("main"),
            1,
            base,
            alternate(0),
            Millis(0)
        ),
        Err(MutateError::NotWatched)
    );
    assert_eq!(
        sim.layouts
            .mutate(MAIN, &space("nowhere"), 1, base, alternate(0), Millis(0)),
        Err(MutateError::UnknownSpace)
    );
    assert_eq!(sim.revision("main"), base);
}

#[test]
fn subscribers_follow_watch_switch_and_unwatch() {
    let mut sim = Sim::open(&two_tiles());
    sim.disk.insert(space("other"), bytes_of(&two_tiles()));
    sim.watch(ConnectionId(2), "main", Millis(0));
    assert_eq!(
        sim.layouts.subscribers(&space("main")),
        [MAIN, ConnectionId(2)]
    );
    sim.watch(ConnectionId(2), "other", Millis(1));
    assert_eq!(sim.layouts.subscribers(&space("main")), [MAIN]);
    assert_eq!(sim.layouts.subscribers(&space("other")), [ConnectionId(2)]);
    sim.layouts.unwatch(MAIN, Millis(2));
    assert!(sim.layouts.subscribers(&space("main")).is_empty());
}

#[test]
fn write_failure_retries_after_retry_interval() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    sim.write_outcomes
        .push_back(LayoutWritten::Failed("disk full".to_string()));
    sim.layouts.tick(Millis(500));
    sim.settle(Millis(500));
    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(
        status.error.as_deref(),
        Some("Couldn't save layout.json (disk full); retrying.")
    );
    assert_eq!(sim.layouts.next_deadline(), Some(Millis(5_500)));
    sim.layouts.tick(Millis(5_500));
    sim.settle(Millis(5_500));
    assert_eq!(sim.written(), 2);
    assert_eq!(sim.layouts.status(&space("main")).unwrap().error, None);
}

#[test]
fn failed_read_keeps_state_and_reports_once() {
    let mut sim = Sim::open(&two_tiles());
    sim.mutate(alternate(0), Millis(0)).unwrap();
    let revision = sim.revision("main");
    let layout_before = sim.current("main");
    sim.layouts.take_changed();

    let failed = || LayoutRead::Failed("permission denied".to_string());
    sim.layouts.apply_read(&space("main"), failed(), Millis(10));
    let status = sim.layouts.status(&space("main")).unwrap();
    assert_eq!(status.layout, layout_before);
    assert_eq!(
        status.error.as_deref(),
        Some("Couldn't read layout.json (permission denied); showing the last good layout.")
    );
    assert_eq!(status.layout_revision, LayoutRevision(revision.0 + 1));
    assert_eq!(sim.layouts.take_changed(), [space("main")]);

    sim.layouts.apply_read(&space("main"), failed(), Millis(20));
    assert_eq!(sim.revision("main"), LayoutRevision(revision.0 + 1));
    assert!(sim.layouts.take_changed().is_empty());

    // Writes still go out, and a good re-read of the unchanged file clears the error.
    sim.layouts.tick(Millis(500));
    assert_eq!(write_effects(&sim.layouts.take_effects()).len(), 1);
    sim.settle(Millis(500));
    sim.layouts.apply_change(&space("main"));
    sim.settle(Millis(600));
    assert_eq!(sim.layouts.status(&space("main")).unwrap().error, None);
}
