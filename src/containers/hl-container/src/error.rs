use crate::{ContainerId, ContainerState, ExecId, ExecState};

/// Container lifecycle failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Checkpoint(#[from] crate::CheckpointError),
    #[error("invalid container configuration: {0}")]
    TranslationCache(#[from] crate::config::TranslationCacheError),
    #[error("invalid container specification: {0}")]
    InvalidSpec(String),
    #[error("invalid volume: {0}")]
    InvalidVolume(String),
    #[error("invalid network: {0}")]
    InvalidNetwork(String),
    #[error("container {0} was not found")]
    NotFound(String),
    #[error("exec {0} was not found")]
    ExecNotFound(ExecId),
    #[error("container name {0:?} is already in use")]
    NameConflict(String),
    #[error("host TCP address {0}:{1} is already in use")]
    PortConflict(std::net::Ipv4Addr, u16),
    #[error("volume name {0:?} is already in use")]
    VolumeConflict(String),
    #[error("volume {0:?} was not found")]
    VolumeNotFound(String),
    #[error("volume {0:?} is referenced by a container")]
    VolumeInUse(String),
    #[error("network name {0:?} is already in use")]
    NetworkConflict(String),
    #[error("network {0:?} was not found")]
    NetworkNotFound(String),
    #[error("network {0:?} has attached containers")]
    NetworkInUse(String),
    #[error("container {container} is already connected to network {network:?}")]
    AlreadyConnected { network: String, container: ContainerId },
    #[error("container {container} is not connected to network {network:?}")]
    NotConnected { network: String, container: ContainerId },
    #[error("container {id} is {actual:?}, expected {expected}")]
    InvalidState {
        id: ContainerId,
        actual: ContainerState,
        expected: &'static str,
    },
    #[error(
        "exec {id} cannot be reattached after a whole-image restore: {reason}"
    )]
    ExecNotReattachable { id: ExecId, reason: &'static str },
    #[error("exec {id} is {actual:?}, expected {expected}")]
    InvalidExecState {
        id: ExecId,
        actual: ExecState,
        expected: &'static str,
    },
    #[error("container {id} did not reach a terminal state within {seconds}s of a forced stop")]
    StopTimeout { id: ContainerId, seconds: u64 },
    #[error("container {0} is already running")]
    AlreadyRunning(ContainerId),
    #[error("process {0} has no terminal")]
    NoTerminal(String),
    #[error("container path is read-only: {}", .0.display())]
    ReadOnly(std::path::PathBuf),
    #[error("runtime failed: {0}")]
    Runtime(String),
    #[error("container state is corrupt: {0}")]
    Corrupt(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Image(#[from] hl_images::Error),
}

/// The boundary back onto `std::io` for callers that report `io::Result`. An I/O failure keeps its
/// original `ErrorKind`; no other variant has an `ErrorKind` equivalent, so it is carried whole.
impl From<Error> for std::io::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Io(error) => error,
            other => std::io::Error::other(other),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
