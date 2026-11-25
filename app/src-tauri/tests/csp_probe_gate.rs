//! The csp-probe feature must never reach the app users install. These textual checks hold the
//! layout that keeps it out: the feature is not a default, every line outside the probe module
//! that names it sits behind the feature, and the webview's sources never carry the probe's
//! environment variable names. `./tools/verify` also builds the default app and searches its
//! binary for those names.

use std::fs;
use std::path::{Path, PathBuf};

const GATE: &str = r#"#[cfg(feature = "csp-probe")]"#;
const ENV_NAMES: [&str; 3] = [
    "COMBINE_CSP_PROBE_OUT",
    "COMBINE_CSP_PROBE_DATA",
    "COMBINE_CSP_PROBE_PLANT",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn files_under(dir: &Path, extensions: &[&str], found: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            files_under(&path, extensions, found);
        } else if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.contains(&extension))
        {
            found.push(path);
        }
    }
}

fn features_section(manifest: &str) -> Vec<&str> {
    manifest
        .lines()
        .skip_while(|line| line.trim() != "[features]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

#[test]
fn the_probe_feature_is_not_a_default() {
    let manifest = fs::read_to_string(manifest_dir().join("Cargo.toml")).expect("Cargo.toml");
    let features = features_section(&manifest);
    assert_eq!(features, ["csp-probe = []"]);
}

#[test]
fn probe_code_is_gated_by_the_feature() {
    let src = manifest_dir().join("src");
    let probe_module = src.join("commands").join("csp_probe.rs");
    let mut files = Vec::new();
    files_under(&src, &["rs"], &mut files);
    assert!(files.contains(&probe_module), "the probe module moved");
    let mut mentions = 0;
    for file in files.into_iter().filter(|file| *file != probe_module) {
        let text = fs::read_to_string(&file).expect("source");
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            for name in ENV_NAMES {
                assert!(
                    !line.contains(name),
                    "{}:{} names {name}",
                    file.display(),
                    index + 1
                );
            }
            if !line.contains("csp_probe") {
                continue;
            }
            mentions += 1;
            let at = format!("{}:{}", file.display(), index + 1);
            assert!(
                index > 0 && lines[index - 1].trim() == GATE,
                "{at} names the probe without {GATE} on the line before"
            );
            let statement = line.trim();
            assert!(statement.ends_with(';'), "{at} is not a whole statement");
            assert!(!statement.starts_with("use "), "{at} imports the probe");
        }
    }
    assert!(mentions >= 4, "found only {mentions} gated mentions");
}

#[test]
fn web_sources_never_name_the_probe_env_markers() {
    let web = manifest_dir().join("../web/src");
    let mut files = Vec::new();
    files_under(
        &web,
        &["ts", "tsx", "js", "jsx", "css", "html", "json"],
        &mut files,
    );
    assert!(!files.is_empty(), "no web sources under {}", web.display());
    for file in files {
        let text = fs::read_to_string(&file).expect("web source");
        for name in ENV_NAMES {
            assert!(!text.contains(name), "{} names {name}", file.display());
        }
    }
}
