//! Typed layout ops: effects, touched fields, the origin map and atomicity.

use std::collections::BTreeSet;

use combined::core::{
    apply_ops, default_layout, diff_fields, identity_origins, validate_layout, AppliedOps,
    FieldPath, LayoutError, LayoutOp, SplitKey, LAYOUT_MAX_BYTES, LAYOUT_MAX_DEPTH,
};
use combined::model::{PanelId, SpaceLayout, Split, SplitDir, SplitNode, Tile, TileId};
use serde_json::{json, Map};

fn t(id: &str) -> TileId {
    TileId::new(id).unwrap()
}

fn p(id: &str) -> PanelId {
    PanelId::new(id).unwrap()
}

fn tile(id: &str, panels: &[&str], active: Option<&str>) -> SplitNode {
    SplitNode::Tile(Tile {
        id: t(id),
        panels: panels.iter().map(|panel| p(panel)).collect(),
        active: active.map(p),
        extra: Map::new(),
    })
}

fn split(ratio: f64, a: SplitNode, b: SplitNode) -> SplitNode {
    SplitNode::Split(Split {
        dir: SplitDir::H,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
        extra: Map::new(),
    })
}

fn layout(root: SplitNode) -> SpaceLayout {
    SpaceLayout {
        version: 2,
        root,
        groups: json!([]),
        extra: Map::new(),
    }
}

fn key(ids: &[&str]) -> SplitKey {
    SplitKey(ids.iter().map(|id| t(id)).collect())
}

fn apply(layout: &SpaceLayout, ops: &[LayoutOp]) -> Result<AppliedOps, LayoutError> {
    apply_ops(layout, &identity_origins(layout), ops)
}

fn tile_of<'a>(node: &'a SplitNode, id: &str) -> Option<&'a Tile> {
    match node {
        SplitNode::Tile(tile) => (tile.id.as_str() == id).then_some(tile),
        SplitNode::Split(split) => tile_of(&split.a, id).or_else(|| tile_of(&split.b, id)),
    }
}

fn two_tiles() -> SpaceLayout {
    layout(split(
        0.5,
        tile("a", &["p1", "p2", "p3"], Some("p2")),
        tile("b", &["p4"], Some("p4")),
    ))
}

#[test]
fn set_active_touches_tile_active() {
    let applied = apply(
        &two_tiles(),
        &[LayoutOp::SetActive {
            tile: t("a"),
            panel: p("p3"),
        }],
    )
    .unwrap();
    assert_eq!(
        tile_of(&applied.layout.root, "a").unwrap().active,
        Some(p("p3"))
    );
    assert_eq!(
        applied.touched,
        [FieldPath::TileActive(t("a"))].into_iter().collect()
    );
    let missing = apply(
        &two_tiles(),
        &[LayoutOp::SetActive {
            tile: t("a"),
            panel: p("p4"),
        }],
    );
    assert!(matches!(missing, Err(LayoutError::PanelNotInTile(..))));
}

#[test]
fn set_ratio_addresses_split_by_leaf_set() {
    let nested = layout(split(
        0.5,
        tile("a", &[], None),
        split(0.5, tile("b", &[], None), tile("c", &[], None)),
    ));
    let op = LayoutOp::SetRatio {
        leaves: key(&["c", "b"]).0,
        ratio: 0.8,
    };
    let applied = apply(&nested, &[op]).unwrap();
    let SplitNode::Split(root) = &applied.layout.root else {
        panic!("split")
    };
    assert_eq!(root.ratio, 0.5);
    let SplitNode::Split(inner) = &*root.b else {
        panic!("split")
    };
    assert_eq!(inner.ratio, 0.8);
    assert_eq!(
        applied.touched,
        [FieldPath::Ratio(key(&["b", "c"]))].into_iter().collect()
    );
    let unknown = LayoutOp::SetRatio {
        leaves: key(&["a", "b"]).0,
        ratio: 0.8,
    };
    assert_eq!(
        apply(&nested, &[unknown]).unwrap_err(),
        LayoutError::UnknownSplit
    );
}

