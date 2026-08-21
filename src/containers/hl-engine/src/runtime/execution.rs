#![allow(unsafe_code)]

#[cfg(unix)]
use crate::composition::{CheckpointSink, CheckpointSource};
use crate::composition::{CompositionError, GuestMachine, RuntimeConstruction, RuntimeFactory};
use crate::engine::{EngineError, EngineExit, ExitKind, StopRequest};
use std::ffi::CString;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use super::checkpoint::Server;

#[cfg(unix)]
use super::terminal::NativeOutputBridge;
#[cfg(unix)]
use super::terminal::{InputDiscipline, NativeTerminalBridge};

const REQUEST_INTERRUPT: u32 = 1;
const REQUEST_FORCE_STOP: u32 = 2;
const REQUEST_SIGNAL: u32 = 3;
#[cfg(unix)]
const REQUEST_CHECKPOINT: u32 = 4;

/// The writable root a plan will materialize guest state into, if it has one.
///
/// An overlay upper layer, when the launch configures one, is where every guest write lands and the
/// lower layers are read-only by construction -- so a root-owned lower layer is legitimate and must
/// not be refused. Without an overlay the rootfs itself is the writable root, unless the launch
/// asked for `HL_ROOTFS_RO`, in which case nothing is written and any owner works.
#[cfg(unix)]
fn writable_root(plan: &crate::launcher::plan::RuntimePlan) -> Option<&[u8]> {
    if let Some(upper) = plan.options.get_bytes("HL_OVERLAY_UPPER") {
        return Some(upper);
    }
    if plan.options.get_bytes("HL_ROOTFS_RO").is_some() {
        return None;
    }
    plan.rootfs.as_deref()
}

/// Refuses a launch whose writable root belongs to a host user the engine cannot act as.
///
/// The engine runs as an unprivileged host uid and never acquires host privilege: guest ownership
/// lives in its own owner overlay (`container/owner.h`, `HL_FILE_OWNERS`), which is why a guest can
/// report `id -u` = 0 while every write to a host-root-owned tree returns `EACCES`. Granting the
/// access is not merely refused by the engine, it is unimplementable -- `chmod(2)` refuses for a
/// non-owner without `CAP_FOWNER` -- so the contract has to be stated at launch instead.
///
/// Only the root directory is examined. Walking the tree would be unbounded work at launch, and a
/// root-owned subtree below a writable root is a legitimate shape: shared read-only layers and host
/// bind mounts both produce it, and the guest can still do everything the host user could. A root
/// the engine does not own is different in kind -- nothing in the workspace is writable, so the
/// failure is total, and refusing is kinder than letting a developer find it through a failing
/// `git checkout`.
///
/// A path that cannot be stat'd is not refused here: the existing launch path already owns "the
/// rootfs is not there", and an ownership cause for a missing directory is a worse diagnostic than
/// the one it would replace. Running as host root refuses nothing, because then every owner is
/// writable.
#[cfg(unix)]
fn refuse_unownable_root(plan: &crate::launcher::plan::RuntimePlan) -> Result<(), EngineError> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    let Some(root) = writable_root(plan) else { return Ok(()) };
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(root));
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    // SAFETY: `geteuid` takes no arguments, reads no caller memory, and is documented never to fail.
    let engine_uid = unsafe { libc::geteuid() };
    let rootfs_uid = metadata.uid();
    if engine_uid == 0 || rootfs_uid == engine_uid {
        return Ok(());
    }
    hl_log::hl_error!(
        hl_log::tag::EXEC,
        "refusing launch: the writable root {} is owned by host uid {rootfs_uid}, but the engine runs \
         as host uid {engine_uid} and never acquires host privilege, so no guest write can succeed \
         however the guest reports its own id. Re-materialize the rootfs as uid {engine_uid} -- unpack \
         it without sudo, or `chown -R {engine_uid} {}` -- or launch it read-only with HL_ROOTFS_RO.",
        path.display(),
        path.display()
    );
    Err(EngineError::RootfsNotOwnedByEngine { rootfs_uid, engine_uid })
}

pub(crate) struct ProductionMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launcher::plan::RuntimePlan,
    #[cfg(unix)]
    terminal: Option<NativeTerminalBridge>,
    #[cfg(unix)]
    output: Option<NativeOutputBridge>,
    #[cfg(unix)]
    checkpoint: Option<CheckpointControl>,
    /// Set instead of `checkpoint` when this machine joins a domain freeze it does
    /// not coordinate. A member has no `Server`, so it has no channel to publish an
    /// image of its own on; its guest processes commit into the coordinator's store.
    #[cfg(unix)]
    member: Option<crate::composition::CheckpointChannel>,
    engine: Mutex<Option<Arc<hl_native::Engine>>>,
}

pub(crate) struct ProductionFactory;

impl RuntimeFactory for ProductionFactory {
    type Machine = ProductionMachine;

