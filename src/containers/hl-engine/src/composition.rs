//! Host-neutral runtime construction and lifecycle composition.

use crate::activation::GuestIsa;
use crate::engine::{Engine, EngineError, EngineExit, Launcher, ProcessId, StopRequest, Workspace, WorkspaceId};
use crate::launcher::plan::RuntimePlan;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Default bound for callers that use the convenience checkpoint API.
///
/// Product paths should pass their request deadline explicitly through
/// `capture_checkpoint_until`; the default exists for direct embedders only.
pub const DEFAULT_CHECKPOINT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionError {
    MissingCheckpointSink,
    MissingCheckpointSource,
    /// Native execution does not yet own a PTY/stdio bridge for the requested terminal.
    UnsupportedTerminal,
    RuntimeConstruction,
    TransactionBusy,
    DeadlineExceeded,
    /// The authoritative generation changed, but its containing directory
    /// could not be synced. Callers must not retry the same publication as if
    /// the former generation were still authoritative.
    PublishedNotDurable,
}

/// Transactional destination for a checkpoint image.
pub trait CheckpointSink: Send + Sync {
    fn replace(&self, image: &[u8]) -> Result<(), CompositionError>;

    /// Acquires exclusive ownership of one unpublished generation.
    fn begin_until(&self, deadline: std::time::Instant) -> Result<NonZeroU64, CompositionError>;

