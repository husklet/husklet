use super::*;
use crate::runtime::checkpoint::CaptureFailure;

pub(super) fn checkpoint_sandbox_refusal(options: &crate::options::Options) -> Option<EngineError> {
    options
        .get_bytes("HL_UNTRUSTED")
        .map(|_| EngineError::CheckpointUnsupportedUnderSandbox)
}
pub(super) fn native_run_failure(status: i32) -> EngineError {
    EngineError::NativeRunFailed(status)
}

#[cfg(unix)]
pub(super) struct CheckpointControl {
    pub(super) server: Arc<Server>,
    pub(super) transport: Arc<hl_native::CheckpointTransport>,
    acceptor: Option<std::thread::JoinHandle<()>>,
    phases: CheckpointPhaseLedger,
}

#[cfg(unix)]
impl CheckpointControl {
    pub(super) fn start(
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

    pub(super) fn capture(
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

    pub(super) fn begin_recovery(&self, deadline: std::time::Instant) -> Result<RecoveryAdmission, EngineError> {
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

    pub(super) fn capture_failure(failure: CaptureFailure) -> EngineError {
        match failure {
            CaptureFailure::Busy => EngineError::Busy,
            CaptureFailure::Deadline => EngineError::WaitFailed,
            CaptureFailure::Unfinalized => EngineError::CheckpointGenerationUnfinalized,
            CaptureFailure::Failed => EngineError::CaptureFailed,
            CaptureFailure::Refused => EngineError::CaptureRefused,
            CaptureFailure::Poisoned => EngineError::CapturePoisoned,
        }
    }

    /// The error a failed capture reaches the caller as, having first said what the engine said.
    ///
    /// A refusal is a DECISION the engine already explained, and the explanation is the whole value of
    /// it. Sending one through the exit-status probe below would replace that explanation with a bare
    /// `CheckpointExited`, which is exactly how a named cause used to become an anonymous one on the
    /// way out of the engine.
    fn capture_failure_reported(&self, engine: &hl_native::Engine, failure: CaptureFailure) -> EngineError {
        if failure == CaptureFailure::Refused {
            if let Some(reason) = self.server.capture_refusal() {
                hl_log::hl_error!(hl_log::tag::CHECKPOINT, "checkpoint refused by the engine: {reason}");
            }
            return EngineError::CaptureRefused;
        }
        Self::capture_failure_with_exit(engine, failure)
    }

    fn capture_failure_with_exit(engine: &hl_native::Engine, failure: CaptureFailure) -> EngineError {
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
pub(crate) fn await_capture_completion(
    server: &Server,
    capture: u64,
    deadline: std::time::Instant,
    mut reinterrupt: impl FnMut(),
) -> Result<Result<(), CaptureFailure>, CaptureFailure> {
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
            return Err(CaptureFailure::Deadline);
        }
        if now >= next_interrupt {
            reinterrupt();
            next_interrupt = now + Duration::from_millis(100);
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy)]
pub(super) struct CheckpointPhaseLedger {
    descriptor: Option<i32>,
    clock_failure: bool,
    isa: crate::activation::GuestIsa,
}

#[cfg(unix)]
impl CheckpointPhaseLedger {
    pub(super) const fn new(descriptor: Option<i32>, clock_failure: bool, isa: crate::activation::GuestIsa) -> Self {
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
pub(super) struct RecoveryAdmission {
    pub(super) server: Arc<Server>,
    pub(super) id: u64,
    pub(super) state: std::sync::atomic::AtomicU8,
    pub(super) phases: CheckpointPhaseLedger,
}

#[cfg(unix)]
pub(super) const RECOVERY_OPEN: u8 = 0;
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
            // Unwind cannot carry this out, and an admission that fails to abort leaves the server's
            // recovery transaction open: the next generation is refused for a reason that names
            // neither this id nor this failure unless the discard reports itself here.
            if let Err(failure) = self.abort_recovery() {
                hl_log::hl_error!(
                    hl_log::tag::CHECKPOINT,
                    "recovery admission {} was dropped in state {state} and its transaction discard failed: {failure:?}",
                    self.id
                );
            }
            if state == RECOVERY_OPEN {
                self.phases.terminal(self.id, 1);
            }
        }
    }
}

#[cfg(unix)]
impl RecoveryAdmission {
    pub(super) fn abort(&self) -> Result<(), EngineError> {
        self.abort_recovery().map_err(CheckpointControl::capture_failure)
    }

    fn abort_recovery(&self) -> Result<(), CaptureFailure> {
        let result = self.server.abort_recovery(self.id);
        if result == Err(CaptureFailure::Poisoned) {
            // capture_lock reports mutex poison once after converting the phase to Poisoned and
            // clearing the mutex flag. No replacement generation can admit in that phase, so one
            // bounded retry safely performs the transaction discard without reopening an ABA gap.
            self.server.abort_recovery(self.id)
        } else {
            result
        }
    }

    pub(super) fn wait(&self) -> Result<(), EngineError> {
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
        let state = if matches!(result, Err(CaptureFailure::Poisoned)) {
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
pub(super) fn run_with_recovery<T>(
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
