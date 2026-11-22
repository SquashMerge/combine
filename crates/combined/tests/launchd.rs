#![cfg(feature = "test-support")]
//! The launchd agent plist, and that no source writes to a user's launchd directory.

mod common;
mod daemon_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const PLIST: &str = "launchd/com.squashmerge.combined.plist";

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn plist_path() -> PathBuf {
    manifest_dir().join(PLIST)
}

/// The text of the `<key>` entry's value element, up to its closing tag.
fn value_after<'a>(plist: &'a str, key: &str) -> &'a str {
    let marker = format!("<key>{key}</key>");
    let start = plist
        .find(&marker)
        .unwrap_or_else(|| panic!("no key {key}"))
        + marker.len();
    let rest = plist[start..].trim_start();
    let end = rest.find("</dict>").unwrap_or(rest.len());
    let next_key = rest.find("<key>").unwrap_or(rest.len());
    &rest[..end.min(next_key)]
}

#[test]
fn plist_runs_combined_serve_under_the_documented_label() {
    let plist = fs::read_to_string(plist_path()).unwrap();
    assert!(value_after(&plist, "Label").starts_with("<string>com.squashmerge.combined</string>"));
    let arguments = value_after(&plist, "ProgramArguments");
    let strings: Vec<&str> = arguments
        .split("<string>")
        .skip(1)
        .map(|part| part.split("</string>").next().unwrap())
        .collect();
    assert_eq!(strings, ["/opt/homebrew/bin/combined", "serve"]);
    assert!(value_after(&plist, "RunAtLoad").starts_with("<true/>"));
    assert!(value_after(&plist, "KeepAlive").starts_with("<true/>"));
    assert!(value_after(&plist, "ThrottleInterval").starts_with("<integer>10</integer>"));
    assert!(value_after(&plist, "ProcessType").starts_with("<string>Interactive</string>"));
    let log = "<string>__HOME__/Library/Logs/combined.log</string>";
    assert!(value_after(&plist, "StandardOutPath").starts_with(log));
    assert!(value_after(&plist, "StandardErrorPath").starts_with(log));
}

#[test]
fn plist_passes_plutil_lint() {
    let plutil = Path::new("/usr/bin/plutil");
    if !plutil.exists() {
        eprintln!("skipped: /usr/bin/plutil is not available");
        return;
    }
    let output = Command::new(plutil)
        .arg("-lint")
        .arg(plist_path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "plutil -lint failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_source_writes_launch_agents() {
    let root = manifest_dir().parent().unwrap().parent().unwrap();
    let mut sources = Vec::new();
    for member in fs::read_dir(root.join("crates")).unwrap() {
        rust_files(&member.unwrap().path().join("src"), &mut sources);
    }
    rust_files(&root.join("app/src-tauri/src"), &mut sources);
    assert!(
        sources
            .iter()
            .any(|path| path.ends_with("combined/src/main.rs")),
        "the scan found no daemon sources: {sources:?}"
    );
    assert!(
        sources
            .iter()
            .any(|path| path.starts_with(root.join("app/src-tauri/src"))),
        "the scan found no app/src-tauri sources: {sources:?}"
    );
    let offenders: Vec<&PathBuf> = sources
        .iter()
        .filter(|path| fs::read_to_string(path).unwrap().contains("LaunchAgents"))
        .collect();
    assert!(
        offenders.is_empty(),
        "sources mention LaunchAgents: {offenders:?}"
    );
}