    fn construct(&self, request: RuntimeConstruction<'_>) -> Result<Self::Machine, CompositionError> {
        #[cfg(unix)]
        let terminal = request
            .services
            .streams
            .terminal()
            .map(|terminal| NativeTerminalBridge::attach(terminal, InputDiscipline::Linux))
            .transpose()?;
        #[cfg(unix)]
        let output = if terminal.is_none() {
            request
                .services
                .streams
                .output()
                .map(NativeOutputBridge::attach)
                .transpose()?
        } else {
            None
        };
        #[cfg(unix)]
        let member = request.services.checkpoint_channel.clone();
        #[cfg(unix)]
        let checkpoint = match (
            request.services.checkpoint_sink.clone(),
            request.services.checkpoint_source.clone(),
        ) {
            (Some(sink), Some(source)) => Some(CheckpointControl::start(
                sink,
                source,
                request.isa,
                request
                    .plan
                    .options
                    .get_bytes("HL_CHECKPOINT_PHASE_LEDGER")
                    .and_then(|_| request.plan.options.get("HL_DIAGNOSTIC_PORT"))
                    .and_then(|value| value.parse().ok()),
                request
                    .plan
                    .options
                    .get_bytes("HL_CHECKPOINT_PHASE_CLOCK_FAIL")
                    .is_some(),
            )?),
            (None, None) => None,
            _ => return Err(CompositionError::RuntimeConstruction),
        };
        Ok(ProductionMachine {
            isa: request.isa,
            plan: request.plan.clone(),
            #[cfg(unix)]
            terminal,
            #[cfg(unix)]
            output,
            #[cfg(unix)]
            checkpoint,
            #[cfg(unix)]
            member,
            engine: Mutex::new(None),
        })
    }
}

impl ProductionMachine {
    fn encode_environment(&self) -> Vec<u8> {
        let mut encoded = Vec::new();
        for (index, record) in self.plan.environment.iter().enumerate() {
            if index != 0 {
                encoded.push(b'\n');
            }
            encode_environment_record(&mut encoded, record);
        }
        encoded
    }

    #[cfg(unix)]
    fn refuse_unownable_root(&self) -> Result<(), EngineError> {
        refuse_unownable_root(&self.plan)
    }

    fn create(&self) -> Result<hl_native::Engine, EngineError> {
        #[cfg(unix)]
        self.refuse_unownable_root()?;
        let rootfs = self
            .plan
            .rootfs
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| EngineError::LaunchFailed)?;
        let executable = self
            .plan
            .executable_host
            .as_deref()
            .map(CString::new)
            .transpose()
            .map_err(|_| EngineError::LaunchFailed)?;
        let mut options = self
            .plan
            .options
            .iter()
            .map(|(name, value)| Ok((CString::new(name)?, CString::new(value)?)))
            .collect::<Result<Vec<_>, std::ffi::NulError>>()
            .map_err(|_| EngineError::LaunchFailed)?;
        // Name the coordinator on the launch boundary. Only a machine holding a CheckpointControl can be
        // sent REQUEST_CHECKPOINT, so this is exactly "the embedder will ask THIS engine to capture"; a
        // domain member carries a channel and no Server. The engine's election reads it instead of asking
        // whether it is the top of a launch, which every exec session also is.
        #[cfg(unix)]
        if self.checkpoint.is_some() {
            options.push((
                CString::new("HL_CHECKPOINT_COORDINATOR").expect("literal"),
                CString::new("1").expect("literal"),
            ));
        }
        options.push((
            CString::new("HL_GUEST_ENV").expect("literal"),
            CString::new(self.encode_environment()).map_err(|_| EngineError::LaunchFailed)?,
        ));
        options.push((
            CString::new("HL_GUEST_ENV_ESC").expect("literal"),
            CString::new("1").expect("literal"),
        ));
        options.push((
            CString::new("HL_GUEST_ENV_EXACT").expect("literal"),
            CString::new("1").expect("literal"),
        ));
        let names = options.iter().map(|(name, _)| name.as_ptr()).collect::<Vec<_>>();
        let values = options.iter().map(|(_, value)| value.as_ptr()).collect::<Vec<_>>();
        #[cfg(unix)]
        let standard_fds = self
            .terminal
            .as_ref()
            .map(NativeTerminalBridge::standard_fds)
            .or_else(|| self.output.as_ref().map(NativeOutputBridge::standard_fds))
            .unwrap_or([0, 1, 2]);
        #[cfg(not(unix))]
        let standard_fds = [0, 1, 2];
        let config = hl_native::EngineConfig {
            isa: match self.isa {
                crate::activation::GuestIsa::Aarch64 => 1,
                crate::activation::GuestIsa::X86_64 => 2,
            },
            rootfs: rootfs.as_deref(),
            executable_host: executable.as_deref(),
            executable_fd: -1,
            option_names: &names,
            option_values: &values,
            standard_fds,
            provider_fd: -1,
        };
        // SAFETY: all pointers in config remain live for this call and there is no callback state.
        let engine = unsafe { hl_native::Engine::create(config) }.map_err(|error| match error {
            hl_native::Error::Load(kind) => EngineError::NativeLoadFailed(kind),
            hl_native::Error::Status(status) => EngineError::NativeCreateFailed(status),
        })?;
        #[cfg(unix)]
        let mut engine = engine;
        #[cfg(unix)]
        if let Some(transport) = self
            .checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.transport.as_ref())
            .or_else(|| self.member.as_ref().map(|channel| channel.0.as_ref()))
        {
            engine
                .configure_checkpoint(transport)
                .map_err(|_| EngineError::LaunchFailed)?;
        }
        Ok(engine)
    }

    fn current(&self) -> Result<Arc<hl_native::Engine>, EngineError> {
        self.engine
            .lock()
            .map_err(|_| EngineError::Synchronization)?
            .clone()
            .ok_or(EngineError::NotStarted)
    }

    fn exit(engine: &hl_native::Engine) -> EngineExit {
        let exit = engine.exit();
        EngineExit {
            kind: match exit.kind {
                1 => ExitKind::Code,
                2 => ExitKind::Signal,
                3 => ExitKind::Fault,
                _ => ExitKind::EngineError,
            },
            guest_status: exit.status,
            detail: exit.detail,
            fault: None,
        }
    }
}

