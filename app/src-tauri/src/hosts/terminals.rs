//! The daemon's terminal list as the views the rest of the app shows.

use std::collections::HashMap;
use std::path::Path;

use combine_protocol::{HostId, TermId, TerminalInfo, TerminalState as WireState, Terminals};

use super::model::{SpaceKey, SpaceView, TerminalState, TerminalSummary};
use super::text::is_blank;

/// One view per space, in the daemon's order, including a space with no terminals.
pub fn space_views(host_id: &HostId, terminals: &Terminals) -> Vec<SpaceView> {
    let by_id: HashMap<&TermId, &TerminalInfo> =
        terminals.terminals.iter().map(|t| (&t.id, t)).collect();
    terminals
        .spaces
        .iter()
        .map(|space| SpaceView {
            key: SpaceKey {
                host_id: host_id.clone(),
                path: space.id.clone(),
            },
            terminals: space
                .terminals
                .iter()
                .filter_map(|id| by_id.get(id).map(|info| summary(info)))
                .collect(),
        })
        .collect()
}

fn summary(info: &TerminalInfo) -> TerminalSummary {
    TerminalSummary {
        id: info.id.clone(),
        name: name_of(info),
        command: info.argv.join(" "),
        state: match info.state {
            WireState::Running => TerminalState::Running,
            WireState::Exited => TerminalState::Exited,
        },
    }
}

/// The `name` label, else the file name of the program, else the terminal ID.
fn name_of(info: &TerminalInfo) -> String {
    if let Some(label) = info.labels.get("name").filter(|l| !is_blank(l)) {
        return label.clone();
    }
    info.argv
        .first()
        .and_then(|program| Path::new(program).file_name())
        .map(|file| file.to_string_lossy().into_owned())
        .filter(|file| !is_blank(file))
        .unwrap_or_else(|| info.id.0.clone())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use combine_protocol::{SpaceInfo, SpaceKind};

    use super::super::testing::{host_id, HOST_A};
    use super::*;

    fn info(id: &str, argv: &[&str], labels: &[(&str, &str)], state: WireState) -> TerminalInfo {
        TerminalInfo {
            id: TermId(id.to_string()),
            cwd: "/work".to_string(),
            argv: argv.iter().map(|a| a.to_string()).collect(),
            state,
            exit_code: None,
            labels: labels
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<BTreeMap<_, _>>(),
            cols: 80,
            rows: 24,
        }
    }

    fn space(id: &str, terminals: &[&str]) -> SpaceInfo {
        SpaceInfo {
            id: id.to_string(),
            kind: SpaceKind::Folder,
            terminals: terminals.iter().map(|t| TermId(t.to_string())).collect(),
        }
    }

    fn one(terminal: TerminalInfo) -> TerminalSummary {
        let id = terminal.id.0.clone();
        let list = Terminals {
            request: None,
            terminals: vec![terminal],
            spaces: vec![space("/work", &[&id])],
        };
        let mut views = space_views(&host_id(HOST_A), &list);
        views.remove(0).terminals.remove(0)
    }

    #[test]
    fn maps_spaces_and_terminals() {
        let list = Terminals {
            request: None,
            terminals: vec![
                info(
                    "t1",
                    &["/usr/bin/claude", "--resume"],
                    &[],
                    WireState::Running,
                ),
                info("t2", &["zsh"], &[], WireState::Exited),
            ],
            spaces: vec![space("/work/a", &["t1", "t2"])],
        };
        let views = space_views(&host_id(HOST_A), &list);
        assert_eq!(
            views,
            vec![SpaceView {
                key: SpaceKey {
                    host_id: host_id(HOST_A),
                    path: "/work/a".to_string()
                },
                terminals: vec![
                    TerminalSummary {
                        id: TermId("t1".to_string()),
                        name: "claude".to_string(),
                        command: "/usr/bin/claude --resume".to_string(),
                        state: TerminalState::Running,
                    },
                    TerminalSummary {
                        id: TermId("t2".to_string()),
                        name: "zsh".to_string(),
                        command: "zsh".to_string(),
                        state: TerminalState::Exited,
                    },
                ],
            }]
        );
    }

    #[test]
    fn keeps_space_without_terminals() {
        let list = Terminals {
            request: None,
            terminals: Vec::new(),
            spaces: vec![space("/empty", &[])],
        };
        let views = space_views(&host_id(HOST_A), &list);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].key.path, "/empty");
        assert!(views[0].terminals.is_empty());
    }

    #[test]
    fn name_from_label_then_argv_then_id() {
        let labeled = one(info(
            "t1",
            &["/bin/zsh"],
            &[("name", "build")],
            WireState::Running,
        ));
        assert_eq!(labeled.name, "build");
        let blank = one(info(
            "t1",
            &["/bin/zsh"],
            &[("name", "  ")],
            WireState::Running,
        ));
        assert_eq!(blank.name, "zsh");
        let bare = one(info("t9", &[], &[], WireState::Running));
        assert_eq!(bare.name, "t9");
    }

    #[test]
    fn empty_argv_gives_empty_command() {
        assert_eq!(one(info("t1", &[], &[], WireState::Running)).command, "");
    }
}
