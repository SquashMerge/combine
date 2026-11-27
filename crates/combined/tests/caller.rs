#![cfg(feature = "test-support")]
//! Which space an MCP caller resolves to: its terminal when the backend's identity accepts it,
//! else the nearest listed space holding its working directory.

mod common;

use combine_protocol::McpCaller;
use combined::core::{resolve_caller, CallerMatch, ResolvedCaller, TerminalList};
use combined::model::{SpaceId, TermInfo, TermState};
use combined::testing::FakeCallerIdentity;
use common::{fake_backend_info, FakeGitProbe};

const SOCKET: &str = "/tmp/c/core.sock";

fn info(term: &str, cwd: &str) -> TermInfo {
    TermInfo {
        id: combine_protocol::TermId(format!("fake:{term}")),
        cwd: cwd.to_string(),
        argv: vec!["sh".to_string()],
        state: TermState::Running,
        labels: Default::default(),
        cols: 80,
        rows: 24,
    }
}

fn terminals() -> TerminalList {
    let mut list = TerminalList::new(Box::new(FakeGitProbe::default()), fake_backend_info());
    list.apply_list(&[
        info("t1", "/work/a"),
        info("t2", "/work/a/inner"),
        info("t3", "/other"),
    ]);
    list
}

fn caller(id: Option<&str>, socket: Option<&str>, cwd: Option<&str>) -> McpCaller {
    McpCaller {
        terminal_id: id.map(str::to_owned),
        terminal_socket: socket.map(str::to_owned),
        cwd: cwd.map(str::to_owned),
    }
}

fn resolved(space: &str, by: CallerMatch) -> Option<ResolvedCaller> {
    Some(ResolvedCaller {
        space: SpaceId(space.to_owned()),
        slot: None,
        by,
    })
}

#[test]
fn an_accepted_terminal_gives_its_space() {
    let identity = FakeCallerIdentity::new(Some(SOCKET));
    let caller = caller(Some("t3"), Some(SOCKET), Some("/work/a"));
    assert_eq!(
        resolve_caller(&caller, Some(&identity), &terminals()),
        resolved("/other", CallerMatch::Terminal)
    );
}

#[test]
fn a_socket_mismatch_falls_to_the_cwd() {
    let identity = FakeCallerIdentity::new(Some(SOCKET));
    let caller = caller(Some("t3"), Some("/tmp/other.sock"), Some("/work/a/sub"));
    assert_eq!(
        resolve_caller(&caller, Some(&identity), &terminals()),
        resolved("/work/a", CallerMatch::Cwd)
    );
    let no_cwd = self::caller(Some("t3"), Some("/tmp/other.sock"), None);
    assert_eq!(resolve_caller(&no_cwd, Some(&identity), &terminals()), None);
}

#[test]
fn the_nearest_ancestor_of_nested_spaces_wins() {
    let list = terminals();
    let deep = caller(None, None, Some("/work/a/inner/deeper/still"));
    assert_eq!(
        resolve_caller(&deep, None, &list),
        resolved("/work/a/inner", CallerMatch::Cwd)
    );
    let beside = caller(None, None, Some("/work/a/innermost"));
    assert_eq!(
        resolve_caller(&beside, None, &list),
        resolved("/work/a", CallerMatch::Cwd)
    );
}

#[test]
fn a_cwd_equal_to_a_space_resolves_to_it() {
    let caller = caller(None, None, Some("/work/a/inner"));
    assert_eq!(
        resolve_caller(&caller, None, &terminals()),
        resolved("/work/a/inner", CallerMatch::Cwd)
    );
}

#[test]
fn relative_and_dotdot_cwds_give_none() {
    for cwd in ["work/a", "", ".", "/work/a/../other", "/work/a/.."] {
        let caller = caller(None, None, Some(cwd));
        assert_eq!(resolve_caller(&caller, None, &terminals()), None, "{cwd}");
    }
}

#[test]
fn spaces_without_an_absolute_root_never_match_a_cwd() {
    let mut list = TerminalList::new(Box::new(FakeGitProbe::default()), fake_backend_info());
    list.apply_list(&[
        info("t1", "/work/a"),
        info("t2", ""),
        info("t3", "relative/dir"),
    ]);
    let outside = caller(None, None, Some("/elsewhere"));
    assert_eq!(resolve_caller(&outside, None, &list), None);
    let inside = caller(None, None, Some("/work/a/src"));
    assert_eq!(
        resolve_caller(&inside, None, &list),
        resolved("/work/a", CallerMatch::Cwd)
    );
}

#[test]
fn no_identity_resolves_by_the_cwd_only() {
    let caller = caller(Some("t3"), Some(SOCKET), Some("/work/a"));
    assert_eq!(
        resolve_caller(&caller, None, &terminals()),
        resolved("/work/a", CallerMatch::Cwd)
    );
    let nowhere = self::caller(Some("t3"), Some(SOCKET), Some("/elsewhere"));
    assert_eq!(resolve_caller(&nowhere, None, &terminals()), None);
}

#[test]
fn an_unknown_terminal_falls_to_the_cwd() {
    let identity = FakeCallerIdentity::new(Some(SOCKET));
    let caller = caller(Some("nope"), Some(SOCKET), Some("/work/a"));
    assert_eq!(
        resolve_caller(&caller, Some(&identity), &terminals()),
        resolved("/work/a", CallerMatch::Cwd)
    );
}

#[test]
fn empty_strings_count_as_absent() {
    let identity = FakeCallerIdentity::new(Some(""));
    let empty_id = caller(Some(""), Some(SOCKET), Some("/work/a"));
    assert_eq!(
        resolve_caller(&empty_id, Some(&identity), &terminals()).map(|resolved| resolved.by),
        Some(CallerMatch::Cwd)
    );
    let empty_socket = caller(Some("t3"), Some(""), None);
    assert_eq!(
        resolve_caller(&empty_socket, Some(&identity), &terminals()),
        None
    );
}

#[test]
fn the_slot_is_always_none() {
    let identity = FakeCallerIdentity::new(Some(SOCKET));
    let by_terminal = caller(Some("t1"), Some(SOCKET), None);
    let by_cwd = caller(None, None, Some("/work/a"));
    for caller in [by_terminal, by_cwd] {
        let resolved = resolve_caller(&caller, Some(&identity), &terminals()).unwrap();
        assert_eq!(resolved.slot, None);
    }
}
