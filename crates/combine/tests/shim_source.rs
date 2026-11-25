//! The shim only connects: it starts no process and listens on nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const BANNED: &[&str] = &[
    "Command",
    "spawn_process",
    "posix_spawn",
    "fork",
    "execvp",
    "TcpListener",
    "TcpStream",
    "UdpSocket",
    "UnixListener",
    "UnixDatagram",
    "listen",
    "bind",
    "ssh",
];

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, found);
        } else {
            found.push(path);
        }
    }
}

fn tokens(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in text.lines().filter(|line| !line.trim().starts_with("//")) {
        for token in line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if !token.is_empty() {
                found.insert(token.to_owned());
            }
        }
    }
    found
}

#[test]
fn the_shim_spawns_nothing_and_listens_on_nothing() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    assert!(!files.is_empty(), "no files under {}", src.display());
    let mut all = BTreeSet::new();
    for file in &files {
        let found = tokens(&std::fs::read_to_string(file).unwrap());
        for banned in BANNED {
            assert!(
                !found.contains(*banned),
                "{} uses `{banned}`: the shim only connects",
                file.display()
            );
        }
        all.extend(found);
    }
    assert!(all.contains("UnixStream"), "the shim never uses UnixStream");
}
