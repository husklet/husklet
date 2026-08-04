//! Safe ownership boundary for platform clock, file, directory, and range adapters.

use std::sync::Arc;

mod authority;
#[cfg(target_os = "linux")]
mod confinement;
mod descriptor;
mod event;
mod executor;
mod fixture;
mod fork_wire;
#[cfg(target_os = "linux")]
pub mod launcher;
mod lock;
mod process;
mod resource;
mod signal;
mod socket;
mod termination;
mod watch;

#[cfg(target_os = "macos")]
pub use crate::ffi::DarwinHost;
#[cfg(target_os = "linux")]
pub use crate::ffi::{
    AddressSpaceAdapter, GuestExecutor, LinuxHost, MappingHostAdapter, MemoryError, Reservation, VirtualMemory,
};
#[cfg(target_os = "linux")]
pub use authority::NetworkClient;
pub use authority::{AuthorityAccess, AuthorityChannel, ProjectionError};
#[cfg(unix)]
pub use authority::{AuthorityHealth, AuthorityWorker, Child, ProcessAuthority};
#[cfg(target_os = "linux")]
pub use confinement::HostConfinement;
pub use descriptor::{
    Descriptor, DescriptorIdentity, DescriptorInstall, DescriptorSyscalls, PrivateDescriptorAllocator,
    ReceivedDescriptors,
};
pub use event::{
    EventCounter, EventInterest, EventMode, EventReady, EventSyscalls, GenerationToken, PollSet, PollSource, Timer,
    TimerSetting,
};
#[allow(unused_imports)]
pub(crate) use executor::{
    BorrowedSource as NativeSource, Executor as NativeExecutor, Exit as NativeExit,
    HostFaultOwner, HostFaultView, InterruptToken, direct_literal_interval,
};
#[cfg(target_os = "linux")]
pub(crate) use executor::NativeFaultOwner;
pub use fixture::ChildFixture;
pub use fork_wire::{AttachmentFrame, ChildChannel, ForkFrame, ForkWireError, ForkWireSyscalls, PeerCredentials};
pub use lock::FileLock;
pub use process::{
    ChildExit, FileAction, HostDescriptor, ProcessGroup, ProcessHandle, ProcessId, ProcessSignal, ProcessSyscalls,
    SpawnRequest,
};
pub use resource::{HostResourceContext, HostResourceLease};
pub use signal::{PreviousSignalMask, Signal, SignalInfo, SignalMask, SignalSource, SignalSyscalls, ThreadSignalMask};
pub use socket::{ShutdownDirection, Socket, SocketAddress, SocketDomain, SocketOption, SocketSyscalls, SocketType};
pub use termination::TerminationSignals;
pub use watch::{FileWatch, WatchEvent, WatchInterest, WatchSyscalls, WatchToken};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostError {
    Interrupted,
    WouldBlock,
    Invalid,
    Denied,
    NotFound,
    Exists,
    Exhausted,
    Unsupported,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockKind {
    Monotonic,
    Realtime,
    RawMonotonic,
    ProcessCpu,
    ThreadCpu,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileMetadata {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub permissions: u16,
    pub links: u64,
    pub modified_ns: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    pub cookie: u64,
    pub inode: u64,
    pub kind: DirectoryEntryKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryEntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Protection(u8);

impl Protection {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const EXECUTE: Self = Self(4);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[must_use]
    pub const fn executable(self) -> bool {
        self.0 & Self::EXECUTE.0 != 0
    }

    #[must_use]
    pub const fn writable(self) -> bool {
        self.0 & Self::WRITE.0 != 0
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn native(self) -> Result<i32, HostError> {
        if self.writable() && self.executable() {
            return Err(HostError::Denied);
        }
        let mut native = 0;
        if self.0 & Self::READ.0 != 0 {
            native |= 1;
        }
        if self.writable() {
            native |= 2;
        }
        if self.executable() {
            native |= 4;
        }
        Ok(native)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileHandle(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MappingHandle(u64);

pub trait HostSyscalls: Send + Sync {
    fn clock_ns(&self, kind: ClockKind) -> Result<u64, HostError>;
    fn close_file(&self, file: u64);
    fn read(&self, file: u64, output: &mut [u8]) -> Result<usize, HostError>;
    fn write(&self, file: u64, input: &[u8]) -> Result<usize, HostError>;
    fn read_at(&self, file: u64, offset: u64, output: &mut [u8]) -> Result<usize, HostError>;
    fn write_at(&self, file: u64, offset: u64, input: &[u8]) -> Result<usize, HostError>;
    fn metadata(&self, file: u64) -> Result<FileMetadata, HostError>;
    fn directory_next(
        &self,
        file: u64,
        cookie: u64,
        name: &mut [u8],
    ) -> Result<Option<(DirectoryEntry, usize)>, HostError>;
    fn page_size(&self) -> Result<usize, HostError>;
    fn map(&self, size: usize, protection: Protection) -> Result<u64, HostError>;
    fn protect(&self, mapping: u64, size: usize, protection: Protection) -> Result<(), HostError>;
    fn unmap(&self, mapping: u64, size: usize) -> Result<(), HostError>;
}

pub struct HostClock<S> {
    syscalls: Arc<S>,
}

impl<S: HostSyscalls> HostClock<S> {
    #[must_use]
    pub fn new(syscalls: Arc<S>) -> Self {
        Self { syscalls }
    }

    pub fn now(&self, kind: ClockKind) -> Result<u64, HostError> {
        self.syscalls.clock_ns(kind)
    }
}

pub struct OwnedFile<S: HostSyscalls> {
    syscalls: Arc<S>,
    file: Option<FileHandle>,
}

impl<S: HostSyscalls> OwnedFile<S> {
    #[must_use]
    pub fn from_host_handle(syscalls: Arc<S>, handle: u64) -> Self {
        Self {
            syscalls,
            file: Some(FileHandle(handle)),
        }
    }

    pub fn read(&self, output: &mut [u8]) -> Result<usize, HostError> {
        self.syscalls.read(self.host_handle(), output)
    }

    pub fn write(&self, input: &[u8]) -> Result<usize, HostError> {
        self.syscalls.write(self.host_handle(), input)
    }

    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, HostError> {
        self.syscalls.read_at(self.host_handle(), offset, output)
    }

    pub fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, HostError> {
        self.syscalls.write_at(self.host_handle(), offset, input)
    }

    pub fn metadata(&self) -> Result<FileMetadata, HostError> {
        self.syscalls.metadata(self.host_handle())
    }

    pub fn next(&self, cookie: u64, name: &mut [u8]) -> Result<Option<(DirectoryEntry, usize)>, HostError> {
        self.syscalls.directory_next(self.host_handle(), cookie, name)
    }

    pub(crate) fn host_handle(&self) -> u64 {
        self.file.expect("owned file is live").0
    }
}

impl<S: HostSyscalls> Drop for OwnedFile<S> {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            self.syscalls.close_file(file.0);
        }
    }
}

pub struct OwnedMapping<S: HostSyscalls> {
    syscalls: Arc<S>,
    mapping: Option<MappingHandle>,
    size: usize,
}

impl<S: HostSyscalls> OwnedMapping<S> {
    pub fn allocate(syscalls: Arc<S>, size: usize, protection: Protection) -> Result<Self, HostError> {
        let page = syscalls.page_size()?;
        if size == 0 || !page.is_power_of_two() || size % page != 0 {
            return Err(HostError::Invalid);
        }
        let mapping = syscalls.map(size, protection)?;
        Ok(Self {
            syscalls,
            mapping: Some(MappingHandle(mapping)),
            size,
        })
    }

    pub fn protect(&self, protection: Protection) -> Result<(), HostError> {
        self.syscalls.protect(self.handle(), self.size, protection)
    }

    fn handle(&self) -> u64 {
        self.mapping.expect("owned mapping is live").0
    }
}

impl<S: HostSyscalls> Drop for OwnedMapping<S> {
    fn drop(&mut self) {
        if let Some(mapping) = self.mapping.take() {
            let _ = self.syscalls.unmap(mapping.0, self.size);
        }
    }
}

pub struct JitCapability<S: HostSyscalls> {
    mapping: OwnedMapping<S>,
}

impl<S: HostSyscalls> JitCapability<S> {
    pub fn allocate(syscalls: Arc<S>, size: usize, authorization: JitAuthorization) -> Result<Self, HostError> {
        if !authorization.available {
            return Err(HostError::Unsupported);
        }
        Ok(Self {
            mapping: OwnedMapping::allocate(syscalls, size, Protection::READ.union(Protection::WRITE))?,
        })
    }

    pub fn publish(&self) -> Result<(), HostError> {
        self.mapping.protect(Protection::READ.union(Protection::EXECUTE))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JitAuthorization {
    available: bool,
}

impl JitAuthorization {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self { available: false }
    }

    #[cfg(target_os = "macos")]
    pub(crate) const fn verified() -> Self {
        Self { available: true }
    }
}

#[cfg(test)]
#[path = "host_test.rs"]
mod tests;
