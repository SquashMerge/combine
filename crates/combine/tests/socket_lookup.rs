//! The shim finds the daemon's socket the way `combined connect` does. The shim keeps its own
//! lookup, so this pins the two to one answer.

use std::path::{Path, PathBuf};

use combine_cli::socket as shim;
use combined::adapters::fs::config as daemon_file;
use combined::testing::ShortTempDir;
use combined::wiring::config::{connect_socket, DaemonEnv};

struct Row {
    name: &'static str,
    flag: Option<&'static str>,
    /// The contents of `daemon.json`; `None` writes no file.
    file: Option<String>,
    home: Option<String>,
}

fn row(name: &'static str) -> Row {
    Row {
        name,
        flag: None,
        file: None,
        home: Some("/Users/a".to_string()),
    }
}

fn rows() -> Vec<Row> {
    let long_home = format!("/{}", "h".repeat(80));
    vec![
        Row {
            flag: Some("/f/flag.sock"),
            ..row("an absolute flag")
        },
        Row {
            flag: Some("relative.sock"),
            ..row("a relative flag, returned as is")
        },
        Row {
            file: Some(r#"{"version":1,"socket":"/s/x.sock"}"#.to_string()),
            ..row("socket")
        },
        Row {
            file: Some(r#"{"version":1,"data_dir":"/d"}"#.to_string()),
            ..row("data_dir")
        },
        Row {
            file: Some(r#"{"version":1}"#.to_string()),
            ..row("version only, with HOME")
        },
        row("no file, with HOME"),
        Row {
            file: Some(r#"{"version":1,"socket":"/s/x.sock","data_dir":"/d"}"#.to_string()),
            ..row("socket and data_dir: socket wins")
        },
        Row {
            file: Some(r#"{"version":1,"socket":"x.sock"}"#.to_string()),
            ..row("a relative socket")
        },
        Row {
            file: Some(r#"{"version":1,"data_dir":"d"}"#.to_string()),
            ..row("a relative data_dir")
        },
        Row {
            file: Some(format!(
                r#"{{"version":1,"socket":"/{}"}}"#,
                "a".repeat(103)
            )),
            ..row("a 104-byte socket")
        },
        Row {
            home: Some(long_home),
            ..row("a long HOME, which puts the default over 103 bytes")
        },
        Row {
            home: None,
            ..row("no file and no HOME")
        },
        Row {
            file: Some(
                r#"{"version":1,"backend":"fake","herdr_core_socket":"/h/hc.sock","data_dir":"/d","socket":"/s/x.sock"}"#
                    .to_string(),
            ),
            ..row("all five daemon fields")
        },
    ]
}

/// `combined connect`'s lookup.
fn daemon_side(row: &Row, config: &Path) -> Result<PathBuf, String> {
    let file = daemon_file::load(config, false).map_err(|e| e.to_string())?;
    let env = DaemonEnv {
        home: row.home.as_ref().map(PathBuf::from),
        hc_socket: None,
        hc_session: None,
        xdg_runtime_dir: None,
        xdg_state_home: None,
    };
    connect_socket(row.flag.map(PathBuf::from), file.as_ref(), &env).map_err(|e| e.to_string())
}

/// The shim's lookup, as `combine mcp` runs it: the config is not read under `--socket`.
fn shim_side(row: &Row, config: &Path) -> Result<PathBuf, String> {
    let loaded = if row.flag.is_some() {
        None
    } else {
        shim::load_config(config, false).map_err(|e| e.to_string())?
    };
    shim::daemon_socket(
        row.flag.map(PathBuf::from),
        loaded.as_ref(),
        row.home.as_deref().map(Path::new),
    )
    .map_err(|e| e.to_string())
}

#[test]
fn shim_and_daemon_find_the_same_socket() {
    assert_eq!(
        shim::MAX_SOCKET_PATH,
        combined::rpc::socket::MAX_SOCKET_PATH
    );
    let dir = ShortTempDir::new();
    let config = dir.path().join("daemon.json");
    for row in rows() {
        match &row.file {
            Some(contents) => std::fs::write(&config, contents).unwrap(),
            None => {
                let _ = std::fs::remove_file(&config);
            }
        }
        let (daemon, shim) = (daemon_side(&row, &config), shim_side(&row, &config));
        match (&daemon, &shim) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{}", row.name),
            (Err(_), Err(_)) => {}
            _ => panic!(
                "{}: the daemon found {daemon:?}, the shim {shim:?}",
                row.name
            ),
        }
    }
}

#[test]
fn every_row_but_the_refused_ones_finds_a_path() {
    // The table would pass if both sides failed everywhere: pin which rows are `Ok`.
    let dir = ShortTempDir::new();
    let config = dir.path().join("daemon.json");
    let mut found = Vec::new();
    for row in rows() {
        match &row.file {
            Some(contents) => std::fs::write(&config, contents).unwrap(),
            None => {
                let _ = std::fs::remove_file(&config);
            }
        }
        found.push((row.name, shim_side(&row, &config).ok()));
    }
    let expected = |name: &str| -> Option<&'static str> {
        match name {
            "an absolute flag" => Some("/f/flag.sock"),
            "a relative flag, returned as is" => Some("relative.sock"),
            "socket" | "socket and data_dir: socket wins" | "all five daemon fields" => {
                Some("/s/x.sock")
            }
            "data_dir" => Some("/d/combined.sock"),
            "version only, with HOME" | "no file, with HOME" => {
                Some("/Users/a/Library/Application Support/com.squashmerge.combined/combined.sock")
            }
            _ => None,
        }
    };
    for (name, path) in found {
        assert_eq!(path, expected(name).map(PathBuf::from), "{name}");
    }
}

#[test]
fn the_shim_ignores_fields_the_daemon_would_refuse() {
    let dir = ShortTempDir::new();
    let config = dir.path().join("daemon.json");
    for contents in [
        r#"{"version":1,"socket":"/s/x.sock","unknown":true}"#,
        r#"{"version":2,"socket":"/s/x.sock"}"#,
    ] {
        std::fs::write(&config, contents).unwrap();
        assert!(daemon_file::load(&config, false).is_err(), "{contents}");
        let loaded = shim::load_config(&config, false).unwrap();
        let found = shim::daemon_socket(None, loaded.as_ref(), None).unwrap();
        assert_eq!(found, PathBuf::from("/s/x.sock"), "{contents}");
    }
}
