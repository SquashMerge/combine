//! The handshake and its failure mapping.
//!
//! `text` is always Combine's wording. Anything the daemon or the OS said goes to `detail`,
//! which the registry trims.

use std::io::{Read, Write};

use combine_protocol::{
    client_handshake, ErrorCode, ErrorMessage, HandshakeError, ReadError, VersionMismatch, Welcome,
    PROTOCOL_VERSION,
};

use super::link::FailureText;
use super::text::display_text;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeFailure {
    Transient(FailureText),
    Permanent(FailureText),
}

pub fn run_handshake(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
) -> Result<Welcome, HandshakeFailure> {
    client_handshake(reader, writer).map_err(|e| describe(&e))
}

fn failure(text: impl Into<String>, detail: Option<String>) -> FailureText {
    FailureText {
        text: text.into(),
        detail,
    }
}

pub fn describe(e: &HandshakeError) -> HandshakeFailure {
    use HandshakeFailure::{Permanent, Transient};
    match e {
        HandshakeError::Read(ReadError::Io(e)) => Transient(failure(
            "connection lost during the handshake",
            Some(e.to_string()),
        )),
        HandshakeError::Read(ReadError::Decode(e)) => Permanent(failure(
            "the daemon sent a malformed handshake frame",
            Some(e.to_string()),
        )),
        HandshakeError::Write(e) => Transient(failure("could not send hello", Some(e.to_string()))),
        HandshakeError::Closed => {
            Transient(failure("connection closed during the handshake", None))
        }
        HandshakeError::UnexpectedMessage { type_name } => Permanent(failure(
            format!("the daemon answered hello with {type_name}, not welcome"),
            None,
        )),
        HandshakeError::VersionMismatch(m) => Permanent(failure(m.to_string(), None)),
        HandshakeError::Refused(e) => Permanent(describe_refusal(e)),
    }
}

fn describe_refusal(e: &ErrorMessage) -> FailureText {
    if e.code == ErrorCode::VersionMismatch {
        return match version_mismatch_text(&e.message) {
            Some(mismatch) => failure(mismatch.to_string(), None),
            None => failure(
                format!("protocol version mismatch: client {PROTOCOL_VERSION}, daemon unknown"),
                Some(e.message.clone()),
            ),
        };
    }
    failure(
        format!("the daemon refused the connection ({})", code_name(e.code)),
        Some(e.message.clone()),
    )
}

/// The message is readable when, once trimmed for display, it is exactly
/// `protocol version mismatch: client <c>, daemon <d>` with both numbers canonical `u32`s.
fn version_mismatch_text(message: &str) -> Option<VersionMismatch> {
    fn number(s: &str) -> Option<u32> {
        s.parse::<u32>().ok().filter(|n| n.to_string() == s)
    }
    let shown = display_text(message);
    let rest = shown.strip_prefix("protocol version mismatch: client ")?;
    let (client, daemon) = rest.split_once(", daemon ")?;
    Some(VersionMismatch {
        client: number(client)?,
        daemon: number(daemon)?,
    })
}