    /// Stores without spawning detachable work. Implementations must bound all
    /// userspace waits they control and report deadline expiry cooperatively.
    fn put_until(
        &self,
        transaction: NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CompositionError>;

    /// Discards the unpublished generation before an absolute monotonic
    /// deadline. Implementations must not abandon background cleanup work.
    fn abort_until(&self, transaction: NonZeroU64, deadline: std::time::Instant) -> Result<(), CompositionError>;

    /// Publishes transactionally. Expiry before the irrevocable publication
    /// point must leave the former generation authoritative; once publication
    /// succeeds the implementation must return success, not a late timeout.
    fn commit_until(
        &self,
        transaction: NonZeroU64,
        manifest: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CompositionError>;
}

/// Deadline-aware source for a checkpoint image.
///
/// Deadlines cooperatively bound userspace waits. They cannot interrupt a
/// synchronous kernel filesystem operation already in progress.
pub trait CheckpointSource: Send + Sync {
    fn read(&self, maximum: usize) -> Result<Vec<u8>, CompositionError>;

    /// Reads one named object from the committed checkpoint generation.
    fn get(&self, _name: &str) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn get_until(&self, name: &str, deadline: std::time::Instant) -> Result<Vec<u8>, CompositionError>;

    /// Lists every object in the committed checkpoint generation.
    fn list(&self) -> Result<Vec<String>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, CompositionError>;
}

/// Host-facing byte transport for one terminal master.
///
/// The runtime owns this consumer port. Implementations merge terminal output
/// into one stream and must make a blocked read or write return promptly after
/// [`TerminalPort::close`]. Reads return zero after close, matching terminal
/// master EOF; writes after close return `BrokenPipe`. Partial reads and writes
/// are valid and callers must retain the unconsumed suffix.
pub trait TerminalPort: Send + Sync {
    fn read(&self, output: &mut [u8]) -> std::io::Result<usize>;
    fn write(&self, input: &[u8]) -> std::io::Result<usize>;
    fn close(&self);
}

pub(crate) trait TerminalAttachment: Send + Sync {
    fn resize(&self, rows: u16, columns: u16) -> Result<(), CompositionError>;
}

pub struct Terminal {
    port: Arc<dyn TerminalPort>,
    #[cfg(unix)]
    initial: (u16, u16),
    attachment: Mutex<Option<Arc<dyn TerminalAttachment>>>,
}

impl Terminal {
    /// Creates an unattached terminal with a non-empty initial cell size.
    pub fn new(port: Arc<dyn TerminalPort>, rows: u16, columns: u16) -> Result<Arc<Self>, CompositionError> {
        if rows == 0 || columns == 0 {
            return Err(CompositionError::RuntimeConstruction);
        }
        Ok(Arc::new(Self {
            port,
            #[cfg(unix)]
            initial: (rows, columns),
            attachment: Mutex::new(None),
        }))
    }

    pub fn resize(&self, rows: u16, columns: u16) -> Result<(), CompositionError> {
        if rows == 0 || columns == 0 {
            return Err(CompositionError::RuntimeConstruction);
        }
        self.attachment
            .lock()
            .map_err(|_| CompositionError::RuntimeConstruction)?
            .as_ref()
            .ok_or(CompositionError::UnsupportedTerminal)?
            .resize(rows, columns)
    }

    pub fn close(&self) {
        self.port.close();
    }

    #[cfg(unix)]
    pub(crate) fn port(&self) -> Arc<dyn TerminalPort> {
        Arc::clone(&self.port)
    }

    #[cfg(unix)]
    pub(crate) fn initial(&self) -> (u16, u16) {
        self.initial
    }

    #[cfg(unix)]
    pub(crate) fn attach(&self, attachment: Arc<dyn TerminalAttachment>) -> Result<(), CompositionError> {
        let mut current = self
            .attachment
            .lock()
            .map_err(|_| CompositionError::RuntimeConstruction)?;
        if current.is_some() {
            return Err(CompositionError::RuntimeConstruction);
        }
        *current = Some(attachment);
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn detach(&self) {
        let mut current = self
            .attachment
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        current.take();
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.close();
    }
}

/// Optional terminal request retained for the native PTY boundary.
///
/// Construction rejects a populated request until the runtime can bind its
/// master transport and resize operations to native descriptors. This keeps a
/// terminal from silently using the supervisor's standard descriptors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardStream {
    Stdout,
    Stderr,
}

pub trait StandardStreamPort: Send + Sync {
    fn read(&self, _bytes: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
    fn write(&self, stream: StandardStream, bytes: &[u8]) -> std::io::Result<usize>;
    fn close(&self);
}

#[derive(Clone, Default)]
pub struct StandardStreams {
    terminal: Option<Arc<Terminal>>,
    output: Option<Arc<dyn StandardStreamPort>>,
}

impl StandardStreams {
    #[must_use]
    pub fn with_terminal(mut self, terminal: Arc<Terminal>) -> Self {
        self.terminal = Some(terminal);
        self
    }

    #[must_use]
    pub fn with_output(mut self, output: Arc<dyn StandardStreamPort>) -> Self {
        self.output = Some(output);
        self
    }

    pub(crate) fn terminal(&self) -> Option<Arc<Terminal>> {
        self.terminal.clone()
    }

    pub(crate) fn output(&self) -> Option<Arc<dyn StandardStreamPort>> {
        self.output.clone()
    }
}

/// The native checkpoint broker and trigger word shared by one process domain.
///
/// A container launch mints exactly one broker socket and one trigger memfd. Every
/// exec session started into the same domain joins that pair instead of minting its
/// own, so the coordinator's generation bump is observed at the safepoint gates of
/// every sealed member and every member commits into the same store.
#[cfg(unix)]
#[derive(Clone)]
pub struct CheckpointChannel(pub(crate) Arc<hl_native::CheckpointTransport>);

#[derive(Clone)]
pub struct RuntimeServices {
    pub checkpoint_sink: Option<Arc<dyn CheckpointSink>>,
    pub checkpoint_source: Option<Arc<dyn CheckpointSource>>,
    /// Set on a session that joins an existing domain freeze. Mutually exclusive
    /// with `checkpoint_sink`/`checkpoint_source`: a member has no image of its own.
    #[cfg(unix)]
    pub checkpoint_channel: Option<CheckpointChannel>,
    pub streams: StandardStreams,
}

pub struct RuntimeConstruction<'a> {
    pub isa: GuestIsa,
    pub plan: &'a RuntimePlan,
    pub services: &'a RuntimeServices,
}

/// One completely constructed runtime instance.
pub trait GuestMachine: Send + Sync {
    fn start(&self) -> Result<(), EngineError>;
    fn wait(&self) -> Result<EngineExit, EngineError>;
    fn stop(&self, request: StopRequest) -> Result<(), EngineError>;
    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }
    /// The container-namespace pid of the guest process this machine launched, once it has one.
    ///
    /// It is the identity a checkpoint image names this process by, so it is what a host may hold
    /// across a capture: a restore re-forks the member under exactly this number.
    fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        None
    }
    /// One member of the tree this machine restored, by the guest pid its image names it by.
    ///
    /// `None` for a machine that started fresh, and for a guest pid no restore announced. A member
    /// that is present can be asked whether it is still the same live process, signalled, and read for
    /// the exit it reported -- it is never relaunched, because a relaunch is a different process.
    #[cfg(unix)]
    fn restored_member(&self, _guest_pid: std::num::NonZeroI32) -> Option<crate::runtime::RestoredMember> {
        None
    }
    /// The domain freeze channel this machine owns, if it is the coordinator.
    #[cfg(unix)]
    fn checkpoint_channel(&self) -> Option<CheckpointChannel> {
        None
    }
    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }
    /// Captures without detached timeout work. Storage waits under Husklet's
    /// control stop by `deadline`; a publication already past its irrevocable
    /// replacement point completes synchronously and reports its real outcome.
    fn capture_checkpoint_until(&self, _deadline: std::time::Instant) -> Result<(), EngineError> {
        Err(EngineError::Unsupported)
    }
}

