//! Layout files: parsing, validation, round-tripping and encoding.

use combined::core::{
    default_layout, encode_layout, parse_layout, validate_layout, LayoutError, LAYOUT_MAX_BYTES,
};
use combined::model::{PanelId, SplitNode, TileId, ACTIVE_TERMINAL_TAB};
use serde_json::{json, Value};

fn load(value: &Value) -> Result<combined::model::SpaceLayout, LayoutError> {
    let layout = parse_layout(value)?;
    validate_layout(&layout)?;
    Ok(layout)
}

fn tile(id: &str, panels: &[&str], active: Option<&str>) -> Value {
    json!({ "type": "tile", "id": id, "panels": panels, "active": active })
}

fn file(root: Value) -> Value {
    json!({ "version": 1, "root": root, "groups": [] })
}

fn split(ratio: f64, a: Value, b: Value) -> Value {
    json!({ "type": "split", "dir": "h", "ratio": ratio, "a": a, "b": b })
}

#[test]
fn parses_minimal_layout() {
    let layout = load(&file(tile("main", &[], None))).unwrap();
    let SplitNode::Tile(root) = &layout.root else {
        panic!("root should be a tile");
    };
    assert_eq!(root.id, TileId::new("main").unwrap());
    assert!(root.panels.is_empty());
    assert_eq!(root.active, None);
    assert_eq!(layout.version, 1);
}

#[test]
fn unknown_fields_and_groups_round_trip() {
    let value = json!({
        "version": 1,
        "future": { "k": [1, 2] },
        "groups": [{ "name": "g", "panels": ["p1"] }],
        "root": {
            "type": "split", "dir": "v", "ratio": 0.25, "note": "n",
            "a": { "type": "tile", "id": "t1", "panels": ["p1"], "active": "p1", "color": "red" },
            "b": tile("t2", &[], None)
        }
    });
    let layout = load(&value).unwrap();
    assert_eq!(layout.extra.get("future"), Some(&json!({ "k": [1, 2] })));
    let bytes = encode_layout(&layout).unwrap();
    let again: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(again, value);
}

#[test]
fn missing_groups_defaults_to_empty_list_and_is_written() {
    let value = json!({ "version": 1, "root": tile("main", &[], None) });
    let layout = load(&value).unwrap();
    assert_eq!(layout.groups, json!([]));
    let again: Value = serde_json::from_slice(&encode_layout(&layout).unwrap()).unwrap();
    assert_eq!(again["groups"], json!([]));
}

#[test]
fn tile_and_panel_ids_stable_across_reload() {
    let value = file(split(
        0.5,
        tile("left", &["p2", "p1"], Some("p1")),
        tile("right", &["p3"], Some("p3")),
    ));
    let first = load(&value).unwrap();
    let second = load(&serde_json::from_slice(&encode_layout(&first).unwrap()).unwrap()).unwrap();
    assert_eq!(first, second);
    let SplitNode::Split(root) = &second.root else {
        panic!("root should be a split");
    };
    let SplitNode::Tile(left) = &*root.a else {
        panic!("a should be a tile");
    };
    assert_eq!(
        left.panels,
        vec![PanelId::new("p2").unwrap(), PanelId::new("p1").unwrap()]
    );
}

#[test]
fn dangling_panel_ids_preserved() {
    let value = file(tile("main", &["gone-1", "gone-2"], Some("gone-2")));
    let layout = load(&value).unwrap();
    let again: Value = serde_json::from_slice(&encode_layout(&layout).unwrap()).unwrap();
    assert_eq!(again["root"]["panels"], json!(["gone-1", "gone-2"]));
    assert_eq!(again["root"]["active"], json!("gone-2"));
}

#[test]
fn default_layout_holds_the_follow_active_tab() {
    let layout = default_layout();
    validate_layout(&layout).unwrap();
    let SplitNode::Tile(root) = &layout.root else {
        panic!("root should be a tile");
    };
    assert_eq!(root.id.as_str(), "main");
    assert_eq!(root.panels, vec![PanelId::active_terminal()]);
    assert_eq!(root.active, Some(PanelId::active_terminal()));
    assert_eq!(ACTIVE_TERMINAL_TAB, ".active-terminal");
    assert_eq!(PanelId::active_terminal().as_str(), ACTIVE_TERMINAL_TAB);
    assert_eq!(layout.groups, json!([]));
}

