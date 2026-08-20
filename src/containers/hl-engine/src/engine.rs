//! Per-engine lifecycle coordination over injected launch capabilities.

use crate::activation::GuestIsa;
use crate::launcher::plan::RuntimePlan;
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

impl EngineExit {
    /// Process status used by both engine frontends and the C worker protocol.
    #[must_use]
    pub const fn process_status(self) -> i32 {
        match self.kind {
            ExitKind::Code => self.guest_status,
            ExitKind::Signal => 128_i32.saturating_add(self.guest_status),
            ExitKind::Fault | ExitKind::EngineError => 125,
        }
    }

    /// Leave this process the way the guest left, and never return.
    ///
    /// A guest killed by a fatal signal has to reach whatever launched the worker as a process that
    /// died from that signal. `_exit(128 + signo)` loses the only distinction `wait(2)` draws:
    /// `WIFSIGNALED` is false, `WTERMSIG` is unavailable and no core is written, so a shell running
    /// a crashing program cannot tell the crash from a program that chose to exit 139. Every other
    /// outcome keeps [`Self::process_status`].
    pub fn exit_process(self) -> ! {
        #[cfg(target_os = "linux")]
        if matches!(self.kind, ExitKind::Signal) && (1..=64).contains(&self.guest_status) {
            // A Linux guest's signal numbers are this host's signal numbers, so the termination is
            // reproducible exactly. It is not elsewhere: Windows has no process-killed-by-signal
            // status at all, and Darwin numbers SIGBUS, SIGUSR1 and SIGUSR2 differently -- the
            // table that knows the difference is the C personality's sig_l2m, not this crate. Those
            // hosts keep the 128 + signo encoding rather than raise a signal that means something
            // else.
            #[allow(unsafe_code)]
            // SAFETY: three libc calls that touch only this process's own signal disposition and
            // mask, immediately before it terminates. No Rust value is borrowed across them, the
            // zeroed sigset is owned by this frame and fully initialised by `sigemptyset` before
            // use, nothing observes the disposition afterwards, and `raise` neither allocates nor
            // unwinds.
            unsafe {
                let mut pending: libc::sigset_t = std::mem::zeroed();
                libc::sigemptyset(&raw mut pending);
                libc::sigaddset(&raw mut pending, self.guest_status);
                libc::signal(self.guest_status, libc::SIG_DFL);
                libc::pthread_sigmask(libc::SIG_UNBLOCK, &raw const pending, std::ptr::null_mut());
                libc::raise(self.guest_status);
            }
            // Reached only when the signal's default action did not terminate this process.
        }
        std::process::exit(self.process_status())
    }
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
    NativeFatal,
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
    /// The guest already reached a terminal state, so the operation had nothing to act on.
    Exited,
    /// No guest has been launched yet, so the operation has nothing to observe.
    NotStarted,
    Destroyed,
    LaunchFailed,
    /// Engine composition refused the launch. The originating
    /// [`CompositionError`](crate::composition::CompositionError) is retained so
    /// callers and logs keep the cause instead of a bare launch failure.
    CompositionFailed(crate::composition::CompositionError),
    /// Native construction refused the configured engine with this stable `hl_status` value.
    NativeCreateFailed(i32),
    /// The private native library could not be loaded or did not satisfy the bridge contract.
    NativeLoadFailed(hl_native::LoadKind),
    /// The native engine completed its run boundary with this stable `hl_status` value.
    NativeRunFailed(i32),
    WorkspaceFailed,
    WaitFailed,
    /// The native process reached a terminal state while a checkpoint transaction was still active.
    CheckpointExited(EngineExit),
    StopFailed,
    NativeStopFailed(i32),
    Synchronization,
    /// The launch selected a sandbox policy the checkpoint engine cannot capture under.
    ///
    /// `HL_UNTRUSTED` moves every host-authority object (open files, sockets, pipes) into the
    /// sentry process, so the worker's own descriptor table no longer describes the guest and the
    /// capture path refuses. Unlike the transient refusals a checkpoint preflight retries, this
    /// one is a property of the launch and cannot become supported while the guest is running.
    CheckpointUnsupportedUnderSandbox,
    /// Recovery refused the generation the checkpoint store offered because it is
    /// staged, not finalized.
    ///
    /// The byte store is adversarial and its committed-generation pointer is data
    /// rather than authority, so a generation whose transaction never committed --
    /// or one an attacker assembled -- must never reach native restore. Like the
    /// sandbox refusal this is a property of the stored image, not a transient
    /// state, so a preflight must not poll on it.
    CheckpointGenerationUnfinalized,
    /// A checkpoint capture that started but did not complete.
    ///
    /// This is not a launch failure and must never be reported as one: the guest launched, ran, and
    /// was still running when the capture was refused or abandoned. A macOS build reported every
    /// refused capture as `LaunchFailed`, which reached the desktop as a bare launch-failure
    /// dialog on a workspace the user had just been typing into.
    CaptureFailed,
    /// A checkpoint capture the engine DECIDED not to publish, having said why.
    ///
    /// Distinct from `CaptureFailed`, which is what a capture that broke or was abandoned reports.
    /// A refusal is a decision with a cause the engine can name -- an unsupported descriptor, a member
    /// that never reached a safepoint -- and the cause is available from the checkpoint control that
    /// produced this error. Reporting the two the same way is what made every checkpoint refusal
    /// surface as an unexplained failure thirty seconds after the decision that caused it.
    CaptureRefused,
    /// The launch names a writable root filesystem owned by a host user the engine cannot act as.
    ///
    /// The engine runs as an unprivileged host uid and materializes guest ownership in its own
    /// owner overlay (`container/owner.h`, `HL_FILE_OWNERS`); it never acquires host privilege. A
    /// rootfs unpacked under `sudo`, or `chown -R 0:0`ed, is therefore unwritable no matter what
    /// the guest's `id -u` reports, and every guest write fails `EACCES` with no explanation --
    /// `git clone` and `git checkout` are the usual first casualties. Granting the access would be
    /// host-privilege escalation and cannot even be implemented: `chmod(2)` refuses for a non-owner
    /// without `CAP_FOWNER`. The contract is that a rootfs is materialized under a uid the engine
    /// can act as, exactly as rootless Docker materializes into a user namespace where the host uid
    /// is guest 0. This refusal states it at launch instead of letting the workspace half-work.
    RootfsNotOwnedByEngine {
        /// The host uid that owns the writable root the launch named.
        rootfs_uid: u32,
        /// The host uid the engine actually runs as.
        engine_uid: u32,
    },
    /// The capture ledger was left poisoned by a panicking participant, so no capture can be
    /// admitted until the engine is rebuilt.
    CapturePoisoned,
    Unsupported,
}