/// Constructs all runtime domains for one engine.
///
/// Implementations validate their domain services before publication. If any
/// constructor fails, already-created domains are destroyed in reverse order
/// before this method returns an error.
pub trait RuntimeFactory {
    type Machine: GuestMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError>;
}

struct MachineLauncher<M: GuestMachine> {
    machine: Arc<M>,
    worker: Mutex<Option<JoinHandle<Result<(), EngineError>>>>,
}

impl<M: GuestMachine> Drop for MachineLauncher<M> {
    fn drop(&mut self) {
        let worker = self.worker.get_mut().unwrap_or_else(|error| error.into_inner()).take();
        let Some(worker) = worker else { return };
        let _ = self.machine.stop(StopRequest::Force);
        let _ = worker.join();
    }
}

impl<M: GuestMachine + 'static> Launcher for MachineLauncher<M> {
    fn launch(&self, _: GuestIsa, _: &RuntimePlan, _: WorkspaceId) -> Result<ProcessId, EngineError> {
        let mut worker = self.worker.lock().map_err(|_| EngineError::Synchronization)?;
        if worker.is_some() {
            return Err(EngineError::Busy);
        }
        let machine = Arc::clone(&self.machine);
        *worker = Some(
            std::thread::Builder::new()
                .name("hl-engine".to_owned())
                .spawn(move || machine.start())
                .map_err(|_| EngineError::LaunchFailed)?,
        );
        Ok(ProcessId(1))
    }

    fn wait(&self, _: ProcessId) -> Result<EngineExit, EngineError> {
        let worker = self
            .worker
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .take()
            .ok_or(EngineError::Busy)?;
        worker.join().map_err(|_| EngineError::WaitFailed)??;
        self.machine.wait()
    }

    fn terminate(&self, _: ProcessId, request: StopRequest) -> Result<(), EngineError> {
        self.machine.stop(request)
    }
}

impl<M: GuestMachine> MachineLauncher<M> {
    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        self.machine.checkpoint_supported()
    }

    fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        self.machine.guest_pid()
    }

    #[cfg(unix)]
    fn restored_member(&self, guest_pid: std::num::NonZeroI32) -> Option<crate::runtime::RestoredMember> {
        self.machine.restored_member(guest_pid)
    }

    #[cfg(unix)]
    fn checkpoint_channel(&self) -> Option<CheckpointChannel> {
        self.machine.checkpoint_channel()
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.machine.capture_checkpoint()
    }

    fn capture_checkpoint_until(&self, deadline: std::time::Instant) -> Result<(), EngineError> {
        self.machine.capture_checkpoint_until(deadline)
    }
}

