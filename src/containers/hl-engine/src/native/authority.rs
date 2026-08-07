//! Validated launch capability for a separate host authority.

use super::HostDescriptor;
mod channel;
pub use channel::{Access as AuthorityAccess, Channel as AuthorityChannel};
#[cfg(unix)]
mod child;
#[cfg(target_os = "linux")]
mod network;
#[cfg(target_os = "linux")]
pub use network::Client as NetworkClient;
#[cfg(unix)]
mod provider;
#[cfg(unix)]
mod tree;
#[cfg(unix)]
use crate::engine::{EngineError, Step as _};
#[cfg(unix)]
use crate::session::{FrameKind, Limits, Secret, Session, connect};
#[cfg(unix)]
pub use child::Child;
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use super::{ChildExit, FileAction, ProcessGroup, ProcessHandle, ProcessSignal, ProcessSyscalls, SpawnRequest};

#[cfg(unix)]
struct ProcessState<S: ProcessSyscalls> {
    pending: BTreeMap<i32, (UnixStream, UnixStream, UnixDatagram, ProcessHandle<S>)>,
    active: BTreeMap<u64, Supervised<S>>,
    next: u64,
}

#[cfg(unix)]
enum Supervised<S: ProcessSyscalls> {
    Running(ProcessHandle<S>),
    Exited(ChildExit),
}

#[cfg(unix)]
impl<S: ProcessSyscalls> Supervised<S> {
    fn healthy(&mut self) -> Result<bool, EngineError> {
        let Self::Running(handle) = self else { return Ok(false) };
        let Some(exit) = handle.wait().step("child_wait")? else {
            return Ok(true);
        };
        *self = Self::Exited(exit);
        Ok(false)
    }
}

/// Unix authority child supervisor. The worker receives only its socket endpoint.
#[cfg(unix)]
pub struct ProcessAuthority<S: ProcessSyscalls> {
    program: CString,
    arguments: Vec<CString>,
    projected: Option<std::fs::File>,
    projected_root: Option<std::fs::File>,
    projected_root_writable: bool,
    processes: Arc<S>,
    state: Mutex<ProcessState<S>>,
}

#[cfg(unix)]
pub struct AuthorityWorker {
    stream: UnixStream,
    session: Session,
    health: UnixStream,
    transfer: Option<UnixDatagram>,
    network_nonce: u64,
    network_capture: Option<u64>,
    network_restore: Option<(u64, [u8; 32])>,
}

#[cfg(unix)]
pub struct AuthorityHealth(UnixStream);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionError {
    Linux(i32),
    Session,
}

#[cfg(unix)]
impl AuthorityWorker {
    pub fn inherit(descriptor: i32, health: i32) -> Result<Self, EngineError> {
        let mut stream =
            crate::ffi::linux::InheritedStream::adopt(descriptor).step("session_adopt")?;
        let secret = Secret::receive(&mut stream).step("secret_recv")?;
        let session =
            connect(&mut stream, secret, Limits::new(4096, 8).unwrap()).step("session_connect")?;
        let health = crate::ffi::linux::InheritedStream::adopt(health).step("health_adopt")?;
        Ok(Self {
            stream,
            session,
            health,
            transfer: None,
            network_nonce: 1,
            network_capture: None,
            network_restore: None,
        })
    }

    pub fn enter<R>(&mut self, entry: impl FnOnce() -> R) -> Result<R, EngineError> {
        self.session
            .send(&mut self.stream, FrameKind::Ready, &[])
            .step("ready_send")?;
        let reply = self
            .session
            .receive(&mut self.stream)
            .step("ready_recv")?;
        if reply.kind != FrameKind::Ready || !reply.payload.is_empty() {
            return Err(EngineError::denied("ready_reply"));
        }
        Ok(entry())
    }

    pub fn ping(&mut self, payload: &[u8]) -> Result<Vec<u8>, EngineError> {
        self.session
            .send(&mut self.stream, FrameKind::Ping, payload)
            .step("ping_send")?;
        let reply = self
            .session
            .receive(&mut self.stream)
            .step("ping_recv")?;
        (reply.kind == FrameKind::Ping)
            .then_some(reply.payload)
            .step("ping_reply")
    }

    pub(super) fn provider(&mut self, payload: &[u8]) -> Result<Vec<u8>, ProjectionError> {
        self.session
            .send(&mut self.stream, FrameKind::Provider, payload)
            .map_err(|_| ProjectionError::Session)?;
        let reply = self
            .session
            .receive(&mut self.stream)
            .map_err(|_| ProjectionError::Session)?;
        (reply.kind == FrameKind::Provider)
            .then_some(reply.payload)
            .ok_or(ProjectionError::Session)
    }

