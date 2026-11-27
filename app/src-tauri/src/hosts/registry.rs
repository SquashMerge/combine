//! The in-memory host model and the display-trim choke point.
//!
//! [`HostRegistry`] is the only way text reaches a snapshot. It trims external text only:
//! `detail` fields, `config.error` and terminal names and commands. `Dropped.reason` and
//! `Failed.message` are Combine text and are stored whole.

use combine_protocol::HostId;

use super::model::{
    BackendView, ConfigStatus, ConnectionState, DecimalU64, HostView, HostsSnapshot, SpaceView,
};
use super::text::display_text;

pub struct HostRegistry {
    revision: u64,
    config: ConfigStatus,
    hosts: Vec<HostView>,
}

impl HostRegistry {
    pub fn new(config: ConfigStatus, hosts: Vec<HostView>) -> Self {
        HostRegistry {
            revision: 0,
            config: ConfigStatus {
                path: config.path,
                error: config.error.map(|e| display_text(&e)),
            },
            hosts,
        }
    }

    fn host_mut(&mut self, name: &str) -> Option<&mut HostView> {
        self.hosts.iter_mut().find(|h| h.name == name)
    }

    pub fn host(&self, name: &str) -> Option<&HostView> {
        self.hosts.iter().find(|h| h.name == name)
    }

    pub fn set_connection(&mut self, name: &str, state: ConnectionState) {
        let state = state.for_display();
        if let Some(host) = self.host_mut(name) {
            host.connection = state;
            self.revision += 1;
        }
    }

    pub fn set_host_id(&mut self, name: &str, host_id: &HostId) {
        debug_assert!(
            !self
                .hosts
                .iter()
                .any(|h| h.name != name && h.host_id.as_ref() == Some(host_id)),
            "host ID {} is already held by another host",
            host_id.0
        );
        if let Some(host) = self.host_mut(name) {
            host.host_id = Some(host_id.clone());
            self.revision += 1;
        }
    }

    /// Replaces the spaces and the backend of the host with this ID. Spaces keyed to another
    /// host are dropped, and an unknown host ID is ignored.
    pub fn replace_spaces(
        &mut self,
        host_id: &HostId,
        spaces: Vec<SpaceView>,
        backend: BackendView,
    ) {
        let spaces: Vec<SpaceView> = spaces
            .into_iter()
            .filter(|space| space.key.host_id == *host_id)
            .map(|mut space| {
                for terminal in &mut space.terminals {
                    terminal.name = display_text(&terminal.name);
                    terminal.command = display_text(&terminal.command);
                }
                space
            })
            .collect();
        if let Some(host) = self
            .hosts
            .iter_mut()
            .find(|h| h.host_id.as_ref() == Some(host_id))
        {
            host.spaces = spaces;
            host.backend = Some(BackendView {
                id: display_text(&backend.id),
                instance: backend.instance,
            });
            self.revision += 1;
        }
    }

