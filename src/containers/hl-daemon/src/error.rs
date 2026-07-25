/// Daemon composition, socket, and protocol failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("socket path has no parent directory")]
    SocketParent,
    #[error("refusing to replace non-socket path {0}")]
    OccupiedSocket(std::path::PathBuf),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("server failed: {0}")]
    Hyper(#[from] hyper::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