    pub fn open_file(&mut self, service: u64) -> Result<u64, ProjectionError> {
        let request = hl_provider::FileWire::open(service, 1);
        let reply = self.provider(&request)?;
        hl_provider::FileWire::open_reply(&reply).map_err(ProjectionError::Linux)
    }

    pub fn read_file(&mut self, handle: u64, offset: u64, size: usize) -> Result<Vec<u8>, ProjectionError> {
        let request = hl_provider::FileWire::read(handle, offset, size).map_err(ProjectionError::Linux)?;
        let reply = self.provider(&request)?;
        hl_provider::FileWire::read_reply(&reply, size).map_err(ProjectionError::Linux)
    }

    pub fn file_info(&mut self, handle: u64) -> Result<hl_provider::FileInfo, ProjectionError> {
        let reply = self.provider(&hl_provider::FileWire::info(handle))?;
        hl_provider::FileWire::info_reply(&reply).map_err(ProjectionError::Linux)
    }

    pub fn close_file(&mut self, handle: u64) -> Result<(), ProjectionError> {
        let reply = self.provider(&hl_provider::FileWire::close(handle))?;
        hl_provider::FileWire::close_reply(&reply).map_err(ProjectionError::Linux)
    }

    pub fn close(&mut self) -> Result<(), EngineError> {
        self.session
            .send(&mut self.stream, FrameKind::Close, &[])
            .step("close_send")
    }

    pub fn health(&self) -> Result<AuthorityHealth, EngineError> {
        self.health
            .try_clone()
            .map(AuthorityHealth)
            .step("health_clone")
    }
}

#[cfg(unix)]
impl AuthorityHealth {
    pub fn monitor(&self, done: &AtomicBool, failure: impl FnOnce()) -> Result<(), EngineError> {
        crate::ffi::linux::InheritedStream::wait_closed(&self.0).step("health_wait")?;
        if done.load(Ordering::Acquire) {
            return Ok(());
        }
        failure();
        Err(EngineError::denied("health_closed"))
    }

    pub fn stop(&self) -> Result<(), EngineError> {
        self.0
            .shutdown(std::net::Shutdown::Both)
            .step("health_shutdown")
    }
}

#[cfg(unix)]
impl<S: ProcessSyscalls> ProcessAuthority<S> {
    pub fn new(program: &Path, processes: Arc<S>) -> Result<Self, EngineError> {
        use std::os::unix::ffi::OsStrExt;
        Ok(Self {
            program: CString::new(program.as_os_str().as_bytes()).step("program_encode")?,
            arguments: Vec::new(),
            projected: None,
            projected_root: None,
            projected_root_writable: false,
            processes,
            state: Mutex::new(ProcessState {
                pending: BTreeMap::new(),
                active: BTreeMap::new(),
                next: 1,
            }),
        })
    }

    pub fn projected(program: &Path, file: &Path, processes: Arc<S>) -> Result<Self, EngineError> {
        let mut authority = Self::new(program, processes)?;
        if file.as_os_str().len() > 4096 {
            return Err(EngineError::denied("projected_program"));
        }
        authority.projected = Some(std::fs::File::open(file).step("projected_open")?);
        Ok(authority)
    }

    pub fn projected_root(program: &Path, root: &Path, image: &Path, processes: Arc<S>) -> Result<Self, EngineError> {
        let mut authority = Self::projected(program, image, processes)?;
        let root = std::fs::File::open(root).step("root_open")?;
        if !root.metadata().step("root_metadata")?.is_dir() {
            return Err(EngineError::denied("root_not_directory"));
        }
        authority.projected_root = Some(root);
        Ok(authority)
    }

    pub fn projected_root_writable(
        program: &Path,
        root: &Path,
        image: &Path,
        processes: Arc<S>,
    ) -> Result<Self, EngineError> {
        let mut authority = Self::projected_root(program, root, image, processes)?;
        authority.projected_root_writable = true;
        Ok(authority)
    }