#[test]
fn encode_is_pretty_with_trailing_newline() {
    let bytes = encode_layout(&default_layout()).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.ends_with("}\n"));
    assert!(text.contains("\n  \"version\": 1"));
    assert!(!text.ends_with("\n\n"));
}

#[test]
fn rejects_ratio_out_of_range() {
    for ratio in [0.0, 1.0, -0.5, 1.5] {
        let value = file(split(ratio, tile("a", &[], None), tile("b", &[], None)));
        assert!(
            matches!(load(&value), Err(LayoutError::RatioOutOfRange(_))),
            "ratio {ratio}"
        );
    }
    let ok = file(split(0.5, tile("a", &[], None), tile("b", &[], None)));
    assert!(load(&ok).is_ok());
}

#[test]
fn rejects_missing_or_unsupported_version() {
    let missing = json!({ "root": tile("main", &[], None) });
    assert_eq!(load(&missing).unwrap_err(), LayoutError::MissingVersion);
    let future = json!({ "version": 2, "root": tile("main", &[], None) });
    assert_eq!(
        load(&future).unwrap_err(),
        LayoutError::UnsupportedVersion(2)
    );
    let mut layout = default_layout();
    layout.version = 0;
    assert_eq!(
        validate_layout(&layout).unwrap_err(),
        LayoutError::UnsupportedVersion(0)
    );
}

#[test]
fn rejects_duplicate_ids() {
    let tiles = file(split(0.5, tile("t", &[], None), tile("t", &[], None)));
    assert!(matches!(load(&tiles), Err(LayoutError::DuplicateTile(_))));
    let panels = file(split(
        0.5,
        tile("a", &["p"], Some("p")),
        tile("b", &["p"], Some("p")),
    ));
    assert!(matches!(load(&panels), Err(LayoutError::DuplicatePanel(_))));
}

#[test]
fn rejects_active_not_in_panels() {
    let value = file(tile("main", &["p1"], Some("p9")));
    assert!(matches!(
        load(&value),
        Err(LayoutError::ActiveNotInPanels(_))
    ));
    let none = file(tile("main", &["p1"], None));
    assert!(matches!(
        load(&none),
        Err(LayoutError::ActiveNotInPanels(_))
    ));
}

#[test]
fn empty_tile_requires_null_active() {
    let value = file(tile("main", &[], Some("p1")));
    assert!(matches!(
        load(&value),
        Err(LayoutError::EmptyTileHasActive(_))
    ));
}

#[test]
fn rejects_oversized_file() {
    let mut value = file(tile("main", &[], None));
    value["groups"] = json!(["x".repeat(LAYOUT_MAX_BYTES)]);
    assert!(matches!(load(&value), Err(LayoutError::TooLarge(_))));
}

#[test]
fn rejects_malformed_and_overdeep_layouts() {
    assert!(matches!(load(&json!([])), Err(LayoutError::Malformed(_))));
    assert!(matches!(
        load(&file(json!({ "type": "tile", "id": "", "panels": [] }))),
        Err(LayoutError::InvalidId(_))
    ));
    let mut deep = tile("leaf", &[], None);
    for level in 0..70 {
        deep = split(0.5, deep, tile(&format!("t{level}"), &[], None));
    }
    assert_eq!(load(&file(deep)).unwrap_err(), LayoutError::TooDeep);
}

#[test]
fn ids_enforce_their_rules() {
    assert!(TileId::new("").is_err());
    assert!(TileId::new("a\nb").is_err());
    assert!(TileId::new("x".repeat(257)).is_err());
    assert!(TileId::new("x".repeat(256)).is_ok());
    assert!(PanelId::new("").is_err());
    assert!(PanelId::new("panel-1").is_ok());
}