fn encode_environment_record(encoded: &mut Vec<u8>, record: &[u8]) {
    for byte in record {
        match byte {
            b'\\' => encoded.extend_from_slice(b"\\\\"),
            b'\n' => encoded.extend_from_slice(b"\\n"),
            byte => encoded.push(*byte),
        }
    }
}

impl GuestMachine for ProductionMachine {
    fn start(&self) -> Result<(), EngineError> {
        #[cfg(unix)]
        let recovery = if self.plan.options.get_bytes("HL_RESTORE").is_some() {
            let checkpoint = self.checkpoint.as_ref().ok_or(EngineError::LaunchFailed)?;
            // A refused recovery admission must name itself: the restore driver downstream is the only
            // other place that reports, and it never runs when admission refuses.
            Some(
                checkpoint
                    .begin_recovery(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)
                    .inspect_err(|error| eprintln!("[restore] refuse: recovery admission rejected: {error:?}"))?,
            )
        } else {
            None
        };
        let engine = Arc::new(self.create()?);
        *self.engine.lock().map_err(|_| EngineError::Synchronization)? = Some(Arc::clone(&engine));
        let arguments = self
            .plan
            .arguments
            .iter()
            .map(|argument| CString::new(argument.as_slice()).map_err(|_| EngineError::LaunchFailed))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers = arguments.iter().map(|argument| argument.as_ptr()).collect::<Vec<_>>();
        #[cfg(unix)]
        let run = if let Some(recovery) = recovery.as_ref() {
            // Recovery publication is completed by the restored process while
            // `run` is still waiting for that process to exit. Waiting only
            // after `run` returns lets a later checkpoint reuse the server
            // state first; the stale recovery waiter then observes that newer
            // generation and reports `Busy` despite a successful checkpoint.
            run_with_recovery(recovery, || engine.run(&pointers).map_err(native_run_failure))
        } else {
            engine.run(&pointers).map_err(native_run_failure)
        };
        #[cfg(not(unix))]
        let run = engine.run(&pointers).map_err(native_run_failure);
        run?;
        #[cfg(unix)]
        if let Some(terminal) = &self.terminal {
            terminal.flush();
        }
        #[cfg(unix)]
        if let Some(output) = &self.output {
            output.flush();
        }
        Ok(())
    }

    fn wait(&self) -> Result<EngineExit, EngineError> {
        let engine = self.current()?;
        Ok(Self::exit(engine.as_ref()))
    }

    fn stop(&self, request: StopRequest) -> Result<(), EngineError> {
        let (kind, signal) = match request {
            StopRequest::Interrupt => (REQUEST_INTERRUPT, request.signal()),
            StopRequest::Force => (REQUEST_FORCE_STOP, request.signal()),
            StopRequest::Signal(signal) => (REQUEST_SIGNAL, signal),
        };
        self.current()?
            .request(kind, signal)
            .map_err(EngineError::NativeStopFailed)
    }

    #[cfg(unix)]
    fn checkpoint_channel(&self) -> Option<crate::composition::CheckpointChannel> {
        self.checkpoint
            .as_ref()
            .map(|checkpoint| crate::composition::CheckpointChannel(Arc::clone(&checkpoint.transport)))
    }

    fn guest_pid(&self) -> Option<std::num::NonZeroI32> {
        self.current().ok().and_then(|engine| engine.guest_pid())
    }

    #[cfg(unix)]
    fn restored_member(&self, guest_pid: std::num::NonZeroI32) -> Option<crate::runtime::RestoredMember> {
        self.checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.server.restored_member(guest_pid))
            .map(crate::runtime::RestoredMember::new)
    }

    #[cfg(unix)]
    fn provide_member_terminal(
        &self,
        guest_pid: std::num::NonZeroI32,
        terminal: std::os::fd::OwnedFd,
    ) -> Result<(), EngineError> {
        let checkpoint = self.checkpoint.as_ref().ok_or(EngineError::Unsupported)?;
        checkpoint
            .server
            .register_member_terminal(guest_pid, terminal)
            .map_err(|reason| {
                hl_log::hl_error!(hl_log::tag::CHECKPOINT, "member terminal registration failed: {reason}");
                EngineError::Unsupported
            })
    }

    fn checkpoint_supported(&self) -> Result<(), EngineError> {
        if let Some(refusal) = checkpoint_sandbox_refusal(&self.plan.options) {
            return Err(refusal);
        }
        #[cfg(unix)]
        if self.checkpoint.is_some() {
            return Ok(());
        }
        Err(EngineError::Unsupported)
    }

    fn capture_checkpoint(&self) -> Result<(), EngineError> {
        self.capture_checkpoint_until(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)
    }

    fn capture_checkpoint_until(&self, deadline: std::time::Instant) -> Result<(), EngineError> {
        #[cfg(not(unix))]
        let _ = deadline;
        if let Some(refusal) = checkpoint_sandbox_refusal(&self.plan.options) {
            return Err(refusal);
        }
        #[cfg(unix)]
        if let Some(checkpoint) = &self.checkpoint {
            let engine = self.current()?;
            return checkpoint.capture(engine.as_ref(), self.isa, deadline);
        }
        Err(EngineError::Unsupported)
    }
}

