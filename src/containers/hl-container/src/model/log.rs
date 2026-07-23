use serde::{Deserialize, Serialize};

use super::{ContainerId, ExecId};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum JournalId {
    Container(ContainerId),
    Exec(ExecId),
}

impl JournalId {
    pub(crate) fn container(id: ContainerId) -> Self {
        Self::Container(id)
    }

    pub(crate) fn exec(id: ExecId) -> Self {
        Self::Exec(id)
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Container(id) => id.as_str(),
            Self::Exec(id) => id.as_str(),
        }
    }
}

/// Standard output stream that produced a log chunk.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One durably ordered process-output record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub stream: Stream,
    pub bytes: Vec<u8>,
}

/// Ordered bytes emitted by one process stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LogChunk {
    pub(crate) stream: Stream,
    pub(crate) bytes: Vec<u8>,
}

/// Captured initial-process output.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Logs {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
