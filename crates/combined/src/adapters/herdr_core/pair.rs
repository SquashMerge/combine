//! Opening one attachment's pair of herdr-core viewers.
//!
//! Only watch-only viewers get ordered resize boundaries from herdr-core, so each attachment is
//! a display half (`observe`, with `resize_notifications`) that carries the output and an input
//! half (an active viewer) that types, resizes and answers queries. The input half's own
//! output is credited and discarded.

use std::time::Duration;

use combine_protocol::Display;
use serde_json::Value;
use tokio::sync::mpsc;

use super::connection::{CallError, Connection};
use super::frame::{self, Frame, Kind};
use super::jsonrpc::{self, AttachParams};
use super::Env;
use crate::ports::BackendError;

/// How long the display half has to send its first RESIZE.
const FIRST_RESIZE_TIMEOUT: Duration = Duration::from_secs(5);

/// One open viewer of the pair: its connection, the frames it receives and its channel.
pub(super) struct Viewer {
    pub connection: Connection,
    pub frames: mpsc::UnboundedReceiver<Frame>,
    pub channel: u32,
}

/// A pair that is open.
pub(super) struct Halves {
    pub display: Viewer,
    pub input: Option<Viewer>,
    /// Display frames that arrived before the first RESIZE, still to be handled.
    pub prefetched: Vec<Frame>,
    /// The PTY size the display half started with.
    pub size: (u16, u16),
    pub ordered: bool,
    /// The input half confirmed AUTOMATIC_INPUT.
    pub automatic: bool,
    pub created_ms: u64,
    pub screen: String,
}

#[derive(Debug)]
pub(super) enum OpenError {
    /// The program has ended.
    Finished,
    NotFound,
    Failed(BackendError),
}

impl From<CallError> for OpenError {
    fn from(error: CallError) -> Self {
        match &error {
            CallError::Rpc(rpc) if rpc.kind.as_deref() == Some("finished") => OpenError::Finished,
            CallError::Rpc(rpc) if rpc.kind.as_deref() == Some("not_found") => OpenError::NotFound,
            _ => OpenError::Failed(error.to_failure()),
        }
    }
}

pub(super) struct OpenParams<'a> {
    pub address: &'a str,
    pub size: (u16, u16),
    pub display: &'a Display,
    pub history: u32,
    pub resumed: bool,
    pub watch_only: bool,
}

async fn attach_half(
    env: &Env,
    params: &OpenParams<'_>,
    display_half: bool,
) -> Result<(Viewer, jsonrpc::AttachResult), OpenError> {
    let (connection, frames) = Connection::open(&env.config.socket)
        .await
        .map_err(OpenError::Failed)?;
    let request = jsonrpc::attach_params(&AttachParams {
        address: params.address,
        cols: params.size.0,
        rows: params.size.1,
        display: params.display,
        history: if display_half { params.history } else { 0 },
        observe: display_half,
        resize_notifications: display_half,
        resumed: if display_half { params.resumed } else { true },
    });
    let result: Value = connection.call("terminal.attach", request).await?;
    let result = jsonrpc::parse_attach_result(result).map_err(OpenError::Failed)?;
    let channel = result.channel;
    Ok((
        Viewer {
            connection,
            frames,
            channel,
        },
        result,
    ))
}

/// The display half's first RESIZE gives `Started` its size; anything before it is kept.
async fn first_resize(half: &mut Viewer) -> Result<((u16, u16), Vec<Frame>), BackendError> {
    let mut prefetched = Vec::new();
    let waited = tokio::time::timeout(FIRST_RESIZE_TIMEOUT, async {
        while let Some(frame) = half.frames.recv().await {
            if frame.kind == Kind::Resize {
                if let Some(size) = frame::parse_resize(&frame.payload) {
                    return Some(size);
                }
            }
            prefetched.push(frame);
        }
        None
    })
    .await;
    match waited {
        Ok(Some(size)) => Ok((size, prefetched)),
        Ok(None) => Err(BackendError::Failed(
            "the display half closed before its first RESIZE".to_string(),
        )),
        Err(_) => Err(BackendError::Failed(
            "the display half sent no RESIZE within 5 s".to_string(),
        )),
    }
}

/// Opens the display half, then the input half. A failure closes whatever opened.
pub(super) async fn open(env: &Env, params: &OpenParams<'_>) -> Result<Halves, OpenError> {
    let (mut display, reply) = attach_half(env, params, true).await?;
    let ordered = reply.resize_notifications;
    let (size, prefetched) = if ordered {
        first_resize(&mut display)
            .await
            .map_err(OpenError::Failed)?
    } else {
        ((reply.terminal.cols, reply.terminal.rows), Vec::new())
    };

    #[cfg(feature = "test-support")]
    if env.control.take_open_failure(params.address) {
        return Err(OpenError::Failed(BackendError::Failed(
            "the input half failed to open".to_string(),
        )));
    }

    let (input, automatic) = if params.watch_only {
        (None, false)
    } else {
        let (input, input_reply) = attach_half(env, params, false).await?;
        let automatic = env.automatic_feature && input_reply.automatic_input;
        (Some(input), automatic)
    };
    Ok(Halves {
        display,
        input,
        prefetched,
        size,
        ordered,
        automatic,
        created_ms: reply.terminal.created_ms,
        screen: reply.terminal.screen,
    })
}

impl Halves {
    /// Says goodbye on both halves and lets the writers flush.
    pub(super) fn close(self) {
        if let Some(writer) = self.display.connection.sender() {
            let _ = writer.try_send(frame::close(self.display.channel));
        }
        self.display.connection.close();
        if let Some(input) = self.input {
            if let Some(writer) = input.connection.sender() {
                let _ = writer.try_send(frame::close(input.channel));
            }
            input.connection.close();
        }
    }
}