/// Classify a launch that the checkpoint engine cannot capture under.
///
/// `HL_UNTRUSTED` forks the sentry and routes every host-authority syscall through it, so the
/// worker process that would dump itself does not own the descriptors, sockets or pipes the guest
/// sees; `ckpt_dump_self_locked` refuses on that gate. Capturing under the sentry requires the
/// sentry to participate in capture and restore -- exporting its descriptor table, open-file
/// descriptions and connection state across the control ring -- which is not implemented.
///
/// Reporting it here rather than as a bare native failure keeps the refusal on the launch-policy
/// boundary that owns the option, and makes it permanent so a preflight does not poll for it.
fn checkpoint_sandbox_refusal(options: &crate::options::Options) -> Option<EngineError> {
    options
        .get_bytes("HL_UNTRUSTED")
        .map(|_| EngineError::CheckpointUnsupportedUnderSandbox)
}

fn native_run_failure(status: i32) -> EngineError {
    EngineError::NativeRunFailed(status)
}

#[cfg(unix)]
struct CheckpointControl {
    server: Arc<Server>,
    transport: Arc<hl_native::CheckpointTransport>,
    acceptor: Option<std::thread::JoinHandle<()>>,
    phases: CheckpointPhaseLedger,
}

#[cfg(unix)]
impl CheckpointControl {
    fn start(
        sink: Arc<dyn CheckpointSink>,
        source: Arc<dyn CheckpointSource>,
        isa: crate::activation::GuestIsa,
        phase_ledger: Option<i32>,
        phase_clock_failure: bool,
    ) -> Result<Self, CompositionError> {
        let (broker, transport) = hl_native::CheckpointTransport::create().map_err(|error| {
            hl_log::hl_error!(hl_log::tag::EXEC, "checkpoint transport creation failed: error={error}");
            CompositionError::RuntimeConstruction
        })?;
        let server = Arc::new(Server::new(sink, source));
        let acceptor = Server::start(&server, broker);
        Ok(Self {
            server,
            transport: Arc::new(transport),
            acceptor: Some(acceptor),
            phases: CheckpointPhaseLedger::new(phase_ledger, phase_clock_failure, isa),
        })
    }

    fn capture(
        &self,
        engine: &hl_native::Engine,
        isa: crate::activation::GuestIsa,
        deadline: std::time::Instant,
    ) -> Result<(), EngineError> {
        use std::time::Instant;

        if Instant::now() >= deadline {
            self.phases.terminal(0, 1);
            return Err(EngineError::WaitFailed);
        }
        let ready = self.phases.begin();
        if let Err(failure) = self.server.wait_capture_ready(deadline) {
            self.phases.terminal(0, 1);
            return Err(Self::capture_failure(failure));
        }
        let admission = self.phases.begin();
        let capture = match self
            .server
            .begin_capture_after_admission(deadline, || self.transport.bump())
        {
            Ok(capture) => capture,
            Err(failure) => {
                self.phases.terminal(0, 1);
                return Err(Self::capture_failure(failure));
            }
        };
        self.phases.finish(capture, "capture_ready_wait", ready);
        self.phases.finish(capture, "capture_admission", admission);
        let signal = hl_native::CheckpointTransport::interrupt_signal(match isa {
            crate::activation::GuestIsa::Aarch64 => 1,
            crate::activation::GuestIsa::X86_64 => 2,
        });
        let dispatch = self.phases.begin();
        if signal <= 0 || engine.request(REQUEST_CHECKPOINT, signal).is_err() {
            if self.server.abort_capture(capture).is_err() {
                self.phases.terminal(capture, 1);
                return Err(EngineError::LaunchFailed);
            }
            self.phases.terminal(capture, 1);
            return Err(EngineError::StopFailed);
        }
        self.phases.finish(capture, "request_dispatch", dispatch);
        let completion = self.phases.begin();
        let result = await_capture_completion(&self.server, capture, deadline, || {
            let _ = engine.request(REQUEST_CHECKPOINT, signal);
        });
        match result {
            Ok(result) => {
                self.phases.finish(capture, "completion_wait", completion);
                self.phases.terminal(capture, u32::from(result.is_err()));
                result.map_err(|failure| self.capture_failure_reported(engine, failure))
            }
            Err(failure) => {
                self.phases.terminal(capture, 1);
                Err(Self::capture_failure(failure))
            }
        }
    }

