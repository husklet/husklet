use std::fmt::Debug;

use hl_descriptor::ReadinessObserver;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketHostError {
    WouldBlock,
    Interrupted,
    Canceled,
    BrokenPipe,
    DestinationRequired,
    MessageTooLarge,
    ConnectionReset,
    ConnectionAborted,
    NotConnected,
    ShutDown,
    HostUnreachable,
    NetworkUnreachable,
    NetworkDown,
    NetworkReset,
    Io,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketConnectError {
    InProgress,
    Already,
    Connected,
    Interrupted,
    Refused,
    TimedOut,
    Canceled,
    Io,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SocketConnectStatus {
    Idle,
    Pending,
    Connected,
    Failed(SocketConnectError),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SocketHostReadiness {
    pub readable: bool,
    pub priority: bool,
    pub read_hangup: bool,
    pub writable: bool,
    pub error: bool,
    pub hangup: bool,
}

pub trait SocketHostIo: Debug + Send + Sync + 'static {
    type Token: Copy + Debug + Send + Sync + 'static;

    fn read(&self, token: Self::Token, output: &mut [u8], nonblocking: bool) -> Result<usize, SocketHostError>;

    fn peek(&self, _token: Self::Token, _output: &mut [u8]) -> Result<usize, SocketHostError> {
        Err(SocketHostError::Io)
    }

    fn write(&self, token: Self::Token, input: &[u8], nonblocking: bool) -> Result<usize, SocketHostError>;

    fn readiness(&self, token: Self::Token) -> SocketHostReadiness;
    fn start_connect(&self, token: Self::Token, nonblocking: bool) -> SocketConnectStatus;
    fn poll_connect(&self, token: Self::Token) -> SocketConnectStatus;
    fn attach_readiness(&self, _token: Self::Token, _observer: std::sync::Weak<dyn ReadinessObserver>) {}
    fn detach_readiness(&self, _token: Self::Token) {}
    /// Wakes every operation parked on this token. Must be idempotent.
    fn cancel(&self, token: Self::Token);
    fn close(&self, token: Self::Token);
}
