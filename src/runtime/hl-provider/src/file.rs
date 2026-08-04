//! Provider-backed regular-file open descriptions.

pub(crate) mod model;
mod protocol;
mod readiness;

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver, ReadinessSubscription, StatusFlags,
};

use self::protocol::FileProtocol;
use self::readiness::{FileEventObserver, FileReadiness};
use crate::{
    FileAccess, FileError, FileMetadata, FileRebind, FileSnapshot, Handle, HandleKind, HandleNamespace, Provider,
    ProviderError, ProviderSubscription, ProviderTransport, RemoteId, SubscriptionIdentity,
};

const SEQUENTIAL: u64 = u64::MAX;
const DEFAULT_IO_LIMIT: usize = 65_536;
pub(crate) const PATH_MAXIMUM: usize = 4096;

impl FileAccess {
    const fn wire(self) -> u8 {
        match self {
            Self::Read => 1,
            Self::Write => 2,
            Self::ReadWrite => 3,
        }
    }

    const fn status(self) -> StatusFlags {
        let bits = match self {
            Self::Read => 0,
            Self::Write => 1,
            Self::ReadWrite => 2,
        };
        StatusFlags::from_bits(bits)
    }

    const fn readable(self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    const fn writable(self) -> bool {
        matches!(self, Self::Write | Self::ReadWrite)
    }
}

struct FileState {
    handle: Option<Handle>,
    offset: u64,
    service: u64,
}

pub struct ProjectedFile<T: ProviderTransport> {
    client: Arc<Provider<T>>,
    handles: Arc<HandleNamespace>,
    state: Mutex<FileState>,
    sequence: Mutex<()>,
    access: FileAccess,
    status: AtomicU32,
    readiness: Arc<FileReadiness>,
    subscription: Mutex<Option<ProviderSubscription>>,
    retired: AtomicBool,
    identity_namespace: u64,
    path: Arc<[u8]>,
    io_limit: usize,
    subscription_generation: u32,
}

impl<T: ProviderTransport> fmt::Debug for ProjectedFile<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectedFile")
            .field("access", &self.access)
            .field("status", &self.status.load(Ordering::Acquire))
            .field("retired", &self.retired.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

pub struct ProjectedFiles<T: ProviderTransport> {
    client: Arc<Provider<T>>,
    handles: Arc<HandleNamespace>,
    identity_namespace: u64,
    io_limit: usize,
    next_file_generation: AtomicU32,
}

impl<T: ProviderTransport> ProjectedFiles<T> {
    pub fn new(client: Arc<Provider<T>>, handle_capacity: usize, identity_namespace: u64) -> Result<Self, FileError> {
        Ok(Self {
            client,
            handles: Arc::new(HandleNamespace::new(handle_capacity)?),
            identity_namespace,
            io_limit: DEFAULT_IO_LIMIT,
            next_file_generation: AtomicU32::new(1),
        })
    }

    pub fn open_service(
        &self,
        service: u64,
        access: FileAccess,
        path: &[u8],
    ) -> Result<Arc<ProjectedFile<T>>, FileError> {
        if path.len() > PATH_MAXIMUM {
            return Err(FileError::InvalidArgument);
        }
        let reservation = self.handles.reserve(HandleKind::File)?;
        let reply = self.client.request(&FileProtocol::open(service, access.wire()))?;
        let remote = FileProtocol::open_reply(reply)?;
        let handle = match reservation.publish(remote) {
            Ok(handle) => handle,
            Err(error) => {
                self.close_unbound(remote);
                return Err(error.into());
            }
        };
        self.file(handle, service, access, access.status(), 0, Readiness::default(), path)
    }

    pub fn rebind(&self, value: FileRebind) -> Result<Arc<ProjectedFile<T>>, (FileError, FileRebind)> {
        let FileRebind { snapshot, capability } = value;
        if snapshot.identity_namespace != self.identity_namespace || snapshot.path.len() > PATH_MAXIMUM {
            return Err((FileError::InvalidArgument, FileRebind { snapshot, capability }));
        }
        let handle = match self.handles.accept(capability) {
            Ok(handle) => handle,
            Err((error, capability)) => {
                return Err((error.into(), FileRebind { snapshot, capability }));
            }
        };
        match self.file(
            handle,
            snapshot.service,
            snapshot.access,
            snapshot.status,
            snapshot.offset,
            snapshot.readiness,
            &snapshot.path,
        ) {
            Ok(file) => Ok(file),
            Err(error) => {
                let capability = self.handles.transfer(handle).unwrap();
                Err((error, FileRebind { snapshot, capability }))
            }
        }
    }

    fn file(
        &self,
        handle: Handle,
        service: u64,
        access: FileAccess,
        status: StatusFlags,
        offset: u64,
        readiness: Readiness,
        path: &[u8],
    ) -> Result<Arc<ProjectedFile<T>>, FileError> {
        if path.len() > PATH_MAXIMUM {
            return Err(FileError::InvalidArgument);
        }
        let path = Arc::from(path);
        Ok(Arc::new(ProjectedFile {
            client: Arc::clone(&self.client),
            handles: Arc::clone(&self.handles),
            state: Mutex::new(FileState {
                handle: Some(handle),
                offset,
                service,
            }),
            sequence: Mutex::new(()),
            access,
            status: AtomicU32::new(status.bits()),
            readiness: Arc::new(FileReadiness::new(readiness)),
            subscription: Mutex::new(None),
            retired: AtomicBool::new(false),
            identity_namespace: self.identity_namespace,
            path,
            io_limit: self.io_limit,
            subscription_generation: self.next_file_generation.fetch_add(1, Ordering::Relaxed).max(1),
        }))
    }

    fn close_unbound(&self, remote: RemoteId) {
        let _ = self
            .client
            .request(&FileProtocol::close(remote))
            .and_then(|reply| FileProtocol::close_reply(reply).map_err(|_| ProviderError::UnexpectedFrame));
    }
}

impl<T: ProviderTransport> ProjectedFile<T> {
    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, FileError> {
        if !self.access.readable() {
            return Err(FileError::Linux(9));
        }
        if output.len() > self.io_limit {
            return Err(FileError::PayloadTooLarge);
        }
        let remote = self.remote()?;
        let reply = self.client.request(&FileProtocol::read(remote, offset, output.len()))?;
        let bytes = FileProtocol::read_reply(reply, output.len())?;
        output[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }

    pub fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, FileError> {
        if !self.access.writable() {
            return Err(FileError::Linux(9));
        }
        if input.len() > self.io_limit {
            return Err(FileError::PayloadTooLarge);
        }
        let remote = self.remote()?;
        let reply = self.client.request(&FileProtocol::write(remote, offset, input))?;
        FileProtocol::write_reply(reply, input.len())
    }

    pub fn seek(&self, offset: i64, whence: u8) -> Result<u64, FileError> {
        if whence > 2 {
            return Err(FileError::InvalidArgument);
        }
        let _sequence = self.sequence.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let remote = self.remote()?;
        let reply = self.client.request(&FileProtocol::seek(remote, offset, whence))?;
        let position = FileProtocol::seek_reply(reply)?;
        self.state().offset = position;
        Ok(position)
    }

    pub fn metadata(&self) -> Result<FileMetadata, FileError> {
        let remote = self.remote()?;
        let reply = self.client.request(&FileProtocol::stat(remote))?;
        FileProtocol::stat_reply(reply, remote)
    }

    pub fn refresh_readiness(&self, interests: Readiness) -> Result<Readiness, FileError> {
        let remote = self.remote()?;
        let wire_interests = FileReadiness::to_wire(interests);
        let reply = self.client.request(&FileProtocol::poll(remote, wire_interests))?;
        let readiness = FileReadiness::from_wire(FileProtocol::poll_reply(reply)?);
        self.readiness.update(readiness, 0);
        Ok(Readiness::from_bits(
            readiness.bits() & (interests.bits() | Readiness::ERROR | Readiness::HANGUP),
        ))
    }

    #[must_use]
    pub fn snapshot(&self) -> Result<FileSnapshot, FileError> {
        let state = self.state();
        let handle = state.handle.ok_or(FileError::Retired)?;
        let remote = self.handles.resolve(handle, HandleKind::File)?;
        Ok(FileSnapshot {
            remote,
            service: state.service,
            access: self.access,
            status: StatusFlags::from_bits(self.status.load(Ordering::Acquire)),
            offset: state.offset,
            readiness: self.readiness.current(),
            identity_namespace: self.identity_namespace,
            path: self.path.to_vec(),
        })
    }

    pub fn release_for_rebind(&self) -> Result<FileRebind, FileError> {
        let snapshot = self.snapshot()?;
        let handle = self.state().handle.take().ok_or(FileError::Retired)?;
        let capability = self.handles.transfer(handle)?;
        self.retired.store(true, Ordering::Release);
        self.subscription
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.readiness.close();
        Ok(FileRebind { snapshot, capability })
    }

    fn remote(&self) -> Result<RemoteId, FileError> {
        if self.retired.load(Ordering::Acquire) {
            return Err(FileError::Retired);
        }
        let handle = self.state().handle.ok_or(FileError::Retired)?;
        Ok(self.handles.resolve(handle, HandleKind::File)?)
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FileState> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn object_error(error: FileError) -> ObjectError {
        match error {
            FileError::Linux(4) => ObjectError::Interrupted,
            FileError::Linux(9) => ObjectError::BadDescriptor,
            FileError::Linux(11) => ObjectError::WouldBlock,
            FileError::Linux(22) => ObjectError::InvalidArgument,
            FileError::Linux(32) => ObjectError::BrokenPipe,
            FileError::Linux(125) => ObjectError::Canceled,
            FileError::Provider(ProviderError::Canceled) => ObjectError::Canceled,
            FileError::Provider(ProviderError::Capacity) => ObjectError::ResourceLimit,
            FileError::Retired => ObjectError::Retired,
            _ => ObjectError::Io,
        }
    }
}

impl<T: ProviderTransport> OpenFileDescription for ProjectedFile<T> {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        let _sequence = self.sequence.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = self.read_at(SEQUENTIAL, output).map_err(Self::object_error)?;
        let mut state = self.state();
        state.offset = state.offset.saturating_add(count as u64);
        Ok(count)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        let _sequence = self.sequence.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = self.write_at(SEQUENTIAL, input).map_err(Self::object_error)?;
        let mut state = self.state();
        state.offset = state.offset.saturating_add(count as u64);
        Ok(count)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        if flags.bits() & StatusFlags::ACCESS_MODE_MASK != self.access.status().bits() {
            return Err(ObjectError::InvalidArgument);
        }
        self.status.store(flags.bits(), Ordering::Release);
        Ok(())
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        self.refresh_readiness(interests)
            .unwrap_or_else(|_| Readiness::from_bits(Readiness::ERROR | Readiness::HANGUP))
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        let mut remote_subscription = self
            .subscription
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if remote_subscription.is_none() {
            let remote = self.remote().map_err(Self::object_error)?;
            let identity =
                SubscriptionIdentity::new(self.identity_namespace ^ remote.get(), self.subscription_generation)
                    .map_err(|_| ObjectError::InvalidArgument)?;
            let payload = FileProtocol::poll(remote, 7);
            let target = Arc::new(FileEventObserver {
                readiness: Arc::clone(&self.readiness),
            });
            *remote_subscription = Some(
                self.client
                    .subscribe(identity, &payload, target)
                    .map_err(|error| Self::object_error(error.into()))?,
            );
        }
        drop(remote_subscription);
        self.readiness.subscribe(observer)
    }

    fn retire(&self) {
        self.retired.store(true, Ordering::Release);
        self.subscription
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        self.readiness.close();
    }

    fn close(&self) {
        let handle = self.state().handle.take();
        let Some(handle) = handle else {
            return;
        };
        let Ok(Some(close)) = self.handles.close(handle) else {
            return;
        };
        let _ = self
            .client
            .request(&FileProtocol::close(close.remote()))
            .and_then(|reply| FileProtocol::close_reply(reply).map_err(|_| ProviderError::UnexpectedFrame));
    }
}

impl<T: ProviderTransport> Drop for ProjectedFile<T> {
    fn drop(&mut self) {
        <Self as OpenFileDescription>::close(self);
    }
}