    fn begin_recovery(&self, deadline: std::time::Instant) -> Result<RecoveryAdmission, EngineError> {
        let id = match self
            .server
            .begin_recovery_after_admission(deadline, || self.transport.bump())
        {
            Ok(id) => id,
            Err(failure) => {
                self.phases.terminal(0, 1);
                return Err(Self::capture_failure(failure));
            }
        };
        Ok(RecoveryAdmission {
            server: Arc::clone(&self.server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: self.phases,
        })
    }

    fn capture_failure(failure: super::checkpoint::CaptureFailure) -> EngineError {
        match failure {
            super::checkpoint::CaptureFailure::Busy => EngineError::Busy,
            super::checkpoint::CaptureFailure::Deadline => EngineError::WaitFailed,
            super::checkpoint::CaptureFailure::Unfinalized => EngineError::CheckpointGenerationUnfinalized,
            super::checkpoint::CaptureFailure::Failed => EngineError::CaptureFailed,
            super::checkpoint::CaptureFailure::Refused => EngineError::CaptureRefused,
            super::checkpoint::CaptureFailure::Poisoned => EngineError::CapturePoisoned,
        }
    }

    /// The error a failed capture reaches the caller as, having first said what the engine said.
    ///
    /// A refusal is a DECISION the engine already explained, and the explanation is the whole value of
    /// it. Sending one through the exit-status probe below would replace that explanation with a bare
    /// `CheckpointExited`, which is exactly how a named cause used to become an anonymous one on the
    /// way out of the engine.
    fn capture_failure_reported(
        &self,
        engine: &hl_native::Engine,
        failure: super::checkpoint::CaptureFailure,
    ) -> EngineError {
        if failure == super::checkpoint::CaptureFailure::Refused {
            if let Some(reason) = self.server.capture_refusal() {
                hl_log::hl_error!(hl_log::tag::CHECKPOINT, "checkpoint refused by the engine: {reason}");
            }
            return EngineError::CaptureRefused;
        }
        Self::capture_failure_with_exit(engine, failure)
    }

    fn capture_failure_with_exit(
        engine: &hl_native::Engine,
        failure: super::checkpoint::CaptureFailure,
    ) -> EngineError {
        // A channel EOF is observed just before the native child-reaper publishes its status. Give that
        // publication a tightly bounded opportunity to win so a host SIGSEGV is reported as Signal(11)
        // instead of the generic capture failure that triggered this diagnostic path.
        for _ in 0..10 {
            let native = engine.exit();
            if native.kind != 0 {
                return EngineError::CheckpointExited(ProductionMachine::exit(engine));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Self::capture_failure(failure)
    }
}

/// Wait for a dispatched capture to reach a terminal result, re-issuing the guest checkpoint
/// interrupt every 100ms until it does.
///
/// The wait is bounded by `deadline` here and not only by the deadline the server recorded with
/// the capture: `Server::wait_capture` returns `Ok(None)` from phases that carry no deadline of
/// their own (an in-flight abort settlement), so a loop that trusted the server to expire the
/// capture would re-interrupt a stalled guest forever. Exceeding the deadline aborts the capture
/// and reports `CaptureFailure::Deadline`, naming the completion wait as the stalled phase.
#[cfg(unix)]
pub(super) fn await_capture_completion(
    server: &Server,
    capture: u64,
    deadline: std::time::Instant,
    mut reinterrupt: impl FnMut(),
) -> Result<Result<(), super::checkpoint::CaptureFailure>, super::checkpoint::CaptureFailure> {
    use std::time::{Duration, Instant};

    let mut next_interrupt = Instant::now() + Duration::from_millis(100);
    loop {
        match server.wait_capture(capture, next_interrupt.min(deadline)) {
            Ok(Some(result)) => return Ok(result),
            Ok(None) => {}
            Err(failure) => return Err(failure),
        }
        let now = Instant::now();
        if now >= deadline {
            let _ = server.abort_capture(capture);
            return Err(super::checkpoint::CaptureFailure::Deadline);
        }
        if now >= next_interrupt {
            reinterrupt();
            next_interrupt = now + Duration::from_millis(100);
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct CheckpointPhaseLedger {
    descriptor: Option<i32>,
    clock_failure: bool,
    isa: crate::activation::GuestIsa,
}

#[cfg(unix)]
impl CheckpointPhaseLedger {
    const fn new(descriptor: Option<i32>, clock_failure: bool, isa: crate::activation::GuestIsa) -> Self {
        Self {
            descriptor,
            clock_failure,
            isa,
        }
    }

    fn now(self) -> u64 {
        if self.descriptor.is_none() {
            return u64::MAX;
        }
        if self.clock_failure {
            return 0;
        }
        let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: `now` is writable storage of the exact ABI type; CLOCK_MONOTONIC
        // retains no pointer and invokes no callback.
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) } != 0 {
            return 0;
        }
        u64::try_from(now.tv_sec)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000))
            .and_then(|micros| u64::try_from(now.tv_nsec).ok().map(|nanos| micros + nanos / 1_000))
            .unwrap_or(0)
    }

    fn begin(self) -> u64 {
        self.now()
    }

    fn finish(self, generation: u64, phase: &str, started: u64) {
        if started == u64::MAX {
            return;
        }
        let finished = self.now();
        let (duration, clock) = if started == 0 || finished == 0 {
            (0, "unavailable")
        } else {
            (finished.saturating_sub(started), "ok")
        };
        self.emit(format!(
            "checkpoint_phase_ledger\tcomponent=control\tisa={}\tsession={generation}\tattempt={generation}\tgeneration={generation}\tphase={phase}\tduration_us={duration}\tbudget_us=0\tclock={clock}\toutcome=progress\tstatus=0",
            match self.isa {
                crate::activation::GuestIsa::Aarch64 => "aarch64",
                crate::activation::GuestIsa::X86_64 => "x86_64",
            }
        ));
    }

    fn terminal(self, generation: u64, status: u32) {
        if self.descriptor.is_none() {
            return;
        }
        static NEXT_UNADMITTED_ATTEMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let correlation = if generation == 0 {
            NEXT_UNADMITTED_ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        } else {
            generation
        };
        let clock = if self.now() == 0 { "unavailable" } else { "ok" };
        self.emit(format!(
            "checkpoint_phase_ledger\tcomponent=control\tisa={}\tsession={correlation}\tattempt={correlation}\tgeneration={generation}\tphase=terminal\tduration_us=0\tbudget_us=0\tclock={}\toutcome={}\tstatus={status}",
            match self.isa {
                crate::activation::GuestIsa::Aarch64 => "aarch64",
                crate::activation::GuestIsa::X86_64 => "x86_64",
            },
            clock,
            if status == 0 { "success" } else { "failure" },
        ));
    }

    fn emit(self, mut record: String) {
        let Some(descriptor) = self.descriptor else { return };
        record.push('\n');
        // SAFETY: the test harness owns this inherited append-only descriptor for
        // the subprocess lifetime; the bounded record is borrowed for one write.
        let written = unsafe { libc::write(descriptor, record.as_ptr().cast(), record.len()) };
        assert_eq!(
            written,
            isize::try_from(record.len()).unwrap(),
            "checkpoint phase ledger write"
        );
    }
}

#[cfg(unix)]
struct RecoveryAdmission {
    server: Arc<Server>,
    id: u64,
    state: std::sync::atomic::AtomicU8,
    phases: CheckpointPhaseLedger,
}

#[cfg(unix)]
const RECOVERY_OPEN: u8 = 0;
#[cfg(unix)]
const RECOVERY_WAIT_CLAIMED: u8 = 1;
#[cfg(unix)]
const RECOVERY_RETURNED_NEEDS_ABORT: u8 = 2;
#[cfg(unix)]
const RECOVERY_SETTLED: u8 = 3;

#[cfg(unix)]
impl Drop for RecoveryAdmission {
    fn drop(&mut self) {
        let state = self.state.load(std::sync::atomic::Ordering::Acquire);
        if state != RECOVERY_SETTLED {
            let _ = self.abort_recovery();
            if state == RECOVERY_OPEN {
                self.phases.terminal(self.id, 1);
            }
        }
    }
}

#[cfg(unix)]
impl RecoveryAdmission {
    fn abort(&self) -> Result<(), EngineError> {
        self.abort_recovery().map_err(CheckpointControl::capture_failure)
    }

    fn abort_recovery(&self) -> Result<(), super::checkpoint::CaptureFailure> {
        let result = self.server.abort_recovery(self.id);
        if result == Err(super::checkpoint::CaptureFailure::Poisoned) {
            // capture_lock reports mutex poison once after converting the phase to Poisoned and
            // clearing the mutex flag. No replacement generation can admit in that phase, so one
            // bounded retry safely performs the transaction discard without reopening an ABA gap.
            self.server.abort_recovery(self.id)
        } else {
            result
        }
    }

    fn wait(&self) -> Result<(), EngineError> {
        let started = self.phases.begin();
        if self
            .state
            .compare_exchange(
                RECOVERY_OPEN,
                RECOVERY_WAIT_CLAIMED,
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
            )
            .is_err()
        {
            return Err(EngineError::Busy);
        }
        let result = self.server.wait_recovery(self.id);
        let state = if matches!(result, Err(super::checkpoint::CaptureFailure::Poisoned)) {
            RECOVERY_RETURNED_NEEDS_ABORT
        } else {
            RECOVERY_SETTLED
        };
        self.state.store(state, std::sync::atomic::Ordering::Release);
        self.phases.finish(self.id, "recovery_wait", started);
        self.phases.terminal(self.id, u32::from(result.is_err()));
        result.map_err(CheckpointControl::capture_failure)
    }
}

#[cfg(unix)]
fn run_with_recovery<T>(
    recovery: &RecoveryAdmission,
    run: impl FnOnce() -> Result<T, EngineError>,
) -> Result<T, EngineError> {
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| recovery.wait());
        let run = run();
        if run.is_err() {
            // A launch can fail before native code adopts a checkpoint channel.
            // With no broker EOF to wake the waiter, settle at the failure site.
            let _ = recovery.abort();
        }
        let restored = waiting.join().map_err(|_| EngineError::WaitFailed)?;
        restored?;
        run
    })
}