/// Public app composition around one independently owned runtime machine.
pub struct EngineBackend<M: GuestMachine, W> {
    engine: Engine<MachineLauncher<M>, W>,
}

impl<M: GuestMachine + 'static, W: Workspace> EngineBackend<M, W> {
    pub fn construct<F>(
        isa: GuestIsa,
        plan: RuntimePlan,
        services: RuntimeServices,
        factory: &F,
        workspaces: W,
    ) -> Result<Self, CompositionError>
    where
        F: RuntimeFactory<Machine = M>,
    {
        Self::validate_services(&plan, &services)?;
        let machine = factory.construct(RuntimeConstruction {
            isa,
            plan: &plan,
            services: &services,
        })?;
        Ok(Self {
            engine: Engine::new(
                isa,
                plan,
                MachineLauncher {
                    machine: Arc::new(machine),
                    worker: Mutex::new(None),
                },
                workspaces,
            ),
        })
    }

    pub fn start(&self) -> Result<(), EngineError> {
        self.engine.start()
    }

    pub fn wait(&self) -> Result<EngineExit, EngineError> {
        self.engine.wait()
    }

    pub fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        self.engine.terminate(request)
    }

    pub fn checkpoint_supported(&self) -> Result<(), EngineError> {
        self.engine.launcher().checkpoint_supported()
    }

    /// The container-namespace pid of the launched guest process, once it has one.
    #[must_use]
    pub fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        self.engine.launcher().guest_pid()
    }

    /// One member of the tree this engine restored, by the guest pid its image names it by.
    #[cfg(unix)]
    #[must_use]
    pub fn restored_member(&self, guest_pid: std::num::NonZeroI32) -> Option<crate::runtime::RestoredMember> {
        self.engine.launcher().restored_member(guest_pid)
    }

    pub fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.engine.launcher().capture_checkpoint()
    }

    /// The domain freeze channel every session in this domain must join.
    #[cfg(unix)]
    #[must_use]
    pub fn checkpoint_channel(&self) -> Option<CheckpointChannel> {
        self.engine.launcher().checkpoint_channel()
    }

    /// Captures the running process tree before an absolute monotonic deadline.
    ///
    /// # Errors
    /// Returns lifecycle, storage, synchronization, or deadline failures.
    pub fn capture_checkpoint_until(&self, deadline: std::time::Instant) -> Result<(), EngineError> {
        self.engine.launcher().capture_checkpoint_until(deadline)
    }

    pub fn destroy(&self) -> Result<Option<EngineExit>, EngineError> {
        self.engine.destroy()
    }

    fn validate_services(plan: &RuntimePlan, services: &RuntimeServices) -> Result<(), CompositionError> {
        if services.streams.terminal().is_some() && !cfg!(unix) {
            return Err(CompositionError::UnsupportedTerminal);
        }
        if services.streams.terminal().is_some() && services.streams.output().is_some() {
            return Err(CompositionError::RuntimeConstruction);
        }
        #[cfg(unix)]
        let joined = services.checkpoint_channel.is_some();
        #[cfg(not(unix))]
        let joined = false;
        #[cfg(unix)]
        if joined && (services.checkpoint_sink.is_some() || services.checkpoint_source.is_some()) {
            return Err(CompositionError::RuntimeConstruction);
        }
        if plan.options.get("HL_CHECKPOINT").is_some() && services.checkpoint_sink.is_none() && !joined {
            return Err(CompositionError::MissingCheckpointSink);
        }
        if plan.options.get("HL_RESTORE").is_some() && services.checkpoint_source.is_none() && !joined {
            return Err(CompositionError::MissingCheckpointSource);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "composition_test.rs"]
mod tests;