    fn spawn(
        &self,
        session: &UnixStream,
        bootstrap: &UnixStream,
        health: &UnixStream,
        transfer: &UnixDatagram,
    ) -> Result<ProcessHandle<S>, EngineError> {
        let session_fd = session.as_raw_fd();
        let bootstrap_fd = bootstrap.as_raw_fd();
        let health_fd = health.as_raw_fd();
        let transfer_fd = transfer.as_raw_fd();
        let mut arguments = vec![
            CString::new("--session-fd").unwrap(),
            CString::new(session_fd.to_string()).unwrap(),
            CString::new("--bootstrap-fd").unwrap(),
            CString::new(bootstrap_fd.to_string()).unwrap(),
            CString::new("--health-fd").unwrap(),
            CString::new(health_fd.to_string()).unwrap(),
            CString::new("--transfer-fd").unwrap(),
            CString::new(transfer_fd.to_string()).unwrap(),
        ];
        let projected = self.projected.as_ref().map(AsRawFd::as_raw_fd);
        let projected_root = self.projected_root.as_ref().map(AsRawFd::as_raw_fd);
        if let Some(descriptor) = projected {
            arguments.push(CString::new("--project-fd").unwrap());
            arguments.push(CString::new(descriptor.to_string()).unwrap());
        }
        if let Some(descriptor) = projected_root {
            arguments.push(CString::new("--root-fd").unwrap());
            arguments.push(CString::new(descriptor.to_string()).unwrap());
            if self.projected_root_writable {
                arguments.push(CString::new("--root-write").unwrap());
            }
        }
        arguments.extend(self.arguments.iter().cloned());
        let mut file_actions = vec![
            FileAction::Inherit(HostDescriptor::new(session_fd).step("session_inherit")?),
            FileAction::Inherit(HostDescriptor::new(bootstrap_fd).step("bootstrap_inherit")?),
            FileAction::Inherit(HostDescriptor::new(health_fd).step("health_inherit")?),
            FileAction::Inherit(HostDescriptor::new(transfer_fd).step("transfer_inherit")?),
        ];
        if let Some(descriptor) = projected {
            file_actions.push(FileAction::Inherit(
                HostDescriptor::new(descriptor).step("projected_inherit")?,
            ));
        }
        if let Some(descriptor) = projected_root {
            file_actions.push(FileAction::Inherit(
                HostDescriptor::new(descriptor).step("root_inherit")?,
            ));
        }
        let request = SpawnRequest {
            program: self.program.clone(),
            arguments,
            environment: Vec::new(),
            process_group: ProcessGroup::New,
            file_actions,
        };
        ProcessHandle::spawn(Arc::clone(&self.processes), &request).step("spawn")
    }

    pub fn healthy(&self, token: u64) -> Result<bool, EngineError> {
        let mut state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
        let process = state.active.get_mut(&token).step("active_lookup")?;
        process.healthy()
    }

    pub fn worker(&self, channel: AuthorityChannel) -> Result<AuthorityWorker, EngineError> {
        let state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
        let pending = state
            .pending
            .get(&channel.descriptor().raw())
            .step("pending_lookup")?;
        let stream = &pending.0;
        let health = pending.1.try_clone().step("health_stream_clone")?;
        let transfer = pending.2.try_clone().step("transfer_clone")?;
        let mut stream = stream.try_clone().step("session_clone")?;
        let secret = Secret::receive(&mut stream).step("worker_secret_recv")?;
        let session = connect(
            &mut stream,
            secret,
            Limits::new(channel.frame_limit(), channel.inflight_limit()).unwrap(),
        )
        .step("worker_connect")?;
        Ok(AuthorityWorker {
            stream,
            session,
            health,
            transfer: Some(transfer),
            network_nonce: 1,
            network_capture: None,
            network_restore: None,
        })
    }

    pub fn reap(&self, token: u64) -> Result<ChildExit, EngineError> {
        let process = self
            .state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .active
            .remove(&token)
            .step("reap_lookup")?;
        match process {
            Supervised::Exited(exit) => Ok(exit),
            Supervised::Running(handle) => handle.wait_blocking().step("reap_wait"),
        }
    }

    pub fn terminate(&self, token: u64) -> Result<ChildExit, EngineError> {
        let process = self
            .state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .active
            .remove(&token)
            .step("terminate_lookup")?;
        match process {
            Supervised::Exited(exit) => Ok(exit),
            Supervised::Running(handle) => {
                handle
                    .signal(ProcessSignal::Kill)
                    .step("terminate_signal")?;
                handle.wait_blocking().step("terminate_wait")
            }
        }
    }

    /// Idempotently removes, terminates, and reaps one supervised authority.
    pub fn cleanup(&self, token: u64) -> Result<Option<ChildExit>, EngineError> {
        let process = self
            .state
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .active
            .remove(&token);
        let Some(process) = process else { return Ok(None) };
        match process {
            Supervised::Exited(exit) => Ok(Some(exit)),
            Supervised::Running(handle) => {
                let _ = handle.signal(ProcessSignal::Kill);
                handle
                    .wait_blocking()
                    .map(Some)
                    .step("cleanup_wait")
            }
        }
    }
}

