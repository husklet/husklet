//! Descriptor-backed filesystem syscalls and consumer-owned pipe ports.

mod cancellation;
mod context;
mod directory;
mod errno;
mod fcntl;
mod ioctl;
mod job;
mod memfd;
mod mutation;
mod open;
mod path;
mod pipe;
mod position;
mod positional;
mod proc;
mod read;
mod splice;
mod statfs;
mod syscalls;
mod vector;
mod xattr;

pub use cancellation::RuntimePipeCancellation;
pub(crate) use errno::{FileErrno, FilesystemErrno};
pub use syscalls::RuntimeFilesystemSyscalls;
pub use vector::{VectorDirection, VectorError, VectorPosition, VectorRequest, VectorTerminal};

/// Signal capability consumed by pipe writes after an `EPIPE` result.
pub trait PipeSignalPort: Send + Sync {
    fn queue_sigpipe(&self) -> Result<(), ()>;
}

/// Process file-size limit and signal capability consumed by regular-file mutations.
pub trait FileSizeLimitPort: Send + Sync {
    fn soft_limit(&self) -> Result<u64, ()>;
    fn queue_sigxfsz(&self) -> Result<(), ()>;
}

/// Owner-targeted signal capability consumed by signal-driven OFD readiness.
pub trait AsyncSignalPort: Send + Sync {
    fn deliver(&self, source: hl_descriptor::SignalSource) -> Result<(), ()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DnotifyError {
    Invalid,
    NotDirectory,
    Failed,
}

/// Directory mutation subscription capability consumed by `F_NOTIFY`.
pub trait DnotifyPort: Send + Sync {
    fn arm(
        &self,
        lease: &hl_descriptor::OperationLease,
        mask: u32,
        signal: u8,
    ) -> Result<Box<dyn hl_descriptor::ReadinessSubscription>, DnotifyError>;
}

/// Cancellation observation consumed by potentially blocking pipe operations.
pub trait PipeCancellationPort: Send + Sync {
    fn observation(&self) -> &dyn hl_descriptor::OperationCancellation;

    fn interrupted_result(&self) -> hl_linux::LinuxResult {
        hl_linux::LinuxResult::Error(hl_linux::Errno::EINTR)
    }
}

/// Publishes successful native-file extent changes into mapped-file ownership.
pub trait BackingChangePort: Send + Sync {
    fn changed(&self, change: hl_memory::BackingChange) -> Result<(), ()>;
}

/// Socket-specific ioctl capability consumed by the descriptor syscall router.
pub trait SocketIoctlPort: Send + Sync {
    /// Returns queued input bytes, or `None` when the description is not a socket.
    fn input_queue(&self, identity: hl_descriptor::DescriptionIdentity) -> Result<Option<u64>, ()>;
    /// Returns queued output bytes, or `None` when the description is not a socket.
    fn output_queue(&self, identity: hl_descriptor::DescriptionIdentity) -> Result<Option<u64>, ()>;
    fn at_urgent_mark(&self, identity: hl_descriptor::DescriptionIdentity) -> Result<Option<bool>, ()>;
    fn interfaces(
        &self,
        identity: hl_descriptor::DescriptionIdentity,
    ) -> Result<Option<Vec<hl_network::NamespaceInterface>>, ()>;
}

#[cfg(test)]
mod splice_test;
