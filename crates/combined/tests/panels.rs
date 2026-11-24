#![cfg(feature = "test-support")]
//! Terminal panels: parsing `panel.json`, cwd normalization and spawn specs.

use std::collections::BTreeMap;

use combined::core::{
    normalize_cwd, parse_terminal_panel, shell_spec, template_info, template_spec,
    valid_panel_folder, CwdRefusal, PanelRead, PanelSkip, TerminalTemplate, DEFAULT_COLS,
    DEFAULT_ROWS, REASON_MAX_BYTES,
};
use combined::model::{ContentHash, SpaceId};
use combined::ports::{DirRefusal, StoreError, StoreRevision, StoredFile};
use serde_json::{json, Value};

fn panel(content: Value) -> Value {
    json!({ "version": 1, "id": "p", "content": content })
}

fn terminal(template: Value) -> Value {
    panel(json!({ "type": "terminal", "slot": "main", "template": template }))
}

fn parse(value: &Value) -> Result<TerminalTemplate, PanelSkip> {
    parse_terminal_panel("p", value.to_string().as_bytes())
}

fn field(path: &'static str) -> impl Fn(&PanelSkip) -> bool {
    move |skip| matches!(skip, PanelSkip::Field { path: p, .. } if *p == path)
}

#[test]
fn valid_template_parses_with_defaults_and_explicit_size() {
    let t = parse(&terminal(json!({ "argv": ["claude", "--x"] }))).unwrap();
    assert_eq!(t.panel, "p");
    assert_eq!(t.title, None);
    assert_eq!(t.slot, "main");
    assert_eq!(t.argv, ["claude", "--x"]);
    assert_eq!(t.cwd, "");
    assert!(t.env.is_empty() && t.labels.is_empty());
    assert_eq!((t.cols, t.rows), (DEFAULT_COLS, DEFAULT_ROWS));

    let mut value = terminal(json!({
        "argv": ["sh"], "cols": 120, "rows": 40, "cwd": "src",
        "env": { "A": "1" }, "labels": { "k": "v" },
    }));
    value["title"] = json!("Agent");
    value["lifetime"] = json!("whatever");
    let t = parse(&value).unwrap();
    assert_eq!(t.title.as_deref(), Some("Agent"));
    assert_eq!((t.cols, t.rows), (120, 40));
    assert_eq!(t.cwd, "src");
    assert_eq!(t.env["A"], "1");
    assert_eq!(t.labels["k"], "v");
}

#[test]
fn missing_or_wrong_version_skipped() {
    let mut value = terminal(json!({ "argv": ["sh"] }));
    value.as_object_mut().unwrap().remove("version");
    assert_eq!(parse(&value), Err(PanelSkip::NoVersion));
    for version in [json!(2), json!("1"), json!(1.5)] {
        value["version"] = version;
        let skip = parse(&value).unwrap_err();
        assert!(matches!(skip, PanelSkip::Version(_)), "{skip:?}");
    }
    value["version"] = json!(2);
    assert_eq!(
        parse(&value).unwrap_err().to_string(),
        "unsupported version 2; this daemon reads version 1"
    );
}

#[test]
fn bad_json_and_non_object_skipped() {
    let skip = parse_terminal_panel("p", b"{nope").unwrap_err();
    assert!(matches!(skip, PanelSkip::BadJson(_)), "{skip:?}");
    assert!(skip
        .to_string()
        .starts_with("panel.json isn't valid JSON: "));
    assert_eq!(parse_terminal_panel("p", b"[1]"), Err(PanelSkip::NotObject));
    assert_eq!(parse_terminal_panel("p", b"3"), Err(PanelSkip::NotObject));
}

#[test]
fn view_panel_is_not_a_terminal_panel() {
    let view = panel(json!({ "type": "view", "url": "x" }));
    assert_eq!(parse(&view), Err(PanelSkip::NotTerminal));
    assert_eq!(parse(&panel(json!({}))), Err(PanelSkip::NotTerminal));
    assert!(field("content")(&parse(&panel(json!("x"))).unwrap_err()));
    assert!(field("content")(
        &parse(&json!({ "version": 1 })).unwrap_err()
    ));
}

#[test]
fn a_view_panel_json_is_not_a_terminal_panel() {
    let view = json!({ "version": 1, "renderer": "mermaid", "title": "Flow" });
    assert_eq!(parse(&view), Err(PanelSkip::NotTerminal));
    assert_eq!(PanelSkip::NotTerminal.to_string(), "not a terminal panel");
    assert_eq!(PanelSkip::NotView.to_string(), "not a view panel");
    let with_content = json!({ "version": 1, "renderer": "markdown", "content": {} });
    assert_eq!(parse(&with_content), Err(PanelSkip::NotTerminal));
}

#[test]
fn id_mismatch_skipped() {
    let mut value = terminal(json!({ "argv": ["sh"] }));
    value["id"] = json!("other");
    assert_eq!(parse(&value), Err(PanelSkip::IdMismatch));
    value["id"] = json!(7);
    assert_eq!(parse(&value), Err(PanelSkip::IdMismatch));
}