#[test]
fn move_panel_updates_both_tiles_and_rule_r() {
    let op = LayoutOp::MovePanel {
        panel: p("p2"),
        to_tile: t("b"),
        index: 0,
    };
    let applied = apply(&two_tiles(), &[op]).unwrap();
    let a = tile_of(&applied.layout.root, "a").unwrap();
    assert_eq!(a.panels, vec![p("p1"), p("p3")]);
    assert_eq!(a.active, Some(p("p3")));
    let b = tile_of(&applied.layout.root, "b").unwrap();
    assert_eq!(b.panels, vec![p("p2"), p("p4")]);
    assert_eq!(b.active, Some(p("p2")));
    let expected: BTreeSet<_> = [
        FieldPath::TilePanels(t("a")),
        FieldPath::TileActive(t("a")),
        FieldPath::TilePanels(t("b")),
        FieldPath::TileActive(t("b")),
    ]
    .into_iter()
    .collect();
    assert_eq!(applied.touched, expected);

    let last = LayoutOp::MovePanel {
        panel: p("p3"),
        to_tile: t("b"),
        index: 99,
    };
    let after_last = apply(&two_tiles(), &[last]).unwrap();
    let a = tile_of(&after_last.layout.root, "a").unwrap();
    assert_eq!(a.active, Some(p("p2")));
    let b = tile_of(&after_last.layout.root, "b").unwrap();
    assert_eq!(b.panels, vec![p("p4"), p("p3")]);

    let only = layout(split(
        0.5,
        tile("a", &["p1"], Some("p1")),
        tile("b", &[], None),
    ));
    let emptied = apply(
        &only,
        &[LayoutOp::MovePanel {
            panel: p("p1"),
            to_tile: t("b"),
            index: 0,
        }],
    )
    .unwrap();
    assert_eq!(tile_of(&emptied.layout.root, "a").unwrap().active, None);

    let kept = LayoutOp::MovePanel {
        panel: p("p1"),
        to_tile: t("b"),
        index: 0,
    };
    let non_active = apply(&two_tiles(), &[kept]).unwrap();
    assert_eq!(
        tile_of(&non_active.layout.root, "a").unwrap().active,
        Some(p("p2"))
    );
}

#[test]
fn split_tile_moves_panel_out_and_recomputes_active() {
    let op = LayoutOp::SplitTile {
        tile: t("a"),
        dir: SplitDir::V,
        new_tile: t("n"),
        ratio: 0.4,
        panel: Some(p("p2")),
    };
    let applied = apply(&two_tiles(), &[op]).unwrap();
    let a = tile_of(&applied.layout.root, "a").unwrap();
    assert_eq!(a.panels, vec![p("p1"), p("p3")]);
    assert_eq!(a.active, Some(p("p3")));
    let n = tile_of(&applied.layout.root, "n").unwrap();
    assert_eq!(n.panels, vec![p("p2")]);
    assert_eq!(n.active, Some(p("p2")));
    let expected: BTreeSet<_> = [
        FieldPath::Structure,
        FieldPath::TilePanels(t("a")),
        FieldPath::TileActive(t("a")),
    ]
    .into_iter()
    .collect();
    assert_eq!(applied.touched, expected);

    let empty = LayoutOp::SplitTile {
        tile: t("b"),
        dir: SplitDir::H,
        new_tile: t("e"),
        ratio: 0.5,
        panel: None,
    };
    let applied = apply(&two_tiles(), &[empty]).unwrap();
    let e = tile_of(&applied.layout.root, "e").unwrap();
    assert!(e.panels.is_empty() && e.active.is_none());
    assert_eq!(
        applied.touched,
        [FieldPath::Structure].into_iter().collect()
    );
}

#[test]
fn split_tile_rejects_used_or_malformed_id() {
    let used = LayoutOp::SplitTile {
        tile: t("a"),
        dir: SplitDir::H,
        new_tile: t("b"),
        ratio: 0.5,
        panel: None,
    };
    assert_eq!(
        apply(&two_tiles(), &[used]).unwrap_err(),
        LayoutError::TileIdInUse(t("b"))
    );
    assert!(TileId::new("").is_err());
    assert!(TileId::new("bad\u{7}id").is_err());
    let foreign = LayoutOp::SplitTile {
        tile: t("a"),
        dir: SplitDir::H,
        new_tile: t("n"),
        ratio: 0.5,
        panel: Some(p("p4")),
    };
    assert!(matches!(
        apply(&two_tiles(), &[foreign]),
        Err(LayoutError::PanelNotInTile(..))
    ));
    let bad_ratio = LayoutOp::SplitTile {
        tile: t("a"),
        dir: SplitDir::H,
        new_tile: t("n"),
        ratio: 1.0,
        panel: None,
    };
    assert!(matches!(
        apply(&two_tiles(), &[bad_ratio]),
        Err(LayoutError::RatioOutOfRange(_))
    ));
}