fn code_name(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::VersionMismatch => "version_mismatch",
        ErrorCode::Malformed => "malformed",
        ErrorCode::UnknownTerminal => "unknown_terminal",
        ErrorCode::AttachFailed => "attach_failed",
        ErrorCode::UnknownAttachment => "unknown_attachment",
        ErrorCode::StaleGeneration => "stale_generation",
        ErrorCode::WatchOnly => "watch_only",
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::UnknownSpace => "unknown_space",
        ErrorCode::StaleLayout => "stale_layout",
        ErrorCode::UnknownTemplate => "unknown_template",
        ErrorCode::TemplateChanged => "template_changed",
        ErrorCode::CwdRefused => "cwd_refused",
        ErrorCode::SpawnFailed => "spawn_failed",
        ErrorCode::UnknownPanel => "unknown_panel",
        ErrorCode::UnknownCaller => "unknown_caller",
        ErrorCode::PanelLimit => "panel_limit",
        ErrorCode::Internal => "internal",
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::{host_id, joins_within, within, Double, HOST_A, WAIT};
    use super::*;

    fn handshake_with(double: Double) -> Result<Welcome, HandshakeFailure> {
        let (mut link, _drop, server) = double.pair();
        let result = within("run_handshake", move || {
            let result = run_handshake(&mut *link.reader, &mut *link.writer);
            drop(link);
            result
        });
        assert!(joins_within(server, WAIT));
        result
    }

    fn refusal(code: ErrorCode, message: &str) -> ErrorMessage {
        ErrorMessage {
            code,
            message: message.to_string(),
            request: None,
            attachment: None,
        }
    }

    #[test]
    fn welcome_from_double_is_accepted() {
        let welcome = handshake_with(Double::welcome(host_id(HOST_A))).expect("welcome");
        assert_eq!(welcome.host_id, host_id(HOST_A));
        assert_eq!(welcome.protocol_version, PROTOCOL_VERSION);
    }

    #[test]
    fn version_mismatch_error_frame_is_shown_with_both_versions() {
        let text = format!(
            "protocol version mismatch: client {PROTOCOL_VERSION}, daemon {}",
            PROTOCOL_VERSION + 1
        );
        let double = Double::refuse(refusal(ErrorCode::VersionMismatch, &text));
        assert_eq!(
            handshake_with(double).expect_err("refused"),
            HandshakeFailure::Permanent(FailureText { text, detail: None })
        );
    }

    #[test]
    fn welcome_with_other_version_is_a_mismatch() {
        let other = PROTOCOL_VERSION + 1;
        let double = Double::welcome_version(other, host_id(HOST_A));
        assert_eq!(
            handshake_with(double).expect_err("mismatch"),
            HandshakeFailure::Permanent(FailureText {
                text: format!(
                    "protocol version mismatch: client {PROTOCOL_VERSION}, daemon {other}"
                ),
                detail: None
            })
        );
    }

    #[test]
    fn unreadable_mismatch_text_names_client_version() {
        let double = Double::refuse(refusal(ErrorCode::VersionMismatch, "versions differ"));
        assert_eq!(
            handshake_with(double).expect_err("refused"),
            HandshakeFailure::Permanent(FailureText {
                text: format!(
                    "protocol version mismatch: client {PROTOCOL_VERSION}, daemon unknown"
                ),
                detail: Some("versions differ".to_string())
            })
        );
    }

    #[test]
    fn refusal_text_is_carried_as_detail() {
        let double = Double::refuse(refusal(ErrorCode::Internal, "disk\nfull"));
        assert_eq!(
            handshake_with(double).expect_err("refused"),
            HandshakeFailure::Permanent(FailureText {
                text: "the daemon refused the connection (internal)".to_string(),
                detail: Some("disk\nfull".to_string())
            })
        );
    }

    #[test]
    fn closed_before_reply_is_transient() {
        assert_eq!(
            handshake_with(Double::close_after_hello()).expect_err("closed"),
            HandshakeFailure::Transient(FailureText {
                text: "connection closed during the handshake".to_string(),
                detail: None
            })
        );
    }

    #[test]
    fn every_error_code_has_its_snake_case_name() {
        let codes = [
            (ErrorCode::VersionMismatch, "version_mismatch"),
            (ErrorCode::Malformed, "malformed"),
            (ErrorCode::UnknownTerminal, "unknown_terminal"),
            (ErrorCode::AttachFailed, "attach_failed"),
            (ErrorCode::UnknownAttachment, "unknown_attachment"),
            (ErrorCode::StaleGeneration, "stale_generation"),
            (ErrorCode::WatchOnly, "watch_only"),
            (ErrorCode::InvalidRequest, "invalid_request"),
            (ErrorCode::UnknownSpace, "unknown_space"),
            (ErrorCode::StaleLayout, "stale_layout"),
            (ErrorCode::UnknownTemplate, "unknown_template"),
            (ErrorCode::TemplateChanged, "template_changed"),
            (ErrorCode::CwdRefused, "cwd_refused"),
            (ErrorCode::SpawnFailed, "spawn_failed"),
            (ErrorCode::UnknownPanel, "unknown_panel"),
            (ErrorCode::UnknownCaller, "unknown_caller"),
            (ErrorCode::PanelLimit, "panel_limit"),
            (ErrorCode::Internal, "internal"),
        ];
        for (code, name) in codes {
            assert_eq!(code_name(code), name);
            let json = serde_json::to_string(&code).expect("serialize");
            assert_eq!(json, format!("\"{name}\""));
        }
    }

    #[test]
    fn describe_maps_the_other_rows() {
        let unexpected = HandshakeError::UnexpectedMessage { type_name: "data" };
        assert_eq!(
            describe(&unexpected),
            HandshakeFailure::Permanent(FailureText {
                text: "the daemon answered hello with data, not welcome".to_string(),
                detail: None
            })
        );
        let mismatch = HandshakeError::VersionMismatch(VersionMismatch {
            client: PROTOCOL_VERSION,
            daemon: PROTOCOL_VERSION + 1,
        });
        assert_eq!(
            describe(&mismatch),
            HandshakeFailure::Permanent(FailureText {
                text: format!(
                    "protocol version mismatch: client {PROTOCOL_VERSION}, daemon {}",
                    PROTOCOL_VERSION + 1
                ),
                detail: None
            })
        );
        let io = HandshakeError::Read(ReadError::Io(std::io::Error::other("reset")));
        assert_eq!(
            describe(&io),
            HandshakeFailure::Transient(FailureText {
                text: "connection lost during the handshake".to_string(),
                detail: Some("reset".to_string())
            })
        );
    }
}
