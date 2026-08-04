use hl_linux::{Errno, NetworkMarshalError};
use hl_network::SocketHostError;

use crate::RuntimeNetworkError;

pub(crate) struct SocketErrno;

impl SocketErrno {
    pub(crate) fn socket_host(error: SocketHostError) -> Errno {
        match error {
            SocketHostError::WouldBlock => Errno::EAGAIN,
            SocketHostError::Interrupted | SocketHostError::Canceled => Errno::EINTR,
            SocketHostError::BrokenPipe => Errno::EPIPE,
            SocketHostError::DestinationRequired => Errno::EDESTADDRREQ,
            SocketHostError::MessageTooLarge => Errno::EMSGSIZE,
            SocketHostError::ConnectionReset => Errno::ECONNRESET,
            SocketHostError::ConnectionAborted => Errno::ECONNABORTED,
            SocketHostError::NotConnected => Errno::ENOTCONN,
            SocketHostError::ShutDown => Errno::ESHUTDOWN,
            SocketHostError::HostUnreachable => Errno::EHOSTUNREACH,
            SocketHostError::NetworkUnreachable => Errno::ENETUNREACH,
            SocketHostError::NetworkDown => Errno::ENETDOWN,
            SocketHostError::NetworkReset => Errno::ENETRESET,
            SocketHostError::Io => Errno::EIO,
        }
    }

    pub(crate) fn marshal(error: NetworkMarshalError) -> Errno {
        match error {
            NetworkMarshalError::Marshal(error) => error.errno(),
            NetworkMarshalError::Control(_) => Errno::EINVAL,
            NetworkMarshalError::InvalidFamily => Errno::EAFNOSUPPORT,
            NetworkMarshalError::InvalidLength | NetworkMarshalError::InvalidFlags => Errno::EINVAL,
            NetworkMarshalError::TooManyVectors => Errno::EMSGSIZE,
        }
    }

    pub(crate) fn runtime(error: RuntimeNetworkError) -> Errno {
        match error {
            RuntimeNetworkError::Invalid => Errno::EINVAL,
            RuntimeNetworkError::Unsupported => Errno::ENOSYS,
            RuntimeNetworkError::AddressInUse => Errno::EADDRINUSE,
            RuntimeNetworkError::AddressNotAvailable => Errno::EADDRNOTAVAIL,
            RuntimeNetworkError::AlreadyConnected => Errno::EISCONN,
            RuntimeNetworkError::NotConnected => Errno::ENOTCONN,
            RuntimeNetworkError::ConnectionAborted => Errno::ECONNABORTED,
            RuntimeNetworkError::ConnectionReset => Errno::ECONNRESET,
            RuntimeNetworkError::DestinationRequired => Errno::EDESTADDRREQ,
            RuntimeNetworkError::MessageTooLarge => Errno::EMSGSIZE,
            RuntimeNetworkError::FamilyNotSupported => Errno::EAFNOSUPPORT,
            RuntimeNetworkError::ProtocolNotSupported => Errno::EPROTONOSUPPORT,
            RuntimeNetworkError::TypeNotSupported => Errno::ESOCKTNOSUPPORT,
            RuntimeNetworkError::OptionNotSupported => Errno::ENOPROTOOPT,
            RuntimeNetworkError::WrongProtocol => Errno::EPROTOTYPE,
            RuntimeNetworkError::NotSocket => Errno::ENOTSOCK,
            RuntimeNetworkError::HostUnreachable => Errno::EHOSTUNREACH,
            RuntimeNetworkError::NetworkUnreachable => Errno::ENETUNREACH,
            RuntimeNetworkError::NetworkDown => Errno::ENETDOWN,
            RuntimeNetworkError::NetworkReset => Errno::ENETRESET,
            RuntimeNetworkError::ShutDown => Errno::ESHUTDOWN,
            RuntimeNetworkError::BrokenPipe => Errno::EPIPE,
            RuntimeNetworkError::OperationNotSupported => Errno::EOPNOTSUPP,
            RuntimeNetworkError::InProgress => Errno::EINPROGRESS,
            RuntimeNetworkError::AlreadyPending => Errno::EALREADY,
            RuntimeNetworkError::WouldBlock => Errno::EAGAIN,
            RuntimeNetworkError::Interrupted => Errno::EINTR,
            RuntimeNetworkError::Refused => Errno::ECONNREFUSED,
            RuntimeNetworkError::TimedOut => Errno::ETIMEDOUT,
            RuntimeNetworkError::Permission => Errno::EACCES,
            RuntimeNetworkError::NoMemory => Errno::ENOMEM,
            RuntimeNetworkError::Failed => Errno::EIO,
        }
    }
}
