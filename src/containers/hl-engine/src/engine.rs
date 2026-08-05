//! Per-engine lifecycle coordination over injected launch capabilities.

use crate::activation::GuestIsa;
use crate::launch_plan::{LaunchMaterial, RuntimeLaunchPlan};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnginePhase {
    Created,
    Starting,
    Running,
    Stopping,
    Exited,
    Destroyed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StopRequest {
    Interrupt,
    Force,
    Signal(i32),
}

impl StopRequest {
    #[must_use]
    pub const fn signal(self) -> i32 {
        match self {
            Self::Interrupt => 2,
            Self::Force => 9,
            Self::Signal(signal) => signal,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExitKind {
    Code,
    Signal,
    Fault,
    EngineError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineExit {
    pub kind: ExitKind,
    pub guest_status: i32,
    pub detail: u64,
    pub fault: Option<FaultDiagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultReason {
    Fetch,
    Memory,
    Decode,
    Unsupported,
    Frozen,
    CacheEpoch,
    Protocol,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultDiagnostic {
    pub isa: GuestIsa,
    pub pc: u64,
    pub opcode: [u8; 15],
    pub opcode_len: u8,
    pub reason: FaultReason,
    pub address: Option<u64>,
    pub access: Option<FaultAccess>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultAccess {
    Read,
    Write,
    Execute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    Busy,
    Destroyed,
    LaunchFailed,
    Construction(crate::composition::ConstructionError),
    WorkspaceFailed,
    WaitFailed,
    StopFailed,
    Synchronization,
    Unsupported,
    AuthorityFailed,
    Load(hl_loader::LoadError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessId(pub u64);

/// Creates and removes launch-private host staging without exposing paths.
pub trait Workspace: Send + Sync {
    fn prepare(&self) -> Result<WorkspaceId, EngineError>;
    fn prepare_material(&self, _: &LaunchMaterial) -> Result<WorkspaceId, EngineError> {
        self.prepare()
    }
    fn cleanup(&self, workspace: WorkspaceId) -> Result<(), EngineError>;
}

/// Starts and controls one selected engine process.
pub trait Launcher: Send + Sync {
    fn launch(&self, isa: GuestIsa, plan: &RuntimeLaunchPlan, workspace: WorkspaceId)
    -> Result<ProcessId, EngineError>;
    fn launch_material(
        &self,
        isa: GuestIsa,
        material: &LaunchMaterial,
        workspace: WorkspaceId,
    ) -> Result<ProcessId, EngineError> {
        self.launch(isa, &material.plan, workspace)
    }
    fn wait(&self, process: ProcessId) -> Result<EngineExit, EngineError>;
    fn terminate(&self, process: ProcessId, request: StopRequest) -> Result<(), EngineError>;
}

struct Lifecycle {
    phase: EnginePhase,
    workspace: Option<WorkspaceId>,
    process: Option<ProcessId>,
    pending_stop: Option<StopRequest>,
    exit: Option<EngineExit>,
    terminal_error: Option<EngineError>,
    wait_in_progress: bool,
    start_in_progress: bool,
}

struct EngineContext<L, W> {
    lifecycle: Mutex<Lifecycle>,
    changed: Condvar,
    launcher: L,
    workspaces: W,
}

pub struct Engine<L, W> {
    isa: GuestIsa,
    plan: RuntimeLaunchPlan,
    material: Option<LaunchMaterial>,
    shared: Arc<EngineContext<L, W>>,
}

impl<L, W> Clone for Engine<L, W> {
    fn clone(&self) -> Self {
        Self {
            isa: self.isa,
            plan: self.plan.clone(),
            material: self.material.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<L: Launcher, W: Workspace> Engine<L, W> {
    pub(crate) fn launcher(&self) -> &L {
        &self.shared.launcher
    }

    #[must_use]
    pub fn new(isa: GuestIsa, plan: RuntimeLaunchPlan, launcher: L, workspaces: W) -> Self {
        Self {
            isa,
            plan,
            material: None,
            shared: Arc::new(EngineContext {
                lifecycle: Mutex::new(Lifecycle {
                    phase: EnginePhase::Created,
                    workspace: None,
                    process: None,
                    pending_stop: None,
                    exit: None,
                    terminal_error: None,
                    wait_in_progress: false,
                    start_in_progress: false,
                }),
                changed: Condvar::new(),
                launcher,
                workspaces,
            }),
        }
    }

    #[must_use]
    pub fn new_material(isa: GuestIsa, material: LaunchMaterial, launcher: L, workspaces: W) -> Self {
        let plan = material.plan.clone();
        let mut engine = Self::new(isa, plan, launcher, workspaces);
        engine.material = Some(material);
        engine
    }

    pub fn start(&self) -> Result<(), EngineError> {
        {
            let mut lifecycle = self.lock()?;
            match lifecycle.phase {
                EnginePhase::Created | EnginePhase::Stopping if lifecycle.process.is_none() => {
                    lifecycle.phase = EnginePhase::Starting;
                    lifecycle.start_in_progress = true;
                }
                EnginePhase::Destroyed => return Err(EngineError::Destroyed),
                _ => return Err(EngineError::Busy),
            }
        }

        let workspace = match self.material.as_ref().map_or_else(
            || self.shared.workspaces.prepare(),
            |material| self.shared.workspaces.prepare_material(material),
        ) {
            Ok(workspace) => workspace,
            Err(_) => {
                self.fail_start(EngineError::WorkspaceFailed)?;
                return Err(EngineError::WorkspaceFailed);
            }
        };
        {
            let mut lifecycle = self.lock()?;
            lifecycle.workspace = Some(workspace);
        }
        let process = match self.material.as_ref().map_or_else(
            || self.shared.launcher.launch(self.isa, &self.plan, workspace),
            |material| self.shared.launcher.launch_material(self.isa, material, workspace),
        ) {
            Ok(process) => process,
            Err(error) => {
                self.lock()?.workspace = None;
                let _ = self.shared.workspaces.cleanup(workspace);
                self.fail_start(error)?;
                return Err(error);
            }
        };
        let pending = {
            let mut lifecycle = self.lock()?;
            lifecycle.process = Some(process);
            lifecycle.start_in_progress = false;
            let pending = lifecycle.pending_stop;
            lifecycle.phase = if matches!(pending, Some(StopRequest::Force)) {
                EnginePhase::Stopping
            } else {
                EnginePhase::Running
            };
            self.shared.changed.notify_all();
            pending
        };
        if let Some(request) = pending {
            self.shared
                .launcher
                .terminate(process, request)
                .map_err(|_| EngineError::StopFailed)?;
        }
        Ok(())
    }

    pub fn terminate(&self, request: StopRequest) -> Result<(), EngineError> {
        let terminal = matches!(request, StopRequest::Force);
        let process = {
            let mut lifecycle = self.lock()?;
            match lifecycle.phase {
                EnginePhase::Created | EnginePhase::Starting => {
                    lifecycle.pending_stop = Some(request);
                    if terminal {
                        lifecycle.phase = EnginePhase::Stopping;
                    }
                    return Ok(());
                }
                EnginePhase::Running => {
                    lifecycle.pending_stop = Some(request);
                    if terminal {
                        lifecycle.phase = EnginePhase::Stopping;
                    }
                    lifecycle.process
                }
                EnginePhase::Stopping if !terminal => lifecycle.process,
                EnginePhase::Stopping => return Ok(()),
                EnginePhase::Exited => return Err(EngineError::Busy),
                EnginePhase::Destroyed => return Err(EngineError::Destroyed),
            }
        };
        self.shared
            .launcher
            .terminate(process.ok_or(EngineError::Synchronization)?, request)
            .map_err(|_| EngineError::StopFailed)
    }

    pub fn wait(&self) -> Result<EngineExit, EngineError> {
        let process = {
            let lifecycle = self.lock()?;
            let mut lifecycle = self
                .shared
                .changed
                .wait_while(lifecycle, |state| {
                    state.exit.is_none()
                        && state.terminal_error.is_none()
                        && state.phase != EnginePhase::Destroyed
                        && (state.wait_in_progress || state.start_in_progress)
                })
                .map_err(|_| EngineError::Synchronization)?;
            if let Some(exit) = lifecycle.exit {
                return Ok(exit);
            }
            if let Some(error) = lifecycle.terminal_error {
                return Err(error);
            }
            if lifecycle.phase == EnginePhase::Destroyed {
                return Err(EngineError::Destroyed);
            }
            let process = lifecycle.process.ok_or(EngineError::Busy)?;
            lifecycle.wait_in_progress = true;
            process
        };
        let result = self.shared.launcher.wait(process);
        let workspace = {
            let mut lifecycle = self.lock()?;
            lifecycle.wait_in_progress = false;
            match result {
                Ok(exit) => {
                    lifecycle.exit = Some(exit);
                    lifecycle.phase = EnginePhase::Exited;
                }
                Err(error) => {
                    lifecycle.terminal_error = Some(error);
                    lifecycle.phase = EnginePhase::Exited;
                }
            }
            self.shared.changed.notify_all();
            lifecycle.workspace.take()
        };
        if let Some(workspace) = workspace {
            self.shared
                .workspaces
                .cleanup(workspace)
                .map_err(|_| EngineError::WorkspaceFailed)?;
        }
        result
    }

    pub fn destroy(&self) -> Result<Option<EngineExit>, EngineError> {
        let phase = self.phase()?;
        if matches!(phase, EnginePhase::Created) || (phase == EnginePhase::Stopping && !self.lock()?.start_in_progress)
        {
            self.lock()?.phase = EnginePhase::Destroyed;
            return Ok(None);
        }
        let mut failure = None;
        if matches!(
            phase,
            EnginePhase::Running | EnginePhase::Starting | EnginePhase::Created
        ) {
            if let Err(error) = self.terminate(StopRequest::Force) {
                failure = Some(error);
            }
        }
        let exit = if matches!(self.phase()?, EnginePhase::Running | EnginePhase::Stopping) {
            match self.wait() {
                Ok(exit) => Some(exit),
                Err(error) => {
                    failure.get_or_insert(error);
                    None
                }
            }
        } else {
            self.lock()?.exit
        };
        let workspace = {
            let mut lifecycle = self.lock()?;
            lifecycle.phase = EnginePhase::Destroyed;
            self.shared.changed.notify_all();
            lifecycle.workspace.take()
        };
        if let Some(workspace) = workspace {
            if self.shared.workspaces.cleanup(workspace).is_err() {
                failure.get_or_insert(EngineError::WorkspaceFailed);
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(exit)
    }

    pub fn phase(&self) -> Result<EnginePhase, EngineError> {
        Ok(self.lock()?.phase)
    }

    fn fail_start(&self, error: EngineError) -> Result<(), EngineError> {
        let mut lifecycle = self.lock()?;
        lifecycle.start_in_progress = false;
        lifecycle.terminal_error = Some(error);
        lifecycle.phase = EnginePhase::Exited;
        self.shared.changed.notify_all();
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Lifecycle>, EngineError> {
        self.shared.lifecycle.lock().map_err(|_| EngineError::Synchronization)
    }
}

#[cfg(test)]
#[path = "engine_test.rs"]
mod tests;