impl EngineError {
    /// Whether re-asking would ever produce a different answer.
    ///
    /// A checkpoint preflight polls until its deadline, which turns a permanent refusal into a
    /// full-timeout stall reported as an opaque preflight failure. A permanent refusal must be
    /// surfaced on the first observation instead.
    #[must_use]
    pub fn is_permanent_refusal(self) -> bool {
        matches!(
            self,
            Self::CheckpointUnsupportedUnderSandbox | Self::CheckpointGenerationUnfinalized | Self::Unsupported
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkspaceId(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessId(pub u64);

/// Creates and removes launch-private host staging without exposing paths.
pub trait Workspace: Send + Sync {
    fn prepare(&self) -> Result<WorkspaceId, EngineError>;
    fn cleanup(&self, workspace: WorkspaceId) -> Result<(), EngineError>;
}

/// Starts and controls one selected engine process.
pub trait Launcher: Send + Sync {
    fn launch(&self, isa: GuestIsa, plan: &RuntimePlan, workspace: WorkspaceId) -> Result<ProcessId, EngineError>;
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
    plan: RuntimePlan,
    shared: Arc<EngineContext<L, W>>,
}

impl<L, W> Clone for Engine<L, W> {
    fn clone(&self) -> Self {
        Self {
            isa: self.isa,
            plan: self.plan.clone(),
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<L: Launcher, W: Workspace> Engine<L, W> {
    pub(crate) fn launcher(&self) -> &L {
        &self.shared.launcher
    }

    #[must_use]
    pub fn new(isa: GuestIsa, plan: RuntimePlan, launcher: L, workspaces: W) -> Self {
        Self {
            isa,
            plan,
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

    pub fn start(&self) -> Result<(), EngineError> {
        let stale = {
            let mut lifecycle = self.lock()?;
            match lifecycle.phase {
                EnginePhase::Created | EnginePhase::Stopping if lifecycle.process.is_none() => {
                    lifecycle.phase = EnginePhase::Starting;
                    lifecycle.start_in_progress = true;
                    None
                }
                // `docker start` on an exited container runs it again, so a terminal engine is
                // restartable once the previous run's process, exit and workspace are released.
                EnginePhase::Exited => {
                    lifecycle.phase = EnginePhase::Starting;
                    lifecycle.start_in_progress = true;
                    lifecycle.process = None;
                    lifecycle.exit = None;
                    lifecycle.terminal_error = None;
                    lifecycle.pending_stop = None;
                    lifecycle.workspace.take()
                }
                EnginePhase::Destroyed => return Err(EngineError::Destroyed),
                _ => return Err(EngineError::Busy),
            }
        };
        if let Some(workspace) = stale {
            let _ = self.shared.workspaces.cleanup(workspace);
        }

        let Ok(workspace) = self.shared.workspaces.prepare() else {
            self.fail_start(EngineError::WorkspaceFailed)?;
            return Err(EngineError::WorkspaceFailed);
        };
        {
            let mut lifecycle = self.lock()?;
            lifecycle.workspace = Some(workspace);
        }
        let process = match self.shared.launcher.launch(self.isa, &self.plan, workspace) {
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
            self.shared.launcher.terminate(process, request)?;
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
                EnginePhase::Exited => return Err(EngineError::Exited),
                EnginePhase::Destroyed => return Err(EngineError::Destroyed),
            }
        };
        self.shared
            .launcher
            .terminate(process.ok_or(EngineError::Synchronization)?, request)
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
            let process = lifecycle.process.ok_or(EngineError::NotStarted)?;
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
        ) && let Err(error) = self.terminate(StopRequest::Force)
        {
            failure = Some(error);
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
        if let Some(workspace) = workspace
            && self.shared.workspaces.cleanup(workspace).is_err()
        {
            failure.get_or_insert(EngineError::WorkspaceFailed);
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
