//! Host-neutral runtime construction and lifecycle composition.

use crate::activation::GuestIsa;
use crate::engine::{Engine, EngineError, EngineExit, Launcher, ProcessId, StopRequest, Workspace, WorkspaceId};
use crate::launcher::plan::RuntimePlan;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionError {
    MissingCheckpointSink,
    MissingCheckpointSource,
    /// Native execution does not yet own a PTY/stdio bridge for the requested terminal.
    UnsupportedTerminal,
    RuntimeConstruction,
}

/// Transactional destination for a checkpoint image.
pub trait CheckpointSink: Send + Sync {
    fn replace(&self, image: &[u8]) -> Result<(), CompositionError>;

    /// Stores one named object in the unpublished checkpoint generation.
    ///
    /// The retained C engine emits a process-tree image as independently
    /// addressable objects. Legacy single-image transports may leave this
    /// unsupported; construction does not advertise retained-C checkpointing
    /// until the complete object-store contract is available.
    fn put(&self, _name: &str, _bytes: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    /// Atomically publishes the generation after every object is durable.
    fn commit(&self, _manifest: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

/// Bounded source for a checkpoint image.
pub trait CheckpointSource: Send + Sync {
    fn read(&self, maximum: usize) -> Result<Vec<u8>, CompositionError>;

    /// Reads one named object from the committed checkpoint generation.
    fn get(&self, _name: &str) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    /// Lists every object in the committed checkpoint generation.
    fn list(&self) -> Result<Vec<String>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
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

    pub(crate) fn port(&self) -> Arc<dyn TerminalPort> {
        Arc::clone(&self.port)
    }

    pub(crate) fn initial(&self) -> (u16, u16) {
        self.initial
    }

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
#[derive(Clone, Default)]
pub struct StandardStreams {
    terminal: Option<Arc<Terminal>>,
}

impl StandardStreams {
    #[must_use]
    pub fn with_terminal(mut self, terminal: Arc<Terminal>) -> Self {
        self.terminal = Some(terminal);
        self
    }

    pub(crate) fn terminal(&self) -> Option<Arc<Terminal>> {
        self.terminal.clone()
    }
}

#[derive(Clone)]
pub struct RuntimeServices {
    pub checkpoint_sink: Option<Arc<dyn CheckpointSink>>,
    pub checkpoint_source: Option<Arc<dyn CheckpointSource>>,
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
    fn capture_checkpoint(&self) -> Result<(), EngineError> {
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

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.machine.capture_checkpoint()
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

    pub fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.engine.launcher().capture_checkpoint()
    }

    pub fn destroy(&self) -> Result<Option<EngineExit>, EngineError> {
        self.engine.destroy()
    }

    fn validate_services(plan: &RuntimePlan, services: &RuntimeServices) -> Result<(), CompositionError> {
        if services.streams.terminal().is_some() && !cfg!(unix) {
            return Err(CompositionError::UnsupportedTerminal);
        }
        if plan.options.get("HL_CHECKPOINT").is_some() && services.checkpoint_sink.is_none() {
            return Err(CompositionError::MissingCheckpointSink);
        }
        if plan.options.get("HL_RESTORE").is_some() && services.checkpoint_source.is_none() {
            return Err(CompositionError::MissingCheckpointSource);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "composition_test.rs"]
mod tests;
