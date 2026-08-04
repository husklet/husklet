//! Consumer-owned provider byte-stream transport.

use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    Interrupted,
    WouldBlock,
    Canceled,
    Closed,
    Failed,
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "provider transport {self:?}")
    }
}

impl std::error::Error for TransportError {}

/// Narrow full-duplex byte-stream capability consumed by [`crate::Provider`].
pub trait ProviderTransport: Send + Sync + 'static {
    fn read(&self, output: &mut [u8]) -> Result<usize, TransportError>;
    fn write(&self, input: &[u8]) -> Result<usize, TransportError>;
    /// Waits until read state changes or returns a terminal error.
    ///
    /// Implementations must not return immediately while still blocked.
    fn wait_readable(&self) -> Result<(), TransportError>;
    /// Waits until write state changes or returns a terminal error.
    ///
    /// Implementations must not return immediately while still blocked.
    fn wait_writable(&self) -> Result<(), TransportError>;
    fn shutdown(&self);
}