    pub fn snapshot(&self) -> HostsSnapshot {
        HostsSnapshot {
            revision: DecimalU64(self.revision),
            config: self.config.clone(),
            hosts: self.hosts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use combine_protocol::TermId;

    use super::super::config::StoreError;
    use super::super::failure::SshFailure;
    use super::super::identity::{duplicate_message, mismatch_message};
    use super::super::local::{herdr_core_socket_refusal, SocketGuardEnv};
    use super::super::model::{SpaceKey, TerminalState, TerminalSummary};
    use super::super::supervisor::store_failure_message;
    use super::super::testing::{host_id, HOST_A, HOST_B};
    use super::*;

    fn config(error: Option<&str>) -> ConfigStatus {
        ConfigStatus {
            path: "/data/hosts.json".to_string(),
            error: error.map(str::to_string),
        }
    }

    fn host(name: &str, id: Option<&str>) -> HostView {
        HostView {
            name: name.to_string(),
            host_id: id.map(host_id),
            connection: ConnectionState::Connecting,
            spaces: Vec::new(),
            backend: None,
        }
    }

    fn backend() -> BackendView {
        BackendView {
            id: "backend-a".to_string(),
            instance: Some("core-a".to_string()),
        }
    }

    fn space(id: &str, path: &str, terminals: Vec<TerminalSummary>) -> SpaceView {
        SpaceView {
            key: SpaceKey {
                host_id: host_id(id),
                path: path.to_string(),
            },
            terminals,
        }
    }

    fn terminal(id: &str, name: &str, command: &str) -> TerminalSummary {
        TerminalSummary {
            id: TermId(id.to_string()),
            name: name.to_string(),
            command: command.to_string(),
            state: TerminalState::Running,
        }
    }

    fn failed_message(registry: &mut HostRegistry, name: &str, message: &str) -> String {
        registry.set_connection(
            name,
            ConnectionState::Failed {
                message: message.to_string(),
                detail: None,
            },
        );
        match registry.host(name).map(|h| h.connection.clone()) {
            Some(ConnectionState::Failed { message, .. }) => message,
            other => panic!("expected failed, got {other:?}"),
        }
    }

    #[test]
    fn combine_messages_are_published_whole() {
        let name = "n".repeat(64);
        let other = "o".repeat(64);
        let dest = format!("u@{}", "h".repeat(253));
        let path = format!("/opt/{}/combined", "x".repeat(1000));
        assert_eq!((dest.len(), path.len()), (255, 1014));
        let (a, b) = (host_id(HOST_A), host_id(HOST_B));

        let mut texts: Vec<String> = [
            SshFailure::UnknownHostKey,
            SshFailure::ChangedHostKey,
            SshFailure::PassphraseNotInAgent,
            SshFailure::AuthenticationFailed,
            SshFailure::MissingRemoteBinary,
        ]
        .iter()
        .map(|f| f.message(&dest, &path))
        .collect();
        texts.push(mismatch_message(&name, &a, &b));
        texts.push(duplicate_message(&name, &other, &a));
        for e in [
            StoreError::Invalid,
            StoreError::ChangedOnDisk,
            StoreError::NoSuchEntry,
            StoreError::AlreadyRecorded(a.clone()),
            StoreError::DuplicateHostId {
                other: other.clone(),
            },
            StoreError::Io("disk full".to_string()),
        ] {
            texts.push(store_failure_message(&e));
        }
        let socket_103 = format!(
            "/tmp/herdr-core/{}",
            "s".repeat(103 - "/tmp/herdr-core/".len())
        );
        assert_eq!(socket_103.len(), 103);
        let refusal =
            herdr_core_socket_refusal(&PathBuf::from(&socket_103), &SocketGuardEnv::default())
                .expect("a herdr-core path is refused");
        texts.push(refusal);
        assert!(texts.iter().any(|t| t.chars().count() > 256));

        let mut registry = HostRegistry::new(config(None), vec![host(&name, None)]);
        for text in &texts {
            assert_eq!(&failed_message(&mut registry, &name, text), text);
        }

        let reason = format!("cannot connect to {socket_103}");
        registry.set_connection(
            &name,
            ConnectionState::Dropped {
                reason: reason.clone(),
                detail: None,
                retry_in_ms: 1000,
            },
        );
        assert_eq!(
            registry.host(&name).map(|h| h.connection.clone()),
            Some(ConnectionState::Dropped {
                reason,
                detail: None,
                retry_in_ms: 1000
            })
        );
    }

    #[test]
    fn set_connection_trims_only_detail() {
        let mut registry = HostRegistry::new(config(None), vec![host("mini", None)]);
        let long = "m".repeat(300);
        registry.set_connection(
            "mini",
            ConnectionState::Failed {
                message: long.clone(),
                detail: Some(format!("a\u{1b}b\u{7}{}", "x".repeat(1 << 20))),
            },
        );
        let expected_detail = format!("a b {}…", "x".repeat(252));
        assert_eq!(
            registry.snapshot().hosts[0].connection,
            ConnectionState::Failed {
                message: long.clone(),
                detail: Some(expected_detail.clone())
            }
        );
        assert_eq!(expected_detail.chars().count(), 257);
        registry.set_connection(
            "mini",
            ConnectionState::Dropped {
                reason: long.clone(),
                detail: Some("\n".to_string()),
                retry_in_ms: 1,
            },
        );
        assert_eq!(
            registry.snapshot().hosts[0].connection,
            ConnectionState::Dropped {
                reason: long,
                detail: None,
                retry_in_ms: 1
            }
        );
    }

    #[test]
    fn new_trims_config_error() {
        let registry = HostRegistry::new(config(Some("bad\n\u{1b}[0m   json")), Vec::new());
        assert_eq!(
            registry.snapshot().config.error.as_deref(),
            Some("bad [0m json")
        );
        let registry = HostRegistry::new(config(None), Vec::new());
        assert_eq!(registry.snapshot().config.error, None);
    }

    #[test]
    fn replace_spaces_trims_terminal_text() {
        let mut registry = HostRegistry::new(config(None), vec![host("mini", Some(HOST_A))]);
        registry.replace_spaces(
            &host_id(HOST_A),
            vec![space(
                HOST_A,
                "/work/app",
                vec![terminal("t1", "  a\u{1b}[0m\nb ", &"c".repeat(400))],
            )],
            backend(),
        );
        let terminals = &registry.snapshot().hosts[0].spaces[0].terminals;
        assert_eq!(terminals[0].name, "a [0m b");
        assert_eq!(terminals[0].command, format!("{}…", "c".repeat(256)));
    }

    #[test]
    fn replace_spaces_records_the_backend_and_trims_its_id() {
        let mut registry = HostRegistry::new(config(None), vec![host("mini", Some(HOST_A))]);
        assert_eq!(registry.snapshot().hosts[0].backend, None);
        registry.replace_spaces(
            &host_id(HOST_A),
            Vec::new(),
            BackendView {
                id: " core\u{1b}[0m\nx ".to_string(),
                instance: Some("  4bcf\n/ ".to_string()),
            },
        );
        assert_eq!(
            registry.snapshot().hosts[0].backend,
            Some(BackendView {
                id: "core [0m x".to_string(),
                instance: Some("  4bcf\n/ ".to_string()),
            })
        );
    }

    #[test]
    fn spaces_survive_a_drop() {
        let mut registry = HostRegistry::new(config(None), vec![host("mini", Some(HOST_A))]);
        registry.replace_spaces(
            &host_id(HOST_A),
            vec![space(HOST_A, "/work/app", Vec::new())],
            backend(),
        );
        let before = registry.snapshot().revision;
        registry.set_connection(
            "mini",
            ConnectionState::Dropped {
                reason: "connection closed".to_string(),
                detail: None,
                retry_in_ms: 1000,
            },
        );
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.hosts[0].spaces.len(), 1);
        assert!(snapshot.revision > before);
    }

    #[test]
    fn spaces_are_keyed_by_host_id_and_path() {
        let mut registry = HostRegistry::new(
            config(None),
            vec![host("mini", Some(HOST_A)), host("work", Some(HOST_B))],
        );
        registry.replace_spaces(
            &host_id(HOST_A),
            vec![space(HOST_A, "/work/app", Vec::new())],
            backend(),
        );
        registry.replace_spaces(
            &host_id(HOST_B),
            vec![space(HOST_B, "/work/app", Vec::new())],
            backend(),
        );
        let snapshot = registry.snapshot();
        assert_eq!(snapshot.hosts[0].spaces[0].key.host_id, host_id(HOST_A));
        assert_eq!(snapshot.hosts[1].spaces[0].key.host_id, host_id(HOST_B));
        assert_eq!(snapshot.hosts[0].spaces[0].key.path, "/work/app");
        assert_eq!(snapshot.hosts[1].spaces[0].key.path, "/work/app");
    }

    #[test]
    fn replace_spaces_ignores_foreign_host_keys() {
        let mut registry = HostRegistry::new(config(None), vec![host("mini", Some(HOST_A))]);
        registry.replace_spaces(
            &host_id(HOST_A),
            vec![
                space(HOST_A, "/mine", Vec::new()),
                space(HOST_B, "/foreign", Vec::new()),
            ],
            backend(),
        );
        let spaces = registry.snapshot().hosts[0].spaces.clone();
        assert_eq!(spaces.len(), 1);
        assert_eq!(spaces[0].key.path, "/mine");

        let before = registry.snapshot();
        registry.replace_spaces(
            &host_id(HOST_B),
            vec![space(HOST_B, "/x", Vec::new())],
            backend(),
        );
        assert_eq!(registry.snapshot(), before);
    }

    #[test]
    fn set_host_id_records_the_id_and_every_mutation_bumps_the_revision() {
        let mut registry = HostRegistry::new(config(None), vec![host("mini", None)]);
        assert_eq!(registry.snapshot().revision, DecimalU64(0));
        registry.set_host_id("mini", &host_id(HOST_A));
        assert_eq!(registry.snapshot().revision, DecimalU64(1));
        assert_eq!(registry.snapshot().hosts[0].host_id, Some(host_id(HOST_A)));
        registry.set_connection("mini", ConnectionState::Connected);
        assert_eq!(registry.snapshot().revision, DecimalU64(2));
    }
}