#[test]
fn close_tile_touches_structure_panels_active() {
    let three = layout(split(
        0.5,
        tile("a", &["p1"], Some("p1")),
        split(0.3, tile("b", &["p2"], Some("p2")), tile("c", &[], None)),
    ));
    let applied = apply(&three, &[LayoutOp::CloseTile { tile: t("b") }]).unwrap();
    let SplitNode::Split(root) = &applied.layout.root else {
        panic!("split")
    };
    assert!(matches!(&*root.b, SplitNode::Tile(tile) if tile.id == t("c")));
    assert!(tile_of(&applied.layout.root, "b").is_none());
    let expected: BTreeSet<_> = [
        FieldPath::Structure,
        FieldPath::TilePanels(t("b")),
        FieldPath::TileActive(t("b")),
    ]
    .into_iter()
    .collect();
    assert_eq!(applied.touched, expected);
}

#[test]
fn close_root_tile_rejected() {
    let result = apply(
        &default_layout(),
        &[LayoutOp::CloseTile { tile: t("main") }],
    );
    assert_eq!(result.unwrap_err(), LayoutError::CannotCloseRoot);
    let unknown = apply(&two_tiles(), &[LayoutOp::CloseTile { tile: t("zzz") }]);
    assert_eq!(unknown.unwrap_err(), LayoutError::UnknownTile(t("zzz")));
}

#[test]
fn ops_apply_atomically() {
    let before = two_tiles();
    let origins = identity_origins(&before);
    let ops = [
        LayoutOp::SetActive {
            tile: t("a"),
            panel: p("p1"),
        },
        LayoutOp::SetActive {
            tile: t("a"),
            panel: p("nope"),
        },
    ];
    assert!(apply_ops(&before, &origins, &ops).is_err());
    assert_eq!(before, two_tiles());
    assert_eq!(origins, identity_origins(&before));
    let invalid_at_end = [LayoutOp::SetRatio {
        leaves: key(&["a", "b"]).0,
        ratio: 2.0,
    }];
    assert!(matches!(
        apply_ops(&before, &origins, &invalid_at_end),
        Err(LayoutError::RatioOutOfRange(_))
    ));
    let transient = [
        LayoutOp::SetRatio {
            leaves: key(&["a", "b"]).0,
            ratio: 2.0,
        },
        LayoutOp::SetRatio {
            leaves: key(&["a", "b"]).0,
            ratio: 0.9,
        },
    ];
    assert!(apply_ops(&before, &origins, &transient).is_ok());
}

#[test]
fn ops_exceeding_depth_rejected() {
    let mut current = default_layout();
    let mut tile_id = "main".to_string();
    for level in 0..LAYOUT_MAX_DEPTH - 1 {
        let new_tile = format!("n{level}");
        let op = LayoutOp::SplitTile {
            tile: t(&tile_id),
            dir: SplitDir::H,
            new_tile: t(&new_tile),
            ratio: 0.5,
            panel: None,
        };
        current = apply(&current, &[op]).unwrap().layout;
        tile_id = new_tile;
    }
    validate_layout(&current).unwrap();
    let one_more = LayoutOp::SplitTile {
        tile: t(&tile_id),
        dir: SplitDir::H,
        new_tile: t("deepest"),
        ratio: 0.5,
        panel: None,
    };
    assert_eq!(
        apply(&current, &[one_more]).unwrap_err(),
        LayoutError::TooDeep
    );
}

#[test]
fn ops_exceeding_size_cap_rejected() {
    let mut big = default_layout();
    big.groups = json!(["x".repeat(LAYOUT_MAX_BYTES - 1024)]);
    validate_layout(&big).unwrap();
    let op = LayoutOp::SplitTile {
        tile: t("main"),
        dir: SplitDir::H,
        new_tile: t("n"),
        ratio: 0.5,
        panel: None,
    };
    assert!(apply(&big, std::slice::from_ref(&op)).is_ok());
    big.groups = json!(["x".repeat(LAYOUT_MAX_BYTES - 100)]);
    assert!(matches!(apply(&big, &[op]), Err(LayoutError::TooLarge(_))));
}

