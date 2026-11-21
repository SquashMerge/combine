use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn depends_on_no_other_combine_crate() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = fs::read_to_string(manifest).unwrap();
    let mut names = BTreeSet::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[') {
            section = header.trim_end_matches(']').trim().to_owned();
            if let Some(name) = dotted_dependency(&section) {
                names.insert(name.to_owned());
            }
            continue;
        }
        let is_dependency_table = section.ends_with("dependencies");
        let in_dotted_table = dotted_dependency(&section).is_some();
        if is_dependency_table {
            let (key, value) = line.split_once('=').expect("key = value");
            names.insert(key.trim().to_owned());
            assert!(!has_key(value, "path") && !has_key(value, "git"), "{line}");
        } else if in_dotted_table {
            let key = line.split('=').next().unwrap().trim();
            assert!(key != "path" && key != "git", "{section}: {line}");
        }
    }
    let expected: BTreeSet<String> = ["serde", "serde_json"].map(String::from).into();
    assert_eq!(names, expected);
}

/// `dependencies.x`, `dev-dependencies.x`, `target.<cfg>.dependencies.x`.
fn dotted_dependency(section: &str) -> Option<&str> {
    let (table, name) = section.rsplit_once('.')?;
    table.ends_with("dependencies").then_some(name.trim())
}

fn has_key(value: &str, key: &str) -> bool {
    let compact = value.replace(' ', "");
    compact.contains(&format!("{{{key}=")) || compact.contains(&format!(",{key}="))
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn public_sources_name_no_backend() {
    const FRAME_KIND_SPELLINGS: [&str; 7] = [
        "DATA",
        "CREDIT",
        "CLOSE",
        "RESIZE",
        "FOCUS",
        "FACTS",
        "AUTOMATIC_INPUT",
    ];
    let mut files = Vec::new();
    sources(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty());
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        assert!(!text.to_lowercase().contains("herdr"), "{}", file.display());
        for token in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
            assert!(
                !token.eq_ignore_ascii_case("hc"),
                "{} names the backend CLI",
                file.display()
            );
            assert!(
                !FRAME_KIND_SPELLINGS.contains(&token),
                "{} uses {token}",
                file.display()
            );
        }
    }
}
