//! Identifier newtypes and the attachment cursor.

use serde::{Deserialize, Serialize};

macro_rules! numeric_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}

numeric_id!(
    /// Names one attachment of a terminal on a connection.
    AttachmentId
);
numeric_id!(
    /// Counts the backend generations of an attachment; `Started` opens each.
    Epoch
);
numeric_id!(
    /// Orders the events of one attachment within an epoch.
    Seq
);
numeric_id!(
    /// Fences resizes and automatic replies against ownership changes.
    OwnershipGeneration
);
numeric_id!(
    /// The backend's own generation counter, reported on `Started`.
    BackendGeneration
);
numeric_id!(
    /// Chosen by the client and echoed in the reply.
    RequestId
);

/// The daemon's UUID, lowercase and hyphenated. The codec does not validate it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostId(pub String);

/// The backend's opaque terminal ID. The codec does not validate it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TermId(pub String);

/// A position in an attachment's event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Cursor {
    pub attachment: AttachmentId,
    pub epoch: Epoch,
    pub seq: Seq,
}
