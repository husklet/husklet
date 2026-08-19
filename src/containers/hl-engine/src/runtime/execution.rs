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
use super::terminal::NativeTerminalBridge;

const REQUEST_INTERRUPT: u32 = 1;
const REQUEST_FORCE_STOP: u32 = 2;
const REQUEST_SIGNAL: u32 = 3;
#[cfg(unix)]
const REQUEST_CHECKPOINT: u32 = 4;

pub(crate) struct ProductionMachine {
    isa: crate::activation::GuestIsa,
    plan: crate::launcher::plan::RuntimePlan,
    #[cfg(unix)]
    terminal: Option<NativeTerminalBridge>,
    #[cfg(unix)]
    output: Option<NativeOutputBridge>,
    #[cfg(unix)]
    checkpoint: Option<CheckpointControl>,
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
            .map(NativeTerminalBridge::attach)
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
                request.plan.options.get_bytes("HL_CHECKPOINT_PHASE_CLOCK_FAIL").is_some(),
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

    fn create(&self) -> Result<hl_native::Engine, EngineError> {
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
        if let Some(checkpoint) = &self.checkpoint {
            engine
                .configure_checkpoint(&checkpoint.transport)
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
            Some(
                checkpoint
                    .begin_recovery(std::time::Instant::now() + crate::composition::DEFAULT_CHECKPOINT_TIMEOUT)?,
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
    transport: hl_native::CheckpointTransport,
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
        let (broker, transport) =
            hl_native::CheckpointTransport::create().map_err(|error| {
                hl_log::hl_error!(hl_log::tag::EXEC, "checkpoint transport creation failed: error={error}");
                CompositionError::RuntimeConstruction
            })?;
        let server = Arc::new(Server::new(sink, source));
        let acceptor = Server::start(&server, broker);
        Ok(Self {
            server,
            transport,
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
                result.map_err(|failure| Self::capture_failure_with_exit(engine, failure))
            }
            Err(failure) => {
                self.phases.terminal(capture, 1);
                Err(match failure {
                    // The guest never reached its dump safepoint inside the checkpoint deadline.
                    super::checkpoint::CaptureFailure::Deadline => EngineError::WaitFailed,
                    _ => EngineError::LaunchFailed,
                })
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
            super::checkpoint::CaptureFailure::Failed | super::checkpoint::CaptureFailure::Poisoned => {
                EngineError::LaunchFailed
            }
        }
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
            Err(CompositionError::RuntimeConstruction)
        }
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

        assert_eq!(admission.wait(), Err(EngineError::LaunchFailed));
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
