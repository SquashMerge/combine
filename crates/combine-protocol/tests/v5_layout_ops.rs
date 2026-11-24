//! Protocol version 5: the `add_panel` and `remove_panel` layout ops.

mod common;

use combine_protocol::*;
use common::*;

fn mutate(ops: &str) -> String {
    format!(
        r#"{{"type":"mutate_layout","request":15,"space":"/Users/me/src/trellis","base_layout_revision":4,"ops":[{ops}]}}"#
    )
}

fn ops_of(json: &str) -> Vec<LayoutOp> {
    match ClientMessage::decode(&control_frame(json)) {
        Ok((ClientMessage::MutateLayout(m), _)) => m.ops,
        other => panic!("{other:?}"),
    }
}

fn detail_of(json: &str) -> String {
    match ClientMessage::decode(&control_frame(json)) {
        Err(DecodeError::BadJson { detail }) => detail,
        other => panic!("{other:?}"),
    }
}

#[test]
fn add_panel_without_a_tile_reads_as_no_hint() {
    let expected = vec![LayoutOp::AddPanel {
        panel: s("p"),
        tile: None,
    }];
    assert_eq!(
        ops_of(&mutate(r#"{"op":"add_panel","panel":"p"}"#)),
        expected
    );
    assert_eq!(
        ops_of(&mutate(r#"{"op":"add_panel","panel":"p","tile":null}"#)),
        expected
    );
}

#[test]
fn remove_panel_ignores_a_tile() {
    let ops = ops_of(&mutate(
        r#"{"op":"remove_panel","panel":"p","tile":"t"},{"op":"remove_panel","panel":"q"}"#,
    ));
    assert_eq!(
        ops,
        vec![
            LayoutOp::RemovePanel { panel: s("p") },
            LayoutOp::RemovePanel { panel: s("q") },
        ]
    );
    let text = String::from_utf8(
        ClientMessage::MutateLayout(MutateLayout {
            request: RequestId(1),
            space: s("s"),
            base_layout_revision: 0,
            ops,
        })
        .encode()
        .unwrap()[5..]
            .to_vec(),
    )
    .unwrap();
    assert!(
        text.contains(r#"{"op":"remove_panel","panel":"p"}"#),
        "{text}"
    );
}

#[test]
fn panel_ops_without_a_panel_are_malformed() {
    for (op, name) in [("add_panel", "add_panel"), ("remove_panel", "remove_panel")] {
        for body in [
            format!(r#"{{"op":"{op}"}}"#),
            format!(r#"{{"op":"{op}","panel":null}}"#),
            format!(r#"{{"op":"{op}","tile":"t"}}"#),
        ] {
            let detail = detail_of(&mutate(&body));
            assert!(
                detail.contains(&format!("`{name}` is missing `panel`")),
                "{detail}"
            );
        }
    }
}

#[test]
fn panel_ops_with_mistyped_fields_are_malformed() {
    for body in [
        r#"{"op":"add_panel","panel":7}"#,
        r#"{"op":"add_panel","panel":"p","tile":7}"#,
        r#"{"op":"add_panel","panel":"p","tile":[]}"#,
        r#"{"op":"remove_panel","panel":7}"#,
        r#"{"op":"remove_panel","panel":{}}"#,
    ] {
        assert!(
            matches!(
                ClientMessage::decode(&control_frame(&mutate(body))),
                Err(DecodeError::BadJson { .. })
            ),
            "{body}"
        );
    }
}
