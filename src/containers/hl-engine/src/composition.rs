//! Host-neutral runtime construction and lifecycle composition.

use crate::activation::GuestIsa;
use crate::engine::{
    Engine, EngineError, EngineExit, EnginePhase, Launcher, ProcessId, StopRequest, Workspace, WorkspaceId,
};
use crate::launch_plan::RuntimeLaunchPlan;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompositionError {
    MissingCheckpointSink,
    MissingCheckpointSource,
    RuntimeConstruction,
}

/// Safe activation transport presented to a constructed runtime.
pub trait ActivationChannel: Send + Sync {
    fn send(&self, message: &[u8]) -> Result<(), CompositionError>;
    fn receive(&self, maximum: usize) -> Result<Vec<u8>, CompositionError>;
}

/// Transactional destination for a checkpoint image.
pub trait CheckpointSink: Send + Sync {
    fn replace(&self, image: &[u8]) -> Result<(), CompositionError>;
}

/// Bounded source for a checkpoint image.
pub trait CheckpointSource: Send + Sync {
    fn read(&self, maximum: usize) -> Result<Vec<u8>, CompositionError>;
}

type StandardInput = Arc<Mutex<Box<dyn Read + Send>>>;
type StandardOutput = Arc<Mutex<Box<dyn Write + Send>>>;

/// Process-owned standard streams installed as Linux descriptors 0, 1, and 2.
///
/// Clones retain the same underlying stream objects. This matches Linux open-file-description
/// sharing across descriptor duplication and process forks without consulting process-global I/O.
#[derive(Clone)]
pub struct StandardStreams {
    input: StandardInput,
    output: StandardOutput,
    error: StandardOutput,
}

impl StandardStreams {
    #[must_use]
    pub fn new(
        input: impl Read + Send + 'static,
        output: impl Write + Send + 'static,
        error: impl Write + Send + 'static,
    ) -> Self {
        Self {
            input: Arc::new(Mutex::new(Box::new(input))),
            output: Arc::new(Mutex::new(Box::new(output))),
            error: Arc::new(Mutex::new(Box::new(error))),
        }
    }

    pub(crate) fn input(&self) -> StandardInput {
        Arc::clone(&self.input)
    }

    pub(crate) fn output(&self) -> StandardOutput {
        Arc::clone(&self.output)
    }

    pub(crate) fn error(&self) -> StandardOutput {
        Arc::clone(&self.error)
    }
}

impl Default for StandardStreams {
    fn default() -> Self {
        Self::new(std::io::stdin(), std::io::stdout(), std::io::stderr())
    }
}

#[derive(Clone)]
pub struct RuntimeServices {
    pub activation: Arc<dyn ActivationChannel>,
    pub checkpoint_sink: Option<Arc<dyn CheckpointSink>>,
    pub checkpoint_source: Option<Arc<dyn CheckpointSource>>,
    pub streams: StandardStreams,
}

pub struct RuntimeConstruction<'a> {
    pub isa: GuestIsa,
    pub plan: &'a RuntimeLaunchPlan,
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
    fn launch(&self, _: GuestIsa, _: &RuntimeLaunchPlan, _: WorkspaceId) -> Result<ProcessId, EngineError> {
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
        plan: RuntimeLaunchPlan,
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

    pub fn phase(&self) -> Result<EnginePhase, EngineError> {
        self.engine.phase()
    }

    fn validate_services(plan: &RuntimeLaunchPlan, services: &RuntimeServices) -> Result<(), CompositionError> {
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