#[cfg(unix)]
impl<S: ProcessSyscalls + 'static> AuthorityAccess for ProcessAuthority<S> {
    fn open(&self, _: [u64; 2]) -> Result<AuthorityChannel, EngineError> {
        let mut state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
        if state.pending.len() + state.active.len() >= 64 {
            return Err(EngineError::denied("program_missing"));
        }
        let token = state.next;
        state.next = state.next.checked_add(1).step("token_overflow")?;
        let (worker, mut authority) = UnixStream::pair().step("session_pair")?;
        let (mut bootstrap_parent, bootstrap_child) = UnixStream::pair().step("bootstrap_pair")?;
        let (health_worker, health_authority) = UnixStream::pair().step("health_pair")?;
        let (transfer_worker, transfer_authority) = UnixDatagram::pair().step("transfer_pair")?;
        let (worker_secret, authority_secret) = Secret::pair().step("secret_pipe")?;
        worker_secret
            .send(&mut authority)
            .step("worker_secret_send")?;
        authority_secret
            .send(&mut bootstrap_parent)
            .step("authority_secret_send")?;
        let process = self.spawn(&authority, &bootstrap_child, &health_authority, &transfer_authority)?;
        let descriptor = worker.as_raw_fd();
        let health_descriptor = health_worker.as_raw_fd();
        let transfer_descriptor = transfer_worker.as_raw_fd();
        state
            .pending
            .insert(descriptor, (worker, health_worker, transfer_worker, process));
        AuthorityChannel::new(
            HostDescriptor::new(descriptor).step("session_channel")?,
            HostDescriptor::new(health_descriptor).step("health_channel")?,
            HostDescriptor::new(transfer_descriptor).step("transfer_channel")?,
            [token, 1],
            4096,
            8,
        )
    }

    fn commit(&self, channel: AuthorityChannel) -> Result<(), EngineError> {
        let mut state = self.state.lock().map_err(|_| EngineError::Synchronization)?;
        let (_, _, _, process) = state
            .pending
            .remove(&channel.descriptor().raw())
            .step("commit_lookup")?;
        state.active.insert(channel.session()[0], Supervised::Running(process));
        Ok(())
    }

    fn rollback(&self, channel: AuthorityChannel) {
        let pending = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending
            .remove(&channel.descriptor().raw());
        if let Some((_, _, _, process)) = pending {
            let _ = process.signal(ProcessSignal::Kill);
            let _ = process.wait_blocking();
        }
    }
}

#[cfg(unix)]
impl<S: ProcessSyscalls> Drop for ProcessAuthority<S> {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap_or_else(|error| error.into_inner());
        for (_, _, _, process) in state.pending.values() {
            let _ = process.signal(ProcessSignal::Kill);
        }
        for process in state.active.values() {
            if let Supervised::Running(process) = process {
                let _ = process.signal(ProcessSignal::Kill);
            }
        }
        for (_, _, _, process) in state.pending.values() {
            let _ = process.wait_blocking();
        }
        for process in state.active.values() {
            if let Supervised::Running(process) = process {
                let _ = process.wait_blocking();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_channel() {
        let descriptor = HostDescriptor::new(7).unwrap();
        assert_eq!(
            AuthorityChannel::new(descriptor, descriptor, descriptor, [0, 0], 4096, 8),
            Err(EngineError::AuthorityFailed)
        );
        assert_eq!(
            AuthorityChannel::new(descriptor, descriptor, descriptor, [1, 2], channel::FRAME_LIMIT + 1, 8),
            Err(EngineError::AuthorityFailed)
        );
        assert_eq!(
            AuthorityChannel::new(descriptor, descriptor, descriptor, [1, 2], 4096, 0),
            Err(EngineError::AuthorityFailed)
        );
    }

    #[test]
    fn valid_channel() {
        let descriptor = HostDescriptor::new(7).unwrap();
        let channel = AuthorityChannel::new(descriptor, descriptor, descriptor, [1, 2], 4096, 8).unwrap();
        assert_eq!(channel.descriptor(), descriptor);
        assert_eq!(channel.session(), [1, 2]);
        assert_eq!(channel.frame_limit(), 4096);
        assert_eq!(channel.inflight_limit(), 8);
    }
}
