use std::time::Duration;

/// Explicit latency bounds for one remote GPU transport connection.
///
/// Applications may choose tighter or looser bounds before constructing the sink. The reusable transport
/// never reads environment variables or applies product policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportConfig {
    connect_timeout: Duration,
    handshake_timeout: Duration,
    write_timeout: Duration,
    response_timeout: Duration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransportConfigError;

impl TransportConfig {
    pub fn new(
        connect_timeout: Duration,
        handshake_timeout: Duration,
        write_timeout: Duration,
        response_timeout: Duration,
    ) -> Result<Self, TransportConfigError> {
        if [
            connect_timeout,
            handshake_timeout,
            write_timeout,
            response_timeout,
        ]
        .contains(&Duration::ZERO)
        {
            return Err(TransportConfigError);
        }
        Ok(Self {
            connect_timeout,
            handshake_timeout,
            write_timeout,
            response_timeout,
        })
    }

    pub fn connect_timeout(self) -> Duration {
        self.connect_timeout
    }

    pub fn handshake_timeout(self) -> Duration {
        self.handshake_timeout
    }

    pub fn write_timeout(self) -> Duration {
        self.write_timeout
    }

    pub fn response_timeout(self) -> Duration {
        self.response_timeout
    }
}

impl std::fmt::Display for TransportConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("transport deadlines must be greater than zero")
    }
}

impl std::error::Error for TransportConfigError {}

impl Default for TransportConfig {
    fn default() -> Self {
        Self::new(
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(30),
        )
        .expect("default transport deadlines are non-zero")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_deadline_is_rejected() {
        assert_eq!(
            TransportConfig::new(
                Duration::ZERO,
                Duration::from_secs(1),
                Duration::from_secs(1),
                Duration::from_secs(1)
            ),
            Err(TransportConfigError)
        );
    }
}
