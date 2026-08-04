/// Client errors preserve transport and Docker server failures separately.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid client configuration: {0}")]
    Config(String),
    #[error("request timed out")]
    Timeout,
    #[error("transport failed: {0}")]
    Transport(#[from] std::io::Error),
    #[error("HTTP protocol failed: {0}")]
    Http(#[from] hyper::Error),
    #[error("HTTP connection driver failed: {0}")]
    Connection(String),
    #[error("invalid HTTP request: {0}")]
    Request(#[from] http::Error),
    #[error("response exceeded configured limit of {limit} bytes")]
    ResponseTooLarge { limit: usize },
    #[error("Docker API returned HTTP {status}: {message}")]
    Docker { status: http::StatusCode, message: String },
    #[error("invalid Docker response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("Docker protocol violation: {0}")]
    Protocol(String),
}

impl Error {
    pub(crate) fn docker(status: http::StatusCode, bytes: &[u8]) -> Self {
        let message = serde_json::from_slice::<hl_daemon::api::DockerError>(bytes)
            .map_or_else(|_| String::from_utf8_lossy(bytes).into_owned(), |error| error.message);
        Self::Docker { status, message }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