#[test]
fn bad_slot_argv_env_labels_size_skipped() {
    let slot = |slot: Value| {
        parse(&panel(
            json!({ "type": "terminal", "slot": slot, "template": { "argv": ["sh"] } }),
        ))
    };
    assert!(field("content.slot")(&slot(json!("")).unwrap_err()));
    assert!(field("content.slot")(&slot(json!("a\nb")).unwrap_err()));
    assert!(field("content.slot")(&slot(json!(3)).unwrap_err()));
    assert!(field("content.slot")(
        &slot(json!("x".repeat(257))).unwrap_err()
    ));

    let content = panel(json!({ "type": "terminal", "slot": "main", "template": "x" }));
    assert!(field("content.template")(&parse(&content).unwrap_err()));

    for argv in [
        json!([]),
        json!([""]),
        json!(["sh", 3]),
        json!(["sh\0"]),
        json!("sh"),
        json!(null),
    ] {
        let skip = parse(&terminal(json!({ "argv": argv }))).unwrap_err();
        assert!(field("content.template.argv")(&skip), "{argv}: {skip:?}");
    }
    assert!(field("content.template.argv")(
        &parse(&terminal(json!({}))).unwrap_err()
    ));
    assert!(parse(&terminal(json!({ "argv": ["sh", ""] }))).is_ok());

    let bad = |key: &str, value: Value| {
        let mut template = json!({ "argv": ["sh"] });
        template[key] = value;
        parse(&terminal(template)).unwrap_err()
    };
    for env in [
        json!([]),
        json!({ "": "x" }),
        json!({ "A=B": "x" }),
        json!({ "A": 1 }),
        json!({ "A": "x\0" }),
    ] {
        assert!(field("content.template.env")(&bad("env", env)));
    }
    assert!(field("content.template.labels")(&bad(
        "labels",
        json!({ "a": 1 })
    )));
    assert!(field("content.template.labels")(&bad("labels", json!("x"))));
    assert!(field("content.template.cwd")(&bad("cwd", json!(3))));
    assert!(field("title")(&{
        let mut value = terminal(json!({ "argv": ["sh"] }));
        value["title"] = json!(1);
        parse(&value).unwrap_err()
    }));
    for (key, value) in [
        ("cols", json!(0)),
        ("cols", json!(65536)),
        ("rows", json!(-1)),
        ("rows", json!("9")),
        ("cols", json!(1.5)),
    ] {
        let path = if key == "cols" {
            "content.template.cols"
        } else {
            "content.template.rows"
        };
        assert!(field(path)(&bad(key, value)));
    }
    let max = parse(&terminal(
        json!({ "argv": ["sh"], "cols": 65535, "rows": 1 }),
    ))
    .unwrap();
    assert_eq!((max.cols, max.rows), (65535, 1));
}

#[test]
fn cwd_normalization() {
    assert_eq!(normalize_cwd(""), Ok(String::new()));
    assert_eq!(normalize_cwd("."), Ok(String::new()));
    assert_eq!(normalize_cwd("./src/"), Ok("src".to_owned()));
    assert_eq!(normalize_cwd("a//b/./c"), Ok("a/b/c".to_owned()));
    assert_eq!(normalize_cwd("a.git/x"), Ok("a.git/x".to_owned()));
    let t = parse(&terminal(json!({ "argv": ["sh"], "cwd": "./src/" }))).unwrap();
    assert_eq!(t.cwd, "src");
}

#[test]
fn cwd_absolute_dotdot_git_any_case_refused() {
    assert_eq!(normalize_cwd("/etc"), Err(CwdRefusal::Absolute));
    assert_eq!(normalize_cwd("/"), Err(CwdRefusal::Absolute));
    assert_eq!(normalize_cwd(".."), Err(CwdRefusal::ParentSegment));
    assert_eq!(normalize_cwd("a/../b"), Err(CwdRefusal::ParentSegment));
    assert_eq!(normalize_cwd(".git"), Err(CwdRefusal::GitSegment));
    assert_eq!(normalize_cwd("a/.GIT/hooks"), Err(CwdRefusal::GitSegment));
    assert_eq!(normalize_cwd("a/.Git"), Err(CwdRefusal::GitSegment));
    assert_eq!(normalize_cwd("a\0b"), Err(CwdRefusal::Nul));
    assert_eq!(normalize_cwd("/a\0"), Err(CwdRefusal::Nul));

    let skip = parse(&terminal(json!({ "argv": ["sh"], "cwd": "/etc" }))).unwrap_err();
    assert_eq!(
        skip,
        PanelSkip::Cwd {
            cwd: "/etc".to_owned(),
            refusal: CwdRefusal::Absolute
        }
    );
    assert_eq!(
        skip.to_string(),
        "cwd \"/etc\" is absolute; it must be relative to the project root"
    );

    let why = |refusal| CwdRefusal::Dir(refusal).to_string();
    assert_eq!(why(DirRefusal::Missing), "doesn't exist");
    assert_eq!(why(DirRefusal::NotADirectory), "isn't a directory");
    assert_eq!(why(DirRefusal::Outside), "resolves outside the project");
    assert_eq!(why(DirRefusal::InsideGit), "resolves inside .git/");
    assert_eq!(
        why(DirRefusal::Unavailable("EIO".into())),
        "couldn't be checked (EIO)"
    );
    assert_eq!(CwdRefusal::ParentSegment.to_string(), "has a .. segment");
    assert_eq!(CwdRefusal::GitSegment.to_string(), "is inside .git/");
    assert_eq!(CwdRefusal::Nul.to_string(), "contains a NUL byte");
}

