#![cfg(feature = "test-support")]
//! `FsProjectDirs` on a real temporary folder.

mod daemon_support;

use std::fs;
use std::os::unix::fs::symlink;

use combined::adapters::fs::project_dirs::FsProjectDirs;
use combined::model::SpaceId;
use combined::ports::{DirRefusal, ProjectDirs};
use daemon_support::ShortTempDir;

fn root_of(scratch: &ShortTempDir) -> SpaceId {
    SpaceId(scratch.path().join("project").to_str().unwrap().to_string())
}

fn project(scratch: &ShortTempDir) -> std::path::PathBuf {
    let project = scratch.path().join("project");
    fs::create_dir_all(project.join("sub/deeper")).unwrap();
    project
}

#[test]
fn root_and_subdir_ok() {
    let scratch = ShortTempDir::new();
    project(&scratch);
    let root = root_of(&scratch);
    assert_eq!(FsProjectDirs.check_dir(&root, ""), Ok(()));
    assert_eq!(FsProjectDirs.check_dir(&root, "sub"), Ok(()));
    assert_eq!(FsProjectDirs.check_dir(&root, "sub/deeper"), Ok(()));
}

#[test]
fn missing_and_file_refused() {
    let scratch = ShortTempDir::new();
    let project = project(&scratch);
    fs::write(project.join("file"), b"x").unwrap();
    let root = root_of(&scratch);
    assert_eq!(
        FsProjectDirs.check_dir(&root, "nope"),
        Err(DirRefusal::Missing)
    );
    assert_eq!(
        FsProjectDirs.check_dir(&root, "file"),
        Err(DirRefusal::NotADirectory)
    );
    assert_eq!(
        FsProjectDirs.check_dir(&root, "file/inner"),
        Err(DirRefusal::NotADirectory)
    );
    let gone = SpaceId(scratch.path().join("gone").to_str().unwrap().to_string());
    assert_eq!(FsProjectDirs.check_dir(&gone, ""), Err(DirRefusal::Missing));
    assert!(matches!(
        FsProjectDirs.check_dir(&SpaceId("relative".to_string()), ""),
        Err(DirRefusal::Unavailable(_))
    ));
    assert!(matches!(
        FsProjectDirs.check_dir(&root, "../x"),
        Err(DirRefusal::Unavailable(_))
    ));
}

#[test]
fn symlink_inside_ok() {
    let scratch = ShortTempDir::new();
    let project = project(&scratch);
    symlink(project.join("sub/deeper"), project.join("link")).unwrap();
    assert_eq!(FsProjectDirs.check_dir(&root_of(&scratch), "link"), Ok(()));
}

#[test]
fn symlink_outside_is_outside() {
    let scratch = ShortTempDir::new();
    let project = project(&scratch);
    fs::create_dir(scratch.path().join("elsewhere")).unwrap();
    symlink(scratch.path().join("elsewhere"), project.join("out")).unwrap();
    assert_eq!(
        FsProjectDirs.check_dir(&root_of(&scratch), "out"),
        Err(DirRefusal::Outside)
    );
}

#[test]
fn symlink_into_git_is_inside_git() {
    let scratch = ShortTempDir::new();
    let project = project(&scratch);
    fs::create_dir_all(project.join(".git/hooks")).unwrap();
    symlink(project.join(".git/hooks"), project.join("hooks")).unwrap();
    let root = root_of(&scratch);
    assert_eq!(
        FsProjectDirs.check_dir(&root, "hooks"),
        Err(DirRefusal::InsideGit)
    );
    assert_eq!(
        FsProjectDirs.check_dir(&root, ".git"),
        Err(DirRefusal::InsideGit)
    );
}

#[test]
fn root_behind_symlink_uses_canonical_root() {
    let scratch = ShortTempDir::new();
    let project = project(&scratch);
    let alias = scratch.path().join("alias");
    symlink(&project, &alias).unwrap();
    let root = SpaceId(alias.to_str().unwrap().to_string());
    assert_eq!(FsProjectDirs.check_dir(&root, ""), Ok(()));
    assert_eq!(FsProjectDirs.check_dir(&root, "sub"), Ok(()));
}

#[test]
fn git_detection_is_case_insensitive_and_exact() {
    let scratch = ShortTempDir::new();
    let project = project(&scratch);
    fs::create_dir_all(project.join(".GIT/hooks")).unwrap();
    fs::create_dir_all(project.join(".github/workflows")).unwrap();
    let root = root_of(&scratch);
    assert_eq!(
        FsProjectDirs.check_dir(&root, ".GIT"),
        Err(DirRefusal::InsideGit)
    );
    assert_eq!(
        FsProjectDirs.check_dir(&root, ".GIT/hooks"),
        Err(DirRefusal::InsideGit)
    );
    assert_eq!(FsProjectDirs.check_dir(&root, ".github"), Ok(()));
    assert_eq!(FsProjectDirs.check_dir(&root, ".github/workflows"), Ok(()));
}