#[test]
fn origin_map_tracks_split_and_close() {
    let base = layout(split(
        0.5,
        tile("a", &[], None),
        split(0.5, tile("b", &[], None), tile("c", &[], None)),
    ));
    let origins = identity_origins(&base);
    assert_eq!(
        origins.get(&key(&["a", "b", "c"])),
        Some(&Some(key(&["a", "b", "c"])))
    );
    assert_eq!(
        origins.get(&key(&["b", "c"])),
        Some(&Some(key(&["b", "c"])))
    );

    let split_b = LayoutOp::SplitTile {
        tile: t("b"),
        dir: SplitDir::V,
        new_tile: t("n"),
        ratio: 0.5,
        panel: None,
    };
    let after_split = apply_ops(&base, &origins, &[split_b]).unwrap();
    let origins = after_split.origins;
    assert_eq!(origins.get(&key(&["b", "n"])), Some(&None));
    assert_eq!(
        origins.get(&key(&["b", "c", "n"])),
        Some(&Some(key(&["b", "c"])))
    );
    assert_eq!(
        origins.get(&key(&["a", "b", "c", "n"])),
        Some(&Some(key(&["a", "b", "c"])))
    );
    assert_eq!(origins.len(), 3);

    let set_new = LayoutOp::SetRatio {
        leaves: key(&["b", "n"]).0,
        ratio: 0.7,
    };
    let touched_new = apply_ops(&after_split.layout, &origins, &[set_new]).unwrap();
    assert!(touched_new.touched.is_empty());
    let set_ancestor = LayoutOp::SetRatio {
        leaves: key(&["b", "c", "n"]).0,
        ratio: 0.6,
    };
    let touched_ancestor = apply_ops(&after_split.layout, &origins, &[set_ancestor]).unwrap();
    assert_eq!(
        touched_ancestor.touched,
        [FieldPath::Ratio(key(&["b", "c"]))].into_iter().collect()
    );

    let close_c = LayoutOp::CloseTile { tile: t("c") };
    let after_close = apply_ops(&after_split.layout, &origins, &[close_c]).unwrap();
    let origins = after_close.origins;
    assert_eq!(origins.get(&key(&["b", "n"])), Some(&None));
    assert_eq!(
        origins.get(&key(&["a", "b", "n"])),
        Some(&Some(key(&["a", "b", "c"])))
    );
    assert_eq!(origins.len(), 2);
}

#[test]
fn diff_fields_reports_ratio_active_panels_structure() {
    let disk = two_tiles();
    assert!(diff_fields(&disk, &disk).is_empty());

    let mut changed = two_tiles();
    let SplitNode::Split(root) = &mut changed.root else {
        panic!("split")
    };
    root.ratio = 0.7;
    if let SplitNode::Tile(a) = &mut *root.a {
        a.active = Some(p("p1"));
    }
    if let SplitNode::Tile(b) = &mut *root.b {
        b.panels.push(p("p5"));
    }
    let expected: BTreeSet<_> = [
        FieldPath::Ratio(key(&["a", "b"])),
        FieldPath::TileActive(t("a")),
        FieldPath::TilePanels(t("b")),
    ]
    .into_iter()
    .collect();
    assert_eq!(diff_fields(&disk, &changed), expected);

    let closed = apply(&disk, &[LayoutOp::CloseTile { tile: t("b") }])
        .unwrap()
        .layout;
    assert!(diff_fields(&disk, &closed).contains(&FieldPath::Structure));

    let mut grouped = two_tiles();
    grouped.groups = json!([1]);
    grouped.extra.insert("k".to_string(), json!(1));
    let fields = diff_fields(&disk, &grouped);
    assert!(fields.contains(&FieldPath::Groups) && fields.contains(&FieldPath::Extra));
}

fn add(panel: &str, tile: Option<&str>, activate: bool) -> LayoutOp {
    LayoutOp::AddPanel {
        panel: p(panel),
        tile: tile.map(t),
        activate,
    }
}

fn remove(panel: &str) -> LayoutOp {
    LayoutOp::RemovePanel { panel: p(panel) }
}