#[test]
fn panel_folder_and_reason_rules() {
    assert!(valid_panel_folder("claude"));
    assert!(valid_panel_folder("a.b"));
    for bad in ["", ".", "..", ".hidden", "a/b", "a\nb"] {
        assert!(!valid_panel_folder(bad), "{bad:?}");
    }
    assert!(!valid_panel_folder(&"x".repeat(257)));

    assert_eq!(PanelSkip::NoFile.to_string(), "no panel.json");
    assert_eq!(
        PanelSkip::TooLarge { size: 9000 }.to_string(),
        "panel.json is 9000 bytes; the limit is 8192"
    );
    let long = PanelSkip::BadJson("é".repeat(400));
    let reason = long.reason();
    assert!(reason.len() <= REASON_MAX_BYTES && reason.len() > REASON_MAX_BYTES - 2);
    assert!(long.to_string().starts_with(&reason));
}

#[test]
fn template_and_shell_specs() {
    let root = SpaceId("/work/proj".to_owned());
    let t = parse(&terminal(json!({
        "argv": ["claude", "--resume"], "cwd": "./sub/dir/", "cols": 100, "rows": 30,
        "env": { "A": "1", "B": "two" }, "labels": { "role": "agent" },
    })))
    .unwrap();
    let spec = template_spec(&root, &t);
    assert_eq!(spec.argv, ["claude", "--resume"]);
    assert_eq!(spec.cwd, "/work/proj/sub/dir");
    assert_eq!(spec.env, t.env);
    assert_eq!(spec.labels, t.labels);
    assert_eq!((spec.cols, spec.rows), (100, 30));

    let at_root = parse(&terminal(json!({ "argv": ["sh"] }))).unwrap();
    assert_eq!(template_spec(&root, &at_root).cwd, "/work/proj");

    let shell = shell_spec(&root);
    assert!(shell.argv.is_empty() && shell.env.is_empty() && shell.labels.is_empty());
    assert_eq!(shell.cwd, "/work/proj");
    assert_eq!((shell.cols, shell.rows), (80, 24));
    assert_eq!(
        shell_spec(&SpaceId("/work/proj/".into())).cwd,
        "/work/proj/"
    );

    let hash = ContentHash([0xab; 32]);
    let info = template_info(&t, &hash);
    assert_eq!(info.hash, "ab".repeat(32));
    assert_eq!(info.panel, "p");
    assert_eq!(info.slot, "main");
    assert_eq!(info.argv, t.argv);
    assert_eq!(info.env, t.env);
    assert_eq!(info.cwd, "sub/dir");
    assert_eq!(info.title, None);
    let _: BTreeMap<String, String> = info.env;
}

fn stored(read_only: bool) -> StoredFile {
    StoredFile {
        bytes: b"{}".to_vec(),
        revision: StoreRevision(3),
        hash: ContentHash([7; 32]),
        read_only,
    }
}

#[test]
fn panel_read_keeps_the_fallback_flag() {
    let project = PanelRead::from(Ok(Some(stored(false))));
    assert_eq!(
        project,
        PanelRead::Found {
            bytes: b"{}".to_vec(),
            hash: ContentHash([7; 32])
        }
    );
    assert!(!project.is_fallback());

    let fallback = PanelRead::from(Ok(Some(stored(true))));
    assert_eq!(
        fallback,
        PanelRead::FallbackFound {
            bytes: b"{}".to_vec(),
            hash: ContentHash([7; 32])
        }
    );
    assert!(fallback.is_fallback());

    let large = |read_only| PanelRead::from(Err(StoreError::TooLarge { size: 9, read_only }));
    assert_eq!(large(false), PanelRead::TooLarge { size: 9 });
    assert!(!large(false).is_fallback());
    assert_eq!(large(true), PanelRead::FallbackTooLarge { size: 9 });
    assert!(large(true).is_fallback());

    assert_eq!(PanelRead::from(Ok(None)), PanelRead::Missing);
    assert!(!PanelRead::Missing.is_fallback());
    let failed = PanelRead::from(Err(StoreError::Unavailable("x".into())));
    match &failed {
        PanelRead::Failed(message) => assert!(message.contains('x'), "{message}"),
        PanelRead::Missing
        | PanelRead::Found { .. }
        | PanelRead::TooLarge { .. }
        | PanelRead::FallbackFound { .. }
        | PanelRead::FallbackTooLarge { .. } => panic!("expected Failed, got {failed:?}"),
    }
    assert!(!failed.is_fallback());
}
