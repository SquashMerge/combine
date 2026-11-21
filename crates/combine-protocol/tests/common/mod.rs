#![allow(dead_code)]

use combine_protocol::*;

pub const DISPLAY_JSON: &str = r##"{"foreground":"#ddeeff","background":"#112233","cursor_color":"#aabbcc","cell_width":9,"cell_height":18}"##;

pub fn display() -> Display {
    Display {
        foreground: "#ddeeff".into(),
        background: "#112233".into(),
        cursor_color: "#aabbcc".into(),
        cell_width: 9,
        cell_height: 18,
    }
}

pub fn cursor(epoch: u64, seq: u64) -> Cursor {
    Cursor {
        attachment: AttachmentId(7),
        epoch: Epoch(epoch),
        seq: Seq(seq),
    }
}

/// `be32(1 + json.len()) ++ 0x01 ++ json`.
pub fn control_frame(json: &str) -> Vec<u8> {
    frame(1, json.as_bytes())
}

pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

pub fn words(words: &[u64], tail: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    out.extend_from_slice(tail);
    out
}

pub fn client_goldens() -> Vec<(&'static str, ClientMessage, Vec<u8>)> {
    let attach = format!(
        r#"{{"type":"attach","request":1,"terminal":"fake:t1","options":{{"cols":80,"rows":24,"display":{DISPLAY_JSON},"observe":false,"history":1000}}}}"#
    );
    let set_display =
        format!(r#"{{"type":"set_display","attachment":7,"display":{DISPLAY_JSON}}}"#);
    vec![
        (
            "hello",
            ClientMessage::Hello(Hello {
                protocol_version: 1,
            }),
            control_frame(r#"{"type":"hello","protocol_version":1}"#),
        ),
        (
            "attach",
            ClientMessage::Attach(Attach {
                request: RequestId(1),
                terminal: TermId("fake:t1".into()),
                options: AttachRequestOptions {
                    cols: 80,
                    rows: 24,
                    display: display(),
                    observe: false,
                    history: 1000,
                },
            }),
            control_frame(&attach),
        ),
        (
            "resume",
            ClientMessage::Resume(Resume {
                request: RequestId(2),
                cursor: cursor(3, 41),
            }),
            control_frame(
                r#"{"type":"resume","request":2,"cursor":{"attachment":7,"epoch":3,"seq":41}}"#,
            ),
        ),
        (
            "detach",
            ClientMessage::Detach(Detach {
                attachment: AttachmentId(7),
            }),
            control_frame(r#"{"type":"detach","attachment":7}"#),
        ),
        (
            "ack",
            ClientMessage::Ack(Ack {
                cursor: cursor(3, 42),
            }),
            control_frame(r#"{"type":"ack","cursor":{"attachment":7,"epoch":3,"seq":42}}"#),
        ),
        (
            "resize",
            ClientMessage::Resize(Resize {
                attachment: AttachmentId(7),
                cols: 120,
                rows: 40,
                ownership_generation: OwnershipGeneration(5),
            }),
            control_frame(
                r#"{"type":"resize","attachment":7,"cols":120,"rows":40,"ownership_generation":5}"#,
            ),
        ),
        (
            "focus",
            ClientMessage::Focus(Focus {
                attachment: AttachmentId(7),
            }),
            control_frame(r#"{"type":"focus","attachment":7}"#),
        ),
        (
            "set_display",
            ClientMessage::SetDisplay(SetDisplay {
                attachment: AttachmentId(7),
                display: display(),
            }),
            control_frame(&set_display),
        ),
        (
            "request_ownership",
            ClientMessage::RequestOwnership(RequestOwnership {
                attachment: AttachmentId(7),
            }),
            control_frame(r#"{"type":"request_ownership","attachment":7}"#),
        ),
        (
            "input",
            ClientMessage::Input(Input {
                attachment: AttachmentId(7),
                bytes: b"ls\r".to_vec(),
            }),
            vec![0, 0, 0, 0x0c, 3, 0, 0, 0, 0, 0, 0, 0, 7, 0x6c, 0x73, 0x0d],
        ),
        (
            "automatic_reply",
            ClientMessage::AutomaticReply(AutomaticReply {
                attachment: AttachmentId(7),
                epoch: Epoch(3),
                ownership_generation: OwnershipGeneration(5),
                bytes: b"\x1b[1;1R".to_vec(),
            }),
            frame(4, &words(&[7, 3, 5], b"\x1b[1;1R")),
        ),
    ]
}

pub fn daemon_goldens() -> Vec<(&'static str, DaemonMessage, Vec<u8>)> {
    let facts = format!(
        r#"{{"type":"facts","cursor":{{"attachment":7,"epoch":4,"seq":13}},"display":{DISPLAY_JSON}}}"#
    );
    vec![
        (
            "welcome",
            DaemonMessage::Welcome(Welcome {
                protocol_version: 1,
                host_id: HostId("3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b".into()),
            }),
            control_frame(
                r#"{"type":"welcome","protocol_version":1,"host_id":"3f2b8c1e-5d4a-4e6f-9a7b-1c2d3e4f5a6b"}"#,
            ),
        ),
        (
            "attached",
            DaemonMessage::Attached(Attached {
                request: RequestId(1),
                attachment: AttachmentId(7),
            }),
            control_frame(r#"{"type":"attached","request":1,"attachment":7}"#),
        ),
        (
            "resumed",
            DaemonMessage::Resumed(Resumed {
                request: RequestId(2),
                attachment: AttachmentId(7),
                replay_through: Some(cursor(3, 45)),
            }),
            control_frame(
                r#"{"type":"resumed","request":2,"attachment":7,"replay_through":{"attachment":7,"epoch":3,"seq":45}}"#,
            ),
        ),
        (
            "resumed_none",
            DaemonMessage::Resumed(Resumed {
                request: RequestId(2),
                attachment: AttachmentId(7),
                replay_through: None,
            }),
            control_frame(r#"{"type":"resumed","request":2,"attachment":7,"replay_through":null}"#),
        ),
        (
            "gap",
            DaemonMessage::Gap(Gap {
                attachment: AttachmentId(7),
                request: None,
            }),
            control_frame(r#"{"type":"gap","attachment":7,"request":null}"#),
        ),
        (
            "gap_answering_resume",
            DaemonMessage::Gap(Gap {
                attachment: AttachmentId(7),
                request: Some(RequestId(2)),
            }),
            control_frame(r#"{"type":"gap","attachment":7,"request":2}"#),
        ),
        (
            "ownership",
            DaemonMessage::Ownership(Ownership {
                attachment: AttachmentId(7),
                ownership_generation: OwnershipGeneration(5),
                owner: Some(AttachmentId(9)),
            }),
            control_frame(
                r#"{"type":"ownership","attachment":7,"ownership_generation":5,"owner":9}"#,
            ),
        ),
        (
            "ownership_none",
            DaemonMessage::Ownership(Ownership {
                attachment: AttachmentId(7),
                ownership_generation: OwnershipGeneration(5),
                owner: None,
            }),
            control_frame(
                r#"{"type":"ownership","attachment":7,"ownership_generation":5,"owner":null}"#,
            ),
        ),
        (
            "error",
            DaemonMessage::Error(ErrorMessage {
                code: ErrorCode::StaleGeneration,
                message: "resize from ownership generation 4; current is 5".into(),
                request: None,
                attachment: Some(AttachmentId(7)),
            }),
            control_frame(
                r#"{"type":"error","code":"stale_generation","message":"resize from ownership generation 4; current is 5","request":null,"attachment":7}"#,
            ),
        ),
        (
            "started",
            DaemonMessage::Started(Started {
                cursor: cursor(4, 10),
                backend_generation: BackendGeneration(2),
                cols: 80,
                rows: 24,
                fidelity: Fidelity::Exact,
                ordered_resizes: true,
            }),
            control_frame(
                r#"{"type":"started","cursor":{"attachment":7,"epoch":4,"seq":10},"backend_generation":2,"cols":80,"rows":24,"fidelity":"exact","ordered_resizes":true}"#,
            ),
        ),
        (
            "data",
            DaemonMessage::Data(Data {
                cursor: cursor(4, 11),
                bytes: b"hi".to_vec(),
            }),
            frame(2, &words(&[7, 4, 11], b"hi")),
        ),
        (
            "resized",
            DaemonMessage::Resized(Resized {
                cursor: cursor(4, 12),
                cols: 120,
                rows: 40,
            }),
            control_frame(
                r#"{"type":"resized","cursor":{"attachment":7,"epoch":4,"seq":12},"cols":120,"rows":40}"#,
            ),
        ),
        (
            "facts",
            DaemonMessage::Facts(Facts {
                cursor: cursor(4, 13),
                display: display(),
            }),
            control_frame(&facts),
        ),
        (
            "resynced",
            DaemonMessage::Resynced(Resynced {
                cursor: cursor(4, 14),
            }),
            control_frame(r#"{"type":"resynced","cursor":{"attachment":7,"epoch":4,"seq":14}}"#),
        ),
        (
            "closed",
            DaemonMessage::Closed(Closed {
                cursor: cursor(4, 15),
                reason: CloseReason::Detached,
            }),
            control_frame(
                r#"{"type":"closed","cursor":{"attachment":7,"epoch":4,"seq":15},"reason":"detached"}"#,
            ),
        ),
        (
            "lost",
            DaemonMessage::Lost(Lost {
                cursor: cursor(4, 16),
            }),
            control_frame(r#"{"type":"lost","cursor":{"attachment":7,"epoch":4,"seq":16}}"#),
        ),
    ]
}
