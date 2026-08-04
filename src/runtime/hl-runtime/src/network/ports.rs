use std::sync::Arc;

use hl_linux::GuestSocketOption;
use hl_network::{
    AddressFamily, EgressRoute, NetworkResourceKey, NetworkSocketResource, SocketAddress, SocketHostIo, SocketProtocol,
    SocketType,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeNetworkError {
    Invalid,
    Unsupported,
    AddressInUse,
    AddressNotAvailable,
    AlreadyConnected,
    NotConnected,
    ConnectionAborted,
    ConnectionReset,
    DestinationRequired,
    MessageTooLarge,
    FamilyNotSupported,
    ProtocolNotSupported,
    TypeNotSupported,
    OptionNotSupported,
    WrongProtocol,
    NotSocket,
    HostUnreachable,
    NetworkUnreachable,
    NetworkDown,
    NetworkReset,
    ShutDown,
    BrokenPipe,
    OperationNotSupported,
    InProgress,
    AlreadyPending,
    WouldBlock,
    Interrupted,
    Refused,
    TimedOut,
    Permission,
    NoMemory,
    Failed,
}

pub struct CreatedSocket<T> {
    pub token: T,
    pub resource: NetworkResourceKey,
    pub binding: Arc<dyn NetworkSocketResource>,
}

pub struct AcceptedSocket<T> {
    pub token: T,
    pub resource: NetworkResourceKey,
    pub binding: Arc<dyn NetworkSocketResource>,
    pub local: SocketAddress,
    pub peer: SocketAddress,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReceivedDatagram {
    pub count: usize,
    /// Length of the complete record before destination truncation.
    pub full_length: usize,
    pub source: SocketAddress,
}

#[derive(Debug)]
pub enum HostControl<A> {
    Rights(Vec<A>),
    Credentials(hl_network::SenderCredentials),
    Unknown { level: i32, kind: i32, data: Vec<u8> },
}

#[derive(Debug)]
pub struct HostSend<A> {
    pub payload: Vec<u8>,
    pub address: Option<SocketAddress>,
    pub controls: Vec<HostControl<A>>,
    pub nonblocking: bool,
    /// Whether the socket preserves message boundaries.
    pub record: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostSendResult {
    pub count: usize,
    /// True only when the host accepted the attached rights in its one
    /// `sendmsg` operation. Stream sockets require positive payload progress;
    /// record sockets may accept rights with a zero-length record.
    pub rights_consumed: bool,
}

#[derive(Debug)]
pub struct HostReceive<A> {
    pub payload: Vec<u8>,
    pub full_length: usize,
    pub source: Option<SocketAddress>,
    pub controls: Vec<HostControl<A>>,
    pub payload_truncated: bool,
    pub control_truncated: bool,
}

/// Process-owned source sampled once for each Unix-domain message send.
pub trait SocketCredentials: Send + Sync {
    fn current(&self) -> Option<hl_network::SenderCredentials>;
}

impl SocketCredentials for hl_network::SenderCredentials {
    fn current(&self) -> Option<hl_network::SenderCredentials> {
        Some(*self)
    }
}

/// Host socket operations selected by the application composition root.
pub trait RuntimeNetworkHost: SocketHostIo {
    type Attachment: Send + Sync + 'static;

    fn create(
        &self,
        family: AddressFamily,
        socket_type: SocketType,
        protocol: SocketProtocol,
    ) -> Result<CreatedSocket<Self::Token>, RuntimeNetworkError>;
    fn bind(&self, token: Self::Token, address: SocketAddress) -> Result<SocketAddress, RuntimeNetworkError>;
    fn bind_route(&self, token: Self::Token, route: EgressRoute) -> Result<SocketAddress, RuntimeNetworkError> {
        self.bind(token, route.address)
    }
    fn prepare_connect(&self, token: Self::Token, address: SocketAddress) -> Result<(), RuntimeNetworkError>;
    fn prepare_connect_route(&self, token: Self::Token, route: EgressRoute) -> Result<(), RuntimeNetworkError> {
        self.prepare_connect(token, route.address)
    }
    fn listen(&self, token: Self::Token, backlog: u32) -> Result<(), RuntimeNetworkError>;
    fn accept(&self, token: Self::Token) -> Result<AcceptedSocket<Self::Token>, RuntimeNetworkError>;
    fn local_address(&self, token: Self::Token) -> Result<SocketAddress, RuntimeNetworkError>;
    fn peer_address(&self, token: Self::Token) -> Result<SocketAddress, RuntimeNetworkError>;
    fn send_to(
        &self,
        token: Self::Token,
        input: &[u8],
        address: SocketAddress,
        nonblocking: bool,
    ) -> Result<usize, RuntimeNetworkError>;
    fn send_to_route(
        &self,
        token: Self::Token,
        input: &[u8],
        route: EgressRoute,
        nonblocking: bool,
    ) -> Result<usize, RuntimeNetworkError> {
        self.send_to(token, input, route.address, nonblocking)
    }
    fn receive_from(
        &self,
        token: Self::Token,
        output: &mut [u8],
        nonblocking: bool,
        peek: bool,
    ) -> Result<ReceivedDatagram, RuntimeNetworkError>;
    fn send_urgent(&self, _token: Self::Token, _input: &[u8]) -> Result<usize, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn receive_urgent(
        &self,
        _token: Self::Token,
        _output: &mut [u8],
        _peek: bool,
    ) -> Result<usize, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn at_urgent_mark(&self, _token: Self::Token) -> Result<bool, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn send_message(
        &self,
        _token: Self::Token,
        _message: HostSend<Self::Attachment>,
    ) -> Result<HostSendResult, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn receive_message(
        &self,
        _token: Self::Token,
        _payload_limit: usize,
        _control_limit: usize,
        _nonblocking: bool,
        _peek: bool,
    ) -> Result<HostReceive<Self::Attachment>, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn shutdown(&self, token: Self::Token, read: bool, write: bool) -> Result<(), RuntimeNetworkError>;
    fn set_option(
        &self,
        token: Self::Token,
        level: i32,
        option: i32,
        value: GuestSocketOption,
    ) -> Result<(), RuntimeNetworkError>;
    fn get_option(&self, token: Self::Token, level: i32, option: i32)
    -> Result<GuestSocketOption, RuntimeNetworkError>;
    fn input_queue(&self, _token: Self::Token) -> Result<u64, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
    fn output_queue(&self, _token: Self::Token) -> Result<u64, RuntimeNetworkError> {
        Err(RuntimeNetworkError::Unsupported)
    }
}
