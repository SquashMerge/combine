//! The DTOs that cross the Tauri boundary, and `DecimalU64`, which keeps 64-bit IDs exact in JS.
//!
//! No DTO field is a raw `u64`: a JS number rounds above 2^53, so every protocol counter that
//! crosses is a decimal string.

use std::fmt;

use combine_protocol::{
    AttachmentId, BackendGeneration, Cursor, Epoch, HostId, OwnershipGeneration, RequestId, Seq,
    TermId,
};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::text::{display_text, is_blank};

/// A `u64` that serializes as its canonical decimal string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DecimalU64(pub u64);

impl Serialize for DecimalU64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

struct DecimalU64Visitor;

impl Visitor<'_> for DecimalU64Visitor {
    type Value = DecimalU64;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a canonical decimal string of a u64")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<DecimalU64, E> {
        let canonical = !v.is_empty()
            && v.len() <= 20
            && v.bytes().all(|b| b.is_ascii_digit())
            && (v == "0" || !v.starts_with('0'));
        if !canonical {
            return Err(E::invalid_value(de::Unexpected::Str(v), &self));
        }
        v.parse::<u64>()
            .map(DecimalU64)
            .map_err(|_| E::invalid_value(de::Unexpected::Str(v), &"a value that fits in a u64"))
    }
}

impl<'de> Deserialize<'de> for DecimalU64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(DecimalU64Visitor)
    }
}

macro_rules! decimal_conversions {
    ($($id:ident),+) => {$(
        impl From<$id> for DecimalU64 {
            fn from(id: $id) -> Self {
                DecimalU64(id.0)
            }
        }

        impl From<DecimalU64> for $id {
            fn from(value: DecimalU64) -> Self {
                $id(value.0)
            }
        }
    )+};
}