fn panels_of(layout: &SpaceLayout, id: &str) -> Vec<String> {
    tile_of(&layout.root, id)
        .unwrap()
        .panels
        .iter()
        .map(|panel| panel.as_str().to_string())
        .collect()
}

#[test]
fn add_panel_appends_to_the_hinted_tile_and_activates_it() {
    let applied = apply(&two_tiles(), &[add("p9", Some("b"), true)]).unwrap();
    let b = tile_of(&applied.layout.root, "b").unwrap();
    assert_eq!(panels_of(&applied.layout, "b"), ["p4", "p9"]);
    assert_eq!(b.active, Some(p("p9")));
    assert_eq!(panels_of(&applied.layout, "a"), ["p1", "p2", "p3"]);

    let quiet = apply(&two_tiles(), &[add("p9", Some("b"), false)]).unwrap();
    assert_eq!(panels_of(&quiet.layout, "b"), ["p4", "p9"]);
    assert_eq!(
        tile_of(&quiet.layout.root, "b").unwrap().active,
        Some(p("p4"))
    );

    let empty = layout(split(
        0.5,
        tile("a", &[], None),
        tile("b", &["p4"], Some("p4")),
    ));
    let applied = apply(&empty, &[add("p9", Some("a"), false)]).unwrap();
    assert_eq!(
        tile_of(&applied.layout.root, "a").unwrap().active,
        Some(p("p9"))
    );

    let held = apply(&two_tiles(), &[add("p1", Some("b"), true)]);
    assert!(matches!(held, Err(LayoutError::PanelInLayout(id)) if id == p("p1")));
    assert_eq!(
        LayoutError::PanelInLayout(p("p1")).to_string(),
        "panel p1 is already in the layout"
    );
}

#[test]
fn with_no_hint_the_first_tile_in_tree_order_gets_it() {
    // Tile `z` is first in tree order, tile `a` sorts first by ID.
    let tree = layout(split(
        0.5,
        split(0.5, tile("z", &["p1"], Some("p1")), tile("y", &[], None)),
        tile("a", &["p2"], Some("p2")),
    ));
    let applied = apply(&tree, &[add("p9", None, true)]).unwrap();
    assert_eq!(panels_of(&applied.layout, "z"), ["p1", "p9"]);
    assert_eq!(panels_of(&applied.layout, "a"), ["p2"]);
    assert_eq!(
        applied.touched,
        [FieldPath::TilePanels(t("z")), FieldPath::TileActive(t("z"))]
            .into_iter()
            .collect()
    );
    assert_eq!(combined::core::first_tile(&tree.root).id, t("z"));
}

#[test]
fn a_hint_naming_no_tile_falls_back_to_the_first_tile() {
    let applied = apply(&two_tiles(), &[add("p9", Some("nope"), true)]).unwrap();
    assert_eq!(panels_of(&applied.layout, "a"), ["p1", "p2", "p3", "p9"]);
    assert_eq!(
        combined::core::target_tile(&two_tiles().root, Some(&t("nope"))).id,
        t("a")
    );
    assert_eq!(
        combined::core::target_tile(&two_tiles().root, Some(&t("b"))).id,
        t("b")
    );
}

#[test]
fn add_and_remove_panel_never_change_the_tiles() {
    let tiles = |layout: &SpaceLayout| {
        let mut flat = Vec::new();
        fn walk(node: &SplitNode, out: &mut Vec<String>) {
            match node {
                SplitNode::Tile(tile) => out.push(tile.id.as_str().to_string()),
                SplitNode::Split(split) => {
                    walk(&split.a, out);
                    walk(&split.b, out);
                }
            }
        }
        walk(&layout.root, &mut flat);
        flat
    };
    let before = two_tiles();
    let added = apply(&before, &[add("p9", None, true)]).unwrap();
    assert_eq!(tiles(&added.layout), tiles(&before));
    assert!(!added.touched.contains(&FieldPath::Structure));
    let removed = apply(&before, &[remove("p4")]).unwrap();
    assert_eq!(tiles(&removed.layout), tiles(&before));

    // RemovePanel moves active to the panel now at its index, else the one before, else none.
    let middle = apply(&before, &[remove("p2")]).unwrap();
    assert_eq!(
        tile_of(&middle.layout.root, "a").unwrap().active,
        Some(p("p3"))
    );
    let end = apply(&before, &[set_active_op("a", "p3"), remove("p3")]).unwrap();
    assert_eq!(
        tile_of(&end.layout.root, "a").unwrap().active,
        Some(p("p2"))
    );
    assert_eq!(tile_of(&removed.layout.root, "b").unwrap().active, None);
    let inactive = apply(&before, &[remove("p1")]).unwrap();
    assert_eq!(
        tile_of(&inactive.layout.root, "a").unwrap().active,
        Some(p("p2"))
    );
    assert!(matches!(
        apply(&before, &[remove("nope")]),
        Err(LayoutError::UnknownPanel(_))
    ));
}