#[cfg(unix)]
impl Drop for CheckpointControl {
    fn drop(&mut self) {
        self.server.stop();
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    struct EmptyCheckpointStore;

    #[cfg(unix)]
    impl crate::composition::CheckpointSink for EmptyCheckpointStore {
        fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn begin_until(&self, _: std::time::Instant) -> Result<std::num::NonZeroU64, CompositionError> {
            Ok(std::num::NonZeroU64::MIN)
        }

        fn put_until(
            &self,
            _: std::num::NonZeroU64,
            _: &str,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn abort_until(&self, _: std::num::NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
            Ok(())
        }

        fn commit_until(
            &self,
            _: std::num::NonZeroU64,
            _: &[u8],
            _: std::time::Instant,
        ) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }
    }

    #[cfg(unix)]
    impl crate::composition::CheckpointSource for EmptyCheckpointStore {
        fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }

        fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
            // Recovery refuses a generation carrying no manifest, so a store that
            // admits recovery must present a finalized one.
            Ok(vec![String::from("MANIFEST")])
        }
    }

    /// A staged generation never becomes finalized while this launch is waiting,
    /// so the checkpoint preflight must surface the refusal instead of polling
    /// until its deadline.
    ///
    /// `#[cfg(unix)]` because both names in the body are: `CheckpointControl` is
    /// declared under one in this file, and `super::super::checkpoint` under one in
    /// `runtime/api.rs`. The gate belongs on the test rather than on the module,
    /// because the module is a checkpoint coordinator that passes descriptors over an
    /// `AF_UNIX` socket with `SCM_RIGHTS` -- widening it to Windows is a port, not a
    /// `cfg` edit. Everything else in this `mod tests` that names either already
    /// carries the same gate; this one item did not, and nothing built the
    /// configuration that would have said so.
    #[cfg(unix)]
    #[test]
    fn an_unfinalized_generation_is_a_permanent_recovery_refusal() {
        assert_eq!(
            CheckpointControl::capture_failure(super::super::checkpoint::CaptureFailure::Unfinalized),
            EngineError::CheckpointGenerationUnfinalized
        );
        assert!(EngineError::CheckpointGenerationUnfinalized.is_permanent_refusal());
    }

    #[test]
    fn native_run_status_survives_the_engine_boundary() {
        assert_eq!(native_run_failure(13), EngineError::NativeRunFailed(13));
    }

    #[cfg(unix)]
    #[test]
    fn dropped_recovery_admission_releases_scope_and_rejects_stale_id() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let first = server
            .begin_recovery(11, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        {
            let _admission = RecoveryAdmission {
                server: Arc::clone(&server),
                id: first,
                state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
                phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
            };
        }
        let second = server
            .begin_recovery(12, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            server.abort_recovery(first),
            Err(super::super::checkpoint::CaptureFailure::Busy)
        );
        server.abort_recovery(second).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn pre_channel_run_failure_aborts_recovery_without_waiting_for_deadline() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(21, std::time::Instant::now() + std::time::Duration::from_secs(5))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let started = std::time::Instant::now();
        assert_eq!(
            run_with_recovery(&admission, || Err::<(), _>(EngineError::NativeRunFailed(7))),
            Err(EngineError::NativeRunFailed(7))
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(250));
        assert_eq!(admission.wait(), Err(EngineError::Busy));
        let retry = server
            .begin_recovery(22, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        server.abort_recovery(retry).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dropping_consumed_admission_cannot_abort_reused_generation() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(23, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        admission.abort().unwrap();
        let _ = admission.wait();

        let reused = server
            .begin_recovery(23, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        drop(admission);
        server
            .fail_recovery(reused, super::super::checkpoint::CaptureFailure::Deadline)
            .expect("a consumed admission must not abort the reused generation");
        assert_eq!(
            server.wait_recovery(reused),
            Err(super::super::checkpoint::CaptureFailure::Deadline)
        );
    }

    #[cfg(unix)]
    #[test]
    fn poisoned_wait_is_aborted_by_admission_drop_and_allows_retry() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(24, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let poison = Arc::clone(&server);
        let _ = std::thread::spawn(move || poison.poison_coordination()).join();

        // A poisoned capture ledger is not a launch failure. Naming it as one is what put
        // "LaunchFailed" in front of a desktop user whose workspace had launched fine.
        assert_eq!(admission.wait(), Err(EngineError::CapturePoisoned));
        drop(admission);
        let retry = server
            .begin_recovery(25, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .expect("dropping a poisoned admission must release its recovery transaction");
        server.abort_recovery(retry).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn poisoned_unwaited_admission_drop_allows_retry() {
        let store = Arc::new(EmptyCheckpointStore);
        let server = Arc::new(Server::new(store.clone(), store));
        let id = server
            .begin_recovery(26, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let admission = RecoveryAdmission {
            server: Arc::clone(&server),
            id,
            state: std::sync::atomic::AtomicU8::new(RECOVERY_OPEN),
            phases: CheckpointPhaseLedger::new(None, false, crate::activation::GuestIsa::Aarch64),
        };
        let poison = Arc::clone(&server);
        let _ = std::thread::spawn(move || poison.poison_coordination()).join();

        drop(admission);
        let retry = server
            .begin_recovery(27, std::time::Instant::now() + std::time::Duration::from_secs(1))
            .expect("dropping an unwaited poisoned admission must release its recovery transaction");
        server.abort_recovery(retry).unwrap();
    }
}

#[cfg(test)]
mod sandbox_refusal_tests {
    use super::*;

    /// `Sandbox::SentryOnly` is the container default, so this is the ordinary launch. A checkpoint
    /// of it must refuse with a cause the product can show, not with a bare native failure, and the
    /// refusal must be permanent so the checkpoint preflight reports it instead of polling for 30s.
    #[test]
    fn a_sentry_launch_is_refused_permanently_with_its_own_cause() {
        let mut options = crate::options::Options::default();
        options.set("HL_UNTRUSTED", "1", true).unwrap();
        assert_eq!(
            checkpoint_sandbox_refusal(&options),
            Some(EngineError::CheckpointUnsupportedUnderSandbox)
        );
        assert!(EngineError::CheckpointUnsupportedUnderSandbox.is_permanent_refusal());
    }

    #[test]
    fn a_launch_without_the_sentry_is_not_refused_by_policy() {
        assert_eq!(checkpoint_sandbox_refusal(&crate::options::Options::default()), None);
    }
}

#[cfg(all(test, unix))]
mod rootfs_ownership_tests {
    use super::{refuse_unownable_root, writable_root};
    use crate::engine::EngineError;
    use crate::launcher::plan::RuntimePlan;
    use crate::options::Options;

    fn plan(rootfs: Option<&str>, options: &[(&str, &str)]) -> RuntimePlan {
        let mut set = Options::default();
        for (name, value) in options {
            set.set(name, value, true).unwrap();
        }
        RuntimePlan {
            rootfs: rootfs.map(|path| path.as_bytes().to_vec()),
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options: set,
        }
    }

    /// `/` is owned by host root on every host this runs on, so for an engine that is not root it is
    /// the portable stand-in for a rootfs unpacked under `sudo` -- and it needs no privilege to set
    /// up. It says nothing when the suite itself runs as root, which is the case the test below
    /// covers separately.
    fn host_root_owned() -> &'static str {
        "/"
    }

    /// `refuse_unownable_root` has two acceptance branches and one refusal, and which of them a run
    /// exercises is decided by the identity the suite happens to have. The root arm used to be a
    /// bare `return;`, so on a host that runs the suite as uid 0 -- as this repository's Linux box
    /// does -- the case asserted nothing and reported `ok`, and `engine_uid == 0` had no coverage on
    /// any host. Assert the branch this identity actually reaches instead of leaving the run empty.
    ///
    /// `/` cannot express the root arm, because root owns it and the `rootfs_uid == engine_uid` arm
    /// would answer first. The fixture therefore hands a directory to another uid, so only
    /// `engine_uid == 0` can account for the acceptance.
    #[test]
    fn a_writable_root_owned_by_another_host_user_refuses_a_launch_that_is_not_root() {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` takes no arguments and cannot fail.
        let engine_uid = unsafe { libc::geteuid() };
        if engine_uid != 0 {
            assert_eq!(
                refuse_unownable_root(&plan(Some(host_root_owned()), &[])),
                Err(EngineError::RootfsNotOwnedByEngine {
                    rootfs_uid: 0,
                    engine_uid,
                })
            );
            return;
        }
        const FOREIGN_UID: u32 = 65_534;
        let directory = tempfile::tempdir().unwrap();
        std::os::unix::fs::chown(directory.path(), Some(FOREIGN_UID), None).unwrap();
        assert_eq!(
            std::fs::metadata(directory.path()).unwrap().uid(),
            FOREIGN_UID,
            "the fixture root must belong to another uid or the acceptance proves nothing"
        );
        assert_eq!(
            refuse_unownable_root(&plan(Some(directory.path().to_str().unwrap()), &[])),
            Ok(()),
            "host root writes through every owner, so no ownership refusal may fire for uid 0"
        );
    }

    /// The kinder-to-refuse judgement stops exactly where the workspace stops being broken. A
    /// read-only launch writes nothing, so a root-owned tree serves it perfectly well and refusing
    /// it would take away a shape that works.
    #[test]
    fn a_read_only_launch_over_the_same_root_is_still_admitted() {
        assert_eq!(
            writable_root(&plan(Some(host_root_owned()), &[("HL_ROOTFS_RO", "1")])),
            None
        );
        assert_eq!(
            refuse_unownable_root(&plan(Some(host_root_owned()), &[("HL_ROOTFS_RO", "1")])),
            Ok(())
        );
    }

    /// With an overlay the lower layers are read-only by construction and every write lands in the
    /// upper, so the upper is the only ownership that decides whether the workspace works.
    #[test]
    fn an_overlay_is_judged_by_its_upper_layer_not_by_a_root_owned_lower() {
        let directory = tempfile::tempdir().unwrap();
        let upper = directory.path().to_str().unwrap();
        let over_root = plan(Some(host_root_owned()), &[("HL_OVERLAY_UPPER", upper)]);
        assert_eq!(writable_root(&over_root), Some(upper.as_bytes()));
        assert_eq!(refuse_unownable_root(&over_root), Ok(()));
    }

    #[test]
    fn a_root_the_engine_owns_and_a_launch_without_one_are_both_admitted() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            refuse_unownable_root(&plan(Some(directory.path().to_str().unwrap()), &[])),
            Ok(())
        );
        assert_eq!(refuse_unownable_root(&plan(None, &[])), Ok(()));
    }

    /// A rootfs that is not there is the existing launch path's error to report, and an ownership
    /// cause for a missing directory would be a worse diagnostic than the one it replaced.
    #[test]
    fn a_missing_root_is_left_to_the_launch_path_that_already_owns_it() {
        assert_eq!(
            refuse_unownable_root(&plan(Some("/var/tmp/husklet-no-such-rootfs-6f21"), &[])),
            Ok(())
        );
    }
}