decimal_conversions!(
    AttachmentId,
    Epoch,
    Seq,
    OwnershipGeneration,
    BackendGeneration,
    RequestId
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcCursor {
    pub attachment: DecimalU64,
    pub epoch: DecimalU64,
    pub seq: DecimalU64,
}

impl From<Cursor> for IpcCursor {
    fn from(c: Cursor) -> Self {
        IpcCursor {
            attachment: c.attachment.into(),
            epoch: c.epoch.into(),
            seq: c.seq.into(),
        }
    }
}

impl From<IpcCursor> for Cursor {
    fn from(c: IpcCursor) -> Self {
        Cursor {
            attachment: c.attachment.into(),
            epoch: c.epoch.into(),
            seq: c.seq.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostsSnapshot {
    pub revision: DecimalU64,
    pub config: ConfigStatus,
    pub hosts: Vec<HostView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigStatus {
    pub path: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostView {
    pub name: String,
    pub host_id: Option<HostId>,
    pub connection: ConnectionState,
    pub spaces: Vec<SpaceView>,
    /// The backend that owns the host's terminals, once the daemon has said. Always serialized,
    /// `null` until then.
    pub backend: Option<BackendView>,
}

/// The backend behind a host's terminals. `id` is trimmed external text; `instance` is passed
/// whole because a layout tab ID is built from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendView {
    pub id: String,
    pub instance: Option<String>,
}

/// `reason` and `message` are Combine text; `detail` is external text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ConnectionState {
    Connecting,
    Connected,
    Dropped {
        reason: String,
        detail: Option<String>,
        retry_in_ms: u32,
    },
    Reconnecting {
        attempt: u32,
    },
    Failed {
        message: String,
        detail: Option<String>,
    },
}

impl ConnectionState {
    /// Trims the external `detail` of `Dropped` and `Failed` and nothing else: Combine's own
    /// `reason` and `message` pass through whole.
    pub fn for_display(self) -> Self {
        fn trim(detail: Option<String>) -> Option<String> {
            detail.filter(|d| !is_blank(d)).map(|d| display_text(&d))
        }
        match self {
            ConnectionState::Dropped {
                reason,
                detail,
                retry_in_ms,
            } => ConnectionState::Dropped {
                reason,
                detail: trim(detail),
                retry_in_ms,
            },
            ConnectionState::Failed { message, detail } => ConnectionState::Failed {
                message,
                detail: trim(detail),
            },
            other => other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceView {
    pub key: SpaceKey,
    pub terminals: Vec<TerminalSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceKey {
    pub host_id: HostId,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSummary {
    pub id: TermId,
    pub name: String,
    pub command: String,
    pub state: TerminalState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TerminalState {
    Running,
    Waiting,
    Idle,
    Exited,
    Unknown,
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    const MIDDLE: u64 = 9_007_199_254_740_993;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/../web/test/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).expect("read fixture");
        serde_json::from_str(&text).expect("fixture is JSON")
    }

    fn decimal(s: &str) -> Result<DecimalU64, serde_json::Error> {
        serde_json::from_value(Value::String(s.to_string()))
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct U64Fixture {
        above2pow53: DecimalU64,
        max: DecimalU64,
        zero: DecimalU64,
        cursor: IpcCursor,
        request: DecimalU64,
        backend_generation: DecimalU64,
        ownership_generation: DecimalU64,
    }

    #[test]
    fn decimal_u64_fixture_round_trips() {
        let file = fixture("u64-roundtrip.json");
        let parsed: U64Fixture = serde_json::from_value(file.clone()).expect("deserialize");
        assert_eq!(parsed.above2pow53.0, MIDDLE);
        assert_eq!(parsed.max.0, u64::MAX);
        assert_eq!(parsed.zero.0, 0);

        let attachment = AttachmentId::from(parsed.above2pow53);
        let epoch = Epoch::from(parsed.max);
        let seq = Seq::from(parsed.above2pow53);
        let request = RequestId::from(parsed.request);
        let backend = BackendGeneration::from(parsed.backend_generation);
        let ownership = OwnershipGeneration::from(parsed.ownership_generation);
        let cursor = Cursor::from(parsed.cursor.clone());
        assert_eq!(
            (
                attachment.0,
                epoch.0,
                seq.0,
                request.0,
                backend.0,
                ownership.0
            ),
            (MIDDLE, u64::MAX, MIDDLE, u64::MAX, MIDDLE, u64::MAX)
        );
        assert_eq!(
            (cursor.attachment, cursor.epoch, cursor.seq),
            (attachment, epoch, seq)
        );

        let rebuilt = U64Fixture {
            above2pow53: attachment.into(),
            max: epoch.into(),
            zero: DecimalU64(0),
            cursor: cursor.into(),
            request: request.into(),
            backend_generation: backend.into(),
            ownership_generation: ownership.into(),
        };
        assert_eq!(serde_json::to_value(&rebuilt).expect("serialize"), file);
    }

    #[test]
    fn decimal_u64_rejects_numbers_and_non_canonical_text() {
        assert_eq!(decimal("0").expect("zero").0, 0);
        assert_eq!(decimal("18446744073709551615").expect("max").0, u64::MAX);
        for bad in [
            "",
            "01",
            "00",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1e3",
            "0x10",
            "1.0",
            "18446744073709551616",
            "99999999999999999999",
            "\u{ff11}",
        ] {
            assert!(decimal(bad).is_err(), "{bad:?} should be rejected");
        }
        for number in [
            json!(1),
            json!(9_007_199_254_740_993_u64),
            json!(1.5),
            json!(-1),
        ] {
            assert!(serde_json::from_value::<DecimalU64>(number).is_err());
        }
        assert!(serde_json::from_value::<DecimalU64>(Value::Null).is_err());
        assert_eq!(
            serde_json::to_value(DecimalU64(u64::MAX)).expect("serialize"),
            json!("18446744073709551615")
        );
    }

    #[test]
    fn protocol_ids_convert_losslessly() {
        for n in [0, 1, MIDDLE, u64::MAX] {
            let d = DecimalU64(n);
            assert_eq!(DecimalU64::from(AttachmentId::from(d)), d);
            assert_eq!(DecimalU64::from(Epoch::from(d)), d);
            assert_eq!(DecimalU64::from(Seq::from(d)), d);
            assert_eq!(DecimalU64::from(OwnershipGeneration::from(d)), d);
            assert_eq!(DecimalU64::from(BackendGeneration::from(d)), d);
            assert_eq!(DecimalU64::from(RequestId::from(d)), d);
        }
        let cursor = Cursor {
            attachment: AttachmentId(MIDDLE),
            epoch: Epoch(u64::MAX),
            seq: Seq(7),
        };
        assert_eq!(Cursor::from(IpcCursor::from(cursor)), cursor);
    }

    #[test]
    fn connection_state_for_display_trims_only_detail() {
        let long = "m".repeat(300);
        let dropped = ConnectionState::Dropped {
            reason: format!("{long}\n"),
            detail: Some("a\n\u{1b}b".to_string()),
            retry_in_ms: 1000,
        }
        .for_display();
        assert_eq!(
            dropped,
            ConnectionState::Dropped {
                reason: format!("{long}\n"),
                detail: Some("a b".to_string()),
                retry_in_ms: 1000
            }
        );
        let failed = ConnectionState::Failed {
            message: long.clone(),
            detail: Some(" \n".to_string()),
        }
        .for_display();
        assert_eq!(
            failed,
            ConnectionState::Failed {
                message: long,
                detail: None
            }
        );
        assert_eq!(
            ConnectionState::Connected.for_display(),
            ConnectionState::Connected
        );
    }

    fn id(s: &str) -> HostId {
        HostId(s.to_string())
    }

    fn view(
        name: &str,
        host_id: Option<&str>,
        connection: ConnectionState,
        spaces: Vec<SpaceView>,
        backend: Option<BackendView>,
    ) -> HostView {
        HostView {
            name: name.to_string(),
            host_id: host_id.map(id),
            connection,
            spaces,
            backend,
        }
    }

    fn terminal(id: &str, name: &str, command: &str, state: TerminalState) -> TerminalSummary {
        TerminalSummary {
            id: TermId(id.to_string()),
            name: name.to_string(),
            command: command.to_string(),
            state,
        }
    }

    #[test]
    fn snapshot_matches_shared_fixture() {
        let this_mac = "0f8c2a6e-4b1d-4c5e-9a7b-3d2e1f0a9b8c";
        let mini = "6a1d3c9e-2f4b-4e8a-b7c5-1d0e9f8a7b6c";
        let snapshot = HostsSnapshot {
            revision: DecimalU64(u64::MAX),
            config: ConfigStatus {
                path: "/Users/example/Library/Application Support/com.squashmerge.combine/hosts.json".to_string(),
                error: None,
            },
            hosts: vec![
                view(
                    "this-mac",
                    Some(this_mac),
                    ConnectionState::Connected,
                    vec![SpaceView {
                        key: SpaceKey { host_id: id(this_mac), path: "/Users/example/src/trellis".to_string() },
                        terminals: vec![
                            terminal("hc:t8", "claude", "claude", TerminalState::Waiting),
                            terminal("hc:t9", "prime-1", "prime-agent run", TerminalState::Running),
                        ],
                    }],
                    Some(BackendView {
                        id: "herdr-core".to_string(),
                        instance: Some("4bcff4a58f4d97d505396e5a43a807e6".to_string()),
                    }),
                ),
                view(
                    "mini",
                    Some(mini),
                    ConnectionState::Dropped {
                        reason: "connection closed".to_string(),
                        detail: None,
                        retry_in_ms: 4000,
                    },
                    vec![SpaceView {
                        key: SpaceKey { host_id: id(mini), path: "/Users/example/src/mimir".to_string() },
                        terminals: Vec::new(),
                    }],
                    None,
                ),
                view(
                    "work-mbp",
                    None,
                    ConnectionState::Failed {
                        message: "SSH authentication to work-mbp.local failed. Run `ssh work-mbp.local` once in a terminal to check your login; if your key has a passphrase, run `ssh-add` first. Then retry.".to_string(),
                        detail: Some("example@work-mbp.local: Permission denied (publickey).".to_string()),
                    },
                    Vec::new(),
                    None,
                ),
                view("spare", None, ConnectionState::Reconnecting { attempt: 2 }, Vec::new(), None),
                view("new", None, ConnectionState::Connecting, Vec::new(), None),
            ],
        };
        let file = fixture("hosts-snapshot.json");
        assert_eq!(serde_json::to_value(&snapshot).expect("serialize"), file);
        let parsed: HostsSnapshot = serde_json::from_value(file).expect("deserialize");
        assert_eq!(parsed, snapshot);
    }
}
