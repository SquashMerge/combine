//! The link to a host: connector, failure text and exit info.
//!
//! A [`Link`] is the byte stream of one connect attempt: an `ssh` child's stdio or a Unix
//! socket. Whoever holds a link ends it with [`Link::finish`] or [`abort_link`], so an `ssh`
//! child is always reaped.

use std::io::{Read, Write};
use std::sync::Arc;

/// Combine's wording for a failure, plus the external text that explains it.
///
/// `text` is Combine text and is published whole. `detail` is raw external text, such as an OS
/// error, `ssh` stderr or a daemon's message, and is trimmed where snapshots are made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureText {
    pub text: String,
    pub detail: Option<String>,
}

/// How a link ended: the exit status of an `ssh` child and the tail of its stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: Option<i32>,
    pub stderr: String,
}

/// Unblocks a blocked read: kills the child, or shuts the socket down.
pub type Closer = Arc<dyn Fn() + Send + Sync>;

/// Reports how the other end of a link ended, after the link's streams are dropped.
pub trait ExitReport: Send {
    fn finish(self: Box<Self>) -> Option<ExitInfo>;
}

pub struct Link {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    closer: Closer,
    exit: Box<dyn ExitReport>,
}

impl Link {
    pub fn new(
        reader: Box<dyn Read + Send>,
        writer: Box<dyn Write + Send>,
        closer: Closer,
        exit: Box<dyn ExitReport>,
    ) -> Link {
        Link {
            reader,
            writer,
            closer,
            exit,
        }
    }

    pub fn closer(&self) -> Closer {
        Arc::clone(&self.closer)
    }

    /// Drops the reader and the writer, then asks how the link ended.
    pub fn finish(self) -> Option<ExitInfo> {
        let Link {
            reader,
            writer,
            closer: _,
            exit,
        } = self;
        drop(reader);
        drop(writer);
        exit.finish()
    }
}

/// Kills an `ssh` child or shuts the socket, then reaps.
pub fn abort_link(link: Link) {
    (link.closer())();
    let _ = link.finish();
}

pub trait Connector: Send + Sync {
    fn connect(&self) -> Result<Link, ConnectError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// Retrying cannot help until the user acts.
    Permanent(FailureText),
    /// Worth retrying after a delay.
    Transient(FailureText),
}
