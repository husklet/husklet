//! Socket syscall integration, host ports, and cancellation-aware waiting.

mod accept;
mod data;
mod errno;
mod import;
mod ioctl;
mod message;
mod message_host;
pub(crate) mod netlink;
mod options;
mod ports;
mod syscalls;
mod transfer;
mod types;
mod wait;

pub use import::HostImport;
pub use ioctl::SocketIoctl;
pub use ports::{
    AcceptedSocket, CreatedSocket, HostControl, HostReceive, HostSend, HostSendResult, ReceivedDatagram,
    RuntimeNetworkError, RuntimeNetworkHost, SocketCredentials,
};
pub use syscalls::RuntimeNetworkSyscalls;
pub use transfer::{
    DescriptorTransfer, ImportedDescription, ImportedTransfer, PreparedTransfer, TransferCommitError,
    TransferPublication,
};

#[cfg(test)]
mod transfer_test;
pub use wait::{SafeNetworkWait, SocketWait};
