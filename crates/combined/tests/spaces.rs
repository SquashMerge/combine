//! Spaces: which folder, worktree or repository each terminal belongs to.

mod common;

use combine_protocol::TermId;
use combined::core::Spaces;
use combined::model::{RootKind, Space, SpaceId, TermEvent, TermInfo, TermState};
use common::FakeGitProbe;

fn info(id: &str, cwd: &str) -> TermInfo {
    TermInfo {
        id: TermId(id.to_string()),
        cwd: cwd.to_string(),
        argv: vec!["sh".to_string()],
        state: TermState::Running,
        labels: Default::default(),
        cols: 80,
        rows: 24,
    }
}

fn id(text: &str) -> TermId {
    TermId(text.to_string())
}

fn spaces(probe: FakeGitProbe) -> Spaces {
    Spaces::new(Box::new(probe))
}

fn space(path: &str, kind: RootKind, terminals: &[&str]) -> Space {
    Space {
        id: SpaceId(path.to_string()),
        kind,
        terminals: terminals.iter().map(|t| id(t)).collect(),
    }
}

#[test]
fn terminal_joins_its_git_root() {
    let mut spaces = spaces(FakeGitProbe::default().with_repository("/work/repo"));
    spaces.apply_list(&[
        info("fake:1", "/work/repo/src/deep"),
        info("fake:2", "/work/repo"),
    ]);
    assert_eq!(
        spaces.spaces(),
        [space(
            "/work/repo",
            RootKind::Repository,
            &["fake:1", "fake:2"]
        )]
    );
    assert_eq!(
        spaces.space_of(&id("fake:1")),
        Some(&SpaceId("/work/repo".to_string()))
    );
    assert_eq!(spaces.space_of(&id("fake:9")), None);
}

#[test]
fn worktree_is_its_own_space() {
    let mut spaces = spaces(FakeGitProbe::default().with_worktree("/work/tree"));
    spaces.apply_event(&TermEvent::Created(info("fake:1", "/work/tree/sub")));
    assert_eq!(
        spaces.spaces(),
        [space("/work/tree", RootKind::Worktree, &["fake:1"])]
    );
}

#[test]
fn worktree_inside_a_repository_is_its_own_space() {
    let mut spaces = spaces(
        FakeGitProbe::default()
            .with_repository("/work/repo")
            .with_worktree("/work/repo/wt"),
    );
    spaces.apply_list(&[
        info("fake:1", "/work/repo/wt/a"),
        info("fake:2", "/work/repo/b"),
    ]);
    assert_eq!(
        spaces.spaces(),
        [
            space("/work/repo", RootKind::Repository, &["fake:2"]),
            space("/work/repo/wt", RootKind::Worktree, &["fake:1"]),
        ]
    );
}

#[test]
fn folder_outside_git_is_its_own_space() {
    let mut spaces = spaces(FakeGitProbe::default().with_repository("/other"));
    spaces.apply_list(&[
        info("fake:1", "/tmp/scratch/"),
        info("fake:2", "/tmp/./scratch"),
        info("fake:3", "/tmp/scratch/inner"),
    ]);
    assert_eq!(
        spaces.spaces(),
        [
            space("/tmp/scratch", RootKind::Folder, &["fake:1", "fake:2"]),
            space("/tmp/scratch/inner", RootKind::Folder, &["fake:3"]),
        ]
    );
}

#[test]
fn relative_cwd_is_its_own_space() {
    let mut spaces = spaces(FakeGitProbe::default().with_repository("work"));
    spaces.apply_list(&[info("fake:1", "work/x"), info("fake:2", "")]);
    assert_eq!(
        spaces.spaces(),
        [
            space("", RootKind::Folder, &["fake:2"]),
            space("work/x", RootKind::Folder, &["fake:1"]),
        ]
    );
}

#[test]
fn terminal_stays_when_its_program_changes_directory() {
    let mut spaces = spaces(FakeGitProbe::default().with_repository("/work/repo"));
    spaces.apply_list(&[info("fake:1", "/work/repo/a")]);
    spaces.apply_event(&TermEvent::Changed(info("fake:1", "/elsewhere")));
    spaces.apply_list(&[info("fake:1", "/elsewhere")]);
    spaces.apply_event(&TermEvent::Restarted { id: id("fake:1") });
    assert_eq!(
        spaces.spaces(),
        [space("/work/repo", RootKind::Repository, &["fake:1"])]
    );

    spaces.apply_event(&TermEvent::Changed(info("fake:2", "/elsewhere")));
    assert_eq!(spaces.spaces().len(), 2);
}

#[test]
fn exited_stays_and_lost_leaves() {
    let mut spaces = spaces(FakeGitProbe::default().with_repository("/work/repo"));
    spaces.apply_list(&[info("fake:1", "/work/repo"), info("fake:2", "/work/repo")]);
    spaces.apply_event(&TermEvent::Exited {
        id: id("fake:1"),
        exit_code: Some(1),
    });
    assert_eq!(
        spaces.spaces(),
        [space(
            "/work/repo",
            RootKind::Repository,
            &["fake:1", "fake:2"]
        )]
    );
    spaces.apply_event(&TermEvent::Lost { id: id("fake:1") });
    assert_eq!(
        spaces.spaces(),
        [space("/work/repo", RootKind::Repository, &["fake:2"])]
    );
    spaces.apply_event(&TermEvent::Lost { id: id("fake:2") });
    assert!(spaces.spaces().is_empty());
    assert_eq!(spaces.space_of(&id("fake:2")), None);

    spaces.apply_list(&[info("fake:3", "/a"), info("fake:4", "/b")]);
    spaces.apply_list(&[info("fake:4", "/b")]);
    assert_eq!(
        spaces.spaces(),
        [space("/b", RootKind::Folder, &["fake:4"])]
    );
}