fn set_active_op(tile: &str, panel: &str) -> LayoutOp {
    LayoutOp::SetActive {
        tile: t(tile),
        panel: p(panel),
    }
}

#[test]
fn panel_ops_touch_only_the_target_tiles_fields() {
    let before = two_tiles();
    let only = |tile: &str| -> BTreeSet<FieldPath> {
        [
            FieldPath::TilePanels(t(tile)),
            FieldPath::TileActive(t(tile)),
        ]
        .into_iter()
        .collect()
    };
    assert_eq!(
        apply(&before, &[add("p9", Some("b"), false)])
            .unwrap()
            .touched,
        only("b")
    );
    assert_eq!(
        apply(&before, &[add("p9", None, true)]).unwrap().touched,
        only("a")
    );
    assert_eq!(apply(&before, &[remove("p4")]).unwrap().touched, only("b"));
    let applied = apply(&before, &[add("p9", Some("b"), true)]).unwrap();
    let changed = diff_fields(&before, &applied.layout);
    assert_eq!(changed, only("b"));
}

#[test]
fn reserved_terminal_tab_ids_are_ordinary_panel_ids() {
    let pinned = ".terminal/instance/7";
    let applied = apply(
        &two_tiles(),
        &[
            add(".active-terminal", Some("a"), false),
            add(pinned, Some("b"), true),
        ],
    )
    .unwrap();
    assert_eq!(
        panels_of(&applied.layout, "a"),
        ["p1", "p2", "p3", ".active-terminal"]
    );
    assert_eq!(panels_of(&applied.layout, "b"), ["p4", pinned]);
    assert_eq!(
        tile_of(&applied.layout.root, "b").unwrap().active,
        Some(p(pinned))
    );

    let active = LayoutOp::SetActive {
        tile: t("b"),
        panel: p(".active-terminal"),
    };
    assert!(matches!(
        apply(&applied.layout, &[active]),
        Err(LayoutError::PanelNotInTile(..))
    ));
    let moved = LayoutOp::MovePanel {
        panel: p(pinned),
        to_tile: t("a"),
        index: 0,
    };
    let moved = apply(&applied.layout, &[moved]).unwrap().layout;
    assert_eq!(panels_of(&moved, "a")[0], pinned);
    let removed = apply(&moved, &[remove(pinned), remove(".active-terminal")])
        .unwrap()
        .layout;
    assert_eq!(panels_of(&removed, "a"), ["p1", "p2", "p3"]);

    let twice = apply(
        &two_tiles(),
        &[add(pinned, None, true), add(pinned, Some("b"), true)],
    );
    assert!(matches!(twice, Err(LayoutError::PanelInLayout(id)) if id == p(pinned)));
}

#[test]
fn split_then_add_into_the_new_tile_in_one_batch() {
    let pinned = ".terminal/instance/7";
    let split = LayoutOp::SplitTile {
        tile: t("a"),
        dir: SplitDir::H,
        new_tile: t("n"),
        ratio: 0.5,
        panel: None,
    };
    let applied = apply(&two_tiles(), &[split, add(pinned, Some("n"), true)]).unwrap();
    assert_eq!(panels_of(&applied.layout, "n"), [pinned]);
    assert_eq!(
        tile_of(&applied.layout.root, "n").unwrap().active,
        Some(p(pinned))
    );
    assert_eq!(panels_of(&applied.layout, "a"), ["p1", "p2", "p3"]);
    assert!(applied.touched.contains(&FieldPath::Structure));
    assert!(applied.touched.contains(&FieldPath::TilePanels(t("n"))));
}
