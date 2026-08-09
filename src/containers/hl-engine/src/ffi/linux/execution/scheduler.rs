use hl_execution::StepOutcome;
use hl_runtime::TraceBoundary;
use std::sync::Arc;

use super::{GuestExecutor, threads, waiter};

mod classify;
mod dispatch;
mod native;
mod pool;

pub(super) use pool::NativePool;
const SLICE_BUDGET: u64 = 4096;
const NATIVE_SOLO_BUDGET: u64 = 65_536;
/// Ceiling on the instructions one native activation may be granted in total.
/// A peer waiting for a scheduler turn counts as parked, so a compute-bound
/// guest thread would otherwise keep extending its own grant forever.
const NATIVE_ACTIVATION_BUDGET: u64 = 1 << 20;
const INLINE_SERVICE_LIMIT: u64 = 64;
const QUANTUM_TURNS: u64 = 4096;
/// Turns of the retry cycle without a dispatch before the scheduler reports a livelock.
const STALLED_TURNS: u64 = 1 << 22;
const NATIVE_SITE_LIMIT: usize = 65_536;
const NATIVE_SOURCE_LIMIT: usize = 65_536;
const NATIVE_BOUNDARY_CAPACITY: usize = 16;

pub(super) enum ThreadTerminal {
    Thread(EngineExit),
    Group(EngineExit),
}

pub(super) enum TurnAction {
    Continue,
    Dispatch,
    Error(EngineError),
    Replace(u64),
    Signal {
        signal: u8,
        code: i32,
        address: u64,
        fallback: EngineExit,
    },
    Terminal(ThreadTerminal),
}

pub(super) struct TurnResult {
    pub(super) run: threads::ThreadRun,
    pub(super) action: TurnAction,
}
use crate::activation::GuestIsa;
use crate::engine::{EngineError, EngineExit};
use crate::launch_plan::RuntimeLaunchPlan;

pub(super) enum CompletionTurn {
    Idle,
    Continue,
    Exit(EngineExit),
}

pub(super) enum SignalTurn {
    Continue,
    Park,
    GatePark,
    Terminal(ThreadTerminal),
}

impl GuestExecutor {
    const fn epoch_rewinds(executed: u64) -> bool {
        executed == 0
    }

    const fn x86_yield_needs_interpreter(exit: crate::native::NativeExit, executed: u64) -> bool {
        matches!(exit, crate::native::NativeExit::Yield) && executed == 0
    }

    fn native_boundary(
        cpu: &mut hl_execution::Aarch64CpuState,
        original: hl_execution::Aarch64CpuState,
        exit: crate::native::NativeExit,
        instruction: u64,
        executed: u64,
    ) -> Option<StepOutcome> {
        match exit {
            crate::native::NativeExit::Epoch => {
                if Self::epoch_rewinds(executed) {
                    *cpu = original;
                }
                Some(StepOutcome::Yield)
            }
            crate::native::NativeExit::Interrupt => {
                // The native stub has already spilled an exact architectural
                // boundary. Its instruction field is the guest PC at that
                // boundary; discarding the spill rewinds registers and gives
                // an asynchronous signal a stale ucontext PC.
                cpu.pc = instruction;
                Some(StepOutcome::Yield)
            }
            _ => None,
        }
    }

    pub(super) fn schedule(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        waiters: &waiter::Pool,
        host_faults: Option<Arc<dyn crate::native::HostFaultOwner>>,
    ) -> Result<EngineExit, EngineError> {
        struct FaultAttachment(Arc<dyn crate::native::HostFaultOwner>);
        impl Drop for FaultAttachment {
            fn drop(&mut self) {
                self.0.detach();
            }
        }

        /// Reports stranded live threads on every exit path, including error returns.
        struct LostReport<'a>(&'a threads::ThreadSet);
        impl Drop for LostReport<'_> {
            fn drop(&mut self) {
                let lost = self.0.lost_completions();
                if lost != 0 {
                    hl_log::hl_error!(hl_log::tag::TASK, "scheduler exited with stranded threads lost={lost}");
                }
            }
        }

        let _lost = LostReport(threads);
        let mut turns = 0_u64;
        let mut stalled = 0_u64;
        let host_faults = NativePool::selected(isa, plan)
            .then(|| NativePool::production_faults(host_faults))
            .flatten();
        let attachment = host_faults
            .as_ref()
            .and_then(|owner| owner.attach().ok().map(|()| FaultAttachment(Arc::clone(owner))));
        let active_faults = attachment.as_ref().and(host_faults);
        let mut native = NativePool::new(isa, plan, active_faults);
        let mut live = std::collections::BTreeSet::new();
        loop {
            if native.tracks_processes()
                && let Some(updated) = threads.active_processes_changed(&live)
            {
                live = updated;
                native.retain_processes(&live);
            }
            if let Some(exit) = Self::cancelled(plan, threads)? {
                return Ok(exit);
            }
            turns += 1;
            if turns == QUANTUM_TURNS {
                // A guest may legitimately need an unbounded number of bounded
                // execution slices. Relinquish the host CPU at each quantum;
                // the API supervisor owns elapsed-time policy and cancellation.
                std::thread::yield_now();
                turns = 0;
            }
            stalled += 1;
            if stalled == STALLED_TURNS {
                // Every turn since the last dispatch re-entered the retry cycle
                // without charging a slice, so no guest budget is being consumed.
                hl_log::hl_error!(
                    hl_log::tag::EXEC,
                    "scheduler retry cycle made no progress turns={stalled} processes={} lost={}",
                    threads.active_processes().len(),
                    threads.lost_completions(),
                );
            }
            match Self::complete(isa, plan, threads, waiters)? {
                CompletionTurn::Idle => {}
                CompletionTurn::Continue => continue,
                CompletionTurn::Exit(exit) => return Ok(exit),
            }
            let Some(run) = threads.next() else {
                match Self::idle(isa, plan, threads, waiters)? {
                    CompletionTurn::Exit(exit) => return Ok(exit),
                    CompletionTurn::Idle | CompletionTurn::Continue => continue,
                }
            };
            stalled = 0;
            let cpu_account = run.cpu_account.clone();
            let mut charged_at = Self::thread_cpu();
            let advanced = Self::advance(isa, plan, threads, waiters, run, &mut native, &mut charged_at);
            Self::charge_elapsed(cpu_account.as_deref(), &mut charged_at);
            if let Some(exit) = advanced? {
                return Ok(exit);
            }
        }
    }

    fn thread_cpu() -> Option<u64> {
        // Both supported hosts project CLOCK_THREAD_CPUTIME_ID. If a future
        // host cannot, leave accounting unchanged instead of charging wall
        // time that includes descheduling or blocked host work.
        crate::native::HostSyscalls::clock_ns(&crate::ffi::LinuxHost, crate::native::ClockKind::ThreadCpu).ok()
    }

    fn charge_elapsed(account: Option<&hl_task::CpuAccount>, charged_at: &mut Option<u64>) {
        let Some(finished) = Self::thread_cpu() else { return };
        if let Some(started) = charged_at.replace(finished)
            && let Some(account) = account
        {
            account.charge(finished.saturating_sub(started));
        }
    }

    fn advance(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        waiters: &waiter::Pool,
        run: threads::ThreadRun,
        native: &mut NativePool,
        charged_at: &mut Option<u64>,
    ) -> Result<Option<EngineExit>, EngineError> {
        let resumed = match Self::trace_resume(&run) {
            Ok(boundary) => boundary,
            Err(error) => return Self::apply_error(threads, run, error),
        };
        match resumed {
            TraceBoundary::Park => {
                if let Err(error) = threads.park(run.thread) {
                    return Self::apply_error(threads, run, Self::thread_error(error));
                }
                return Ok(None);
            }
            TraceBoundary::Dispatch => {
                return Self::dispatch_ready(isa, plan, threads, waiters, run, native);
            }
            TraceBoundary::Kill => {
                return Self::finish(plan, threads, run, ThreadTerminal::Thread(Self::signal(9)));
            }
            TraceBoundary::Continue => {}
            TraceBoundary::Signal(signal) => {
                if run.router.resolve_trace_signal(signal).is_err() {
                    return Self::apply_error(threads, run, EngineError::LaunchFailed);
                }
            }
        }
        let signal = match Self::signal_boundary(threads, &run) {
            Ok(signal) => signal,
            Err(error) => return Self::apply_error(threads, run, error),
        };
        match signal {
            SignalTurn::Park => {
                if let Err(error) = threads.park(run.thread) {
                    return Self::apply_error(threads, run, Self::thread_error(error));
                }
                return Ok(None);
            }
            SignalTurn::GatePark => return Ok(None),
            SignalTurn::Terminal(terminal) => return Self::finish(plan, threads, run, terminal),
            SignalTurn::Continue => {}
        }
        if let Err(error) = threads.acknowledge_interrupt(run.thread) {
            return Self::apply_error(threads, run, Self::thread_error(error));
        }
        let mut run = run;
        let mut serviced = 0;
        // The retained C dispatcher resumes the same spilled CPU and code cache
        // after a synchronous service. Do the same only while this run remains
        // the sole runnable owner. `execute_turn` has returned here, so its
        // checkpoint admission, mapping transaction, projections, write
        // reservations, and host pins have all been published or dropped before
        // the router is entered. Every service rechecks blocking classification,
        // ptrace entry/exit boundaries, asynchronous interruption, runnable
        // ownership, and parked peers; the fixed bound restores the ordinary
        // signal, CPU-timer, cancellation, accounting, and scheduling boundary.
        loop {
            // Capture one generation-qualified scheduler grant before extending
            // this activation. A queued state transition publishes against the
            // grant before waiting for the scheduler lock, so neither the solo
            // native budget nor the following inline service can race past it.
            let continuation = threads.continuation(&run);
            let may_extend = continuation
                .as_ref()
                .is_some_and(threads::SchedulerContinuation::is_current);
            let native_budget = Self::native_budget(may_extend);
            let poll_continuation = (!native.boundary_sensitive.contains(&run.process))
                .then_some(continuation.as_ref())
                .flatten();
            let result = Self::execute_turn(isa, run, native, native_budget, poll_continuation);
            run = result.run;
            if matches!(result.action, TurnAction::Dispatch) && Self::observes_cpu_accounting(isa, &run.machine) {
                Self::charge_elapsed(run.cpu_account.as_deref(), charged_at);
            }
            if matches!(result.action, TurnAction::Dispatch) && Self::configures_cpu_boundary(isa, &run.machine) {
                native.boundary_sensitive.insert(run.process);
            }
            let trace_entry = if matches!(result.action, TurnAction::Dispatch) {
                Self::trace_start(&run, false)
            } else {
                Ok(TraceBoundary::Dispatch)
            };
            let may_inline = matches!(result.action, TurnAction::Dispatch)
                && serviced < INLINE_SERVICE_LIMIT
                && matches!(trace_entry, Ok(TraceBoundary::Continue))
                && !native.boundary_sensitive.contains(&run.process)
                && continuation
                    .as_ref()
                    .is_some_and(threads::SchedulerContinuation::is_current)
                && !threads.has_parked()
                && !Self::defers_signal_delivery(isa, &run.machine)
                && matches!(Self::blocks(isa, &run.machine, &run.router, true), Ok(false));
            if may_inline {
                native.counters.services += 1;
                let memory = run.space.arena_memory();
                let (terminal, replaced) = match Self::dispatch(isa, &memory, &run) {
                    Ok(outcome) => outcome,
                    Err(error) => return Self::apply_error(threads, run, error),
                };
                if replaced {
                    return Ok(None);
                }
                if threads.is_parked(&run) {
                    return Ok(None);
                }
                if let Some(terminal) = terminal {
                    return Self::finish(plan, threads, run, terminal);
                }
                match Self::trace_start(&run, true) {
                    Ok(TraceBoundary::Park) => {
                        if let Err(error) = threads.park(run.thread) {
                            return Self::apply_error(threads, run, Self::thread_error(error));
                        }
                        return Ok(None);
                    }
                    Ok(TraceBoundary::Kill) => {
                        return Self::finish(plan, threads, run, ThreadTerminal::Thread(Self::signal(9)));
                    }
                    Ok(TraceBoundary::Continue | TraceBoundary::Dispatch | TraceBoundary::Signal(_)) => {}
                    Err(error) => return Self::apply_error(threads, run, error),
                }
                // A deliverable signal ends this activation so the next `advance`
                // reaches its delivery boundary; it does not make the process
                // permanently boundary-sensitive.
                if run.interrupt.is_set() {
                    threads.release(&run).map_err(Self::thread_error)?;
                    return Ok(None);
                }
                serviced += 1;
                continue;
            }
            return Self::apply_turn(
                isa,
                plan,
                threads,
                waiters,
                native,
                TurnResult {
                    run,
                    action: result.action,
                },
            );
        }
    }

    /// Executes one bounded guest slice without changing scheduler ownership.
    /// The exact generation-qualified `run` is returned on every path.
    pub(super) fn execute_turn(
        isa: GuestIsa,
        run: threads::ThreadRun,
        native: &mut NativePool,
        native_budget: u64,
        continuation: Option<&threads::SchedulerContinuation>,
    ) -> TurnResult {
        let action = match run.space.with_execution_memory(|memory| {
            let outcome = Self::native_slice(&run, memory, native, native_budget, continuation)
                .unwrap_or_else(|| run.machine.run_slice(1, SLICE_BUDGET, memory));
            Self::step(isa, memory, outcome)
        }) {
            Ok(action) => action,
            Err(error) => TurnAction::Error(error),
        };
        if let TurnAction::Signal {
            signal: 11,
            code,
            address,
            ..
        } = &action
        {
            // This fires for every guest SIGSEGV, whether or not native execution ran and whether
            // or not the guest handles it, so it must not name native execution or call it terminal.
            hl_log::hl_error!(
                hl_log::tag::EXEC,
                "guest took a fault signal=11 code={code} address={address:#x} process={:?}",
                run.process,
            );
            if let Some(boundaries) = &native.boundaries {
                boundaries.report(Some(run.process));
            }
        }
        TurnResult { run, action }
    }

    /// Whether an activation that has already been granted `admitted`
    /// instructions may be extended again rather than returning to the
    /// scheduler. `admitted` grows by one grant per extension, so this bounds
    /// the chain even when no instruction retires.
    const fn may_extend_activation(admitted: u64) -> bool {
        admitted < NATIVE_ACTIVATION_BUDGET
    }

    const fn native_budget(only_runnable: bool) -> u64 {
        if only_runnable {
            NATIVE_SOLO_BUDGET
        } else {
            SLICE_BUDGET
        }
    }

    /// Applies a completed guest turn on the coordinator. Workers never
    /// release, park, retire, or hand a run to the waiter pool directly.
    pub(super) fn apply_turn(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        waiters: &waiter::Pool,
        native: &mut NativePool,
        result: TurnResult,
    ) -> Result<Option<EngineExit>, EngineError> {
        let TurnResult { run, action } = result;
        match action {
            TurnAction::Continue => {}
            TurnAction::Error(error) => return Self::apply_error(threads, run, error),
            TurnAction::Replace(generation) => {
                if let Err(error) = Self::replace(&run.router, generation) {
                    return Self::apply_error(threads, run, error);
                }
                if native.reset_process(run.process).is_none() {
                    native.disable();
                }
                // Exec publishes a new generation in the ready state and retires
                // the running image. The old run no longer owns a scheduler slot.
                return Ok(None);
            }
            TurnAction::Signal {
                signal,
                code,
                address,
                fallback,
            } => {
                if run.router.queue_signal(signal, code, address).is_err() {
                    return Self::finish(plan, threads, run, ThreadTerminal::Thread(fallback));
                }
            }
            TurnAction::Dispatch => match Self::trace_start(&run, false) {
                Err(error) => return Self::apply_error(threads, run, error),
                Ok(boundary) => match boundary {
                    TraceBoundary::Park => {
                        if let Err(error) = threads.park(run.thread) {
                            return Self::apply_error(threads, run, Self::thread_error(error));
                        }
                        return Ok(None);
                    }
                    TraceBoundary::Kill => {
                        return Self::finish(plan, threads, run, ThreadTerminal::Thread(Self::signal(9)));
                    }
                    TraceBoundary::Continue | TraceBoundary::Dispatch => {
                        return Self::dispatch_ready(isa, plan, threads, waiters, run, native);
                    }
                    TraceBoundary::Signal(_) => return Self::apply_error(threads, run, EngineError::WaitFailed),
                },
            },
            TurnAction::Terminal(terminal) => {
                return Self::finish(plan, threads, run, terminal);
            }
        }
        threads.release(&run).map_err(Self::thread_error)?;
        Ok(None)
    }

    pub(super) fn apply_error(
        threads: &threads::ThreadSet,
        run: threads::ThreadRun,
        error: EngineError,
    ) -> Result<Option<EngineExit>, EngineError> {
        threads.release(&run).map_err(Self::thread_error)?;
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs::OpenOptions;
    use std::sync::Arc;

    use super::pool::SourceChange;
    use super::*;

    fn plan(options: crate::options::Options) -> RuntimeLaunchPlan {
        RuntimeLaunchPlan {
            rootfs: None,
            executable_host: None,
            arguments: Vec::new(),
            environment: Vec::new(),
            result_path: None,
            options,
        }
    }

    #[test]
    fn selection_scope() {
        let disabled = plan(crate::options::Options::default());
        let disabled_pool = NativePool::new(GuestIsa::Aarch64, &disabled, None);
        assert!(!disabled_pool.enabled);
        assert!(disabled_pool.boundaries.is_none());
        let mut options = crate::options::Options::default();
        options.set("HL_NATIVE_EXECUTION", "1", true).unwrap();
        let enabled = plan(options);
        assert_eq!(
            NativePool::new(GuestIsa::Aarch64, &enabled, None).enabled,
            cfg!(target_arch = "aarch64")
        );
        assert_eq!(
            NativePool::new(GuestIsa::X86_64, &enabled, None).enabled,
            cfg!(target_arch = "aarch64")
        );
        assert!(NativePool::new(GuestIsa::X86_64, &enabled, None).boundaries.is_none());

        let mut diagnostic_options = crate::options::Options::default();
        diagnostic_options.set("HL_NATIVE_DIAGNOSTICS", "1", true).unwrap();
        assert!(
            NativePool::new(GuestIsa::X86_64, &plan(diagnostic_options), None)
                .boundaries
                .is_some()
        );
    }

    #[test]
    fn the_admission_cache_serves_only_an_identical_site_length_and_epoch() {
        let mut options = crate::options::Options::default();
        options.set("HL_NATIVE_ADMISSION_CACHE", "1", true).unwrap();
        let mut pool = NativePool::new(GuestIsa::X86_64, &plan(options), None);
        let process = hl_task::ProcessId::from_wire(7, 1).unwrap();
        let site = (process, 3, 11, 0x1000);
        let code = [0xcc_u8; 8];
        pool.record_admission(site, code.len(), 5, &code);

        let mut served = [0_u8; 8];
        assert!(pool.admitted_bytes(site, 8, 5, &mut served), "a repeat must hit");
        assert_eq!(served, code);

        // Every invalidation the key must catch: a peer rewriting the code moves
        // `token.version`, a mapping change moves the incarnation, an instruction
        // epoch rotation moves the epoch, and a different entry moves the PC.
        let (_, incarnation, version, pc) = site;
        for (name, miss) in [
            ("self-modifying write", (process, incarnation, version + 1, pc)),
            ("mapping change", (process, incarnation + 1, version, pc)),
            (
                "other process",
                (hl_task::ProcessId::from_wire(8, 1).unwrap(), incarnation, version, pc),
            ),
            ("other entry", (process, incarnation, version, pc + 4)),
        ] {
            assert!(!pool.admitted_bytes(miss, 8, 5, &mut served), "{name} must miss");
        }
        assert!(
            !pool.admitted_bytes(site, 8, 6, &mut served),
            "epoch rotation must miss"
        );
        assert!(
            !pool.admitted_bytes(site, 4, 5, &mut served),
            "a shorter span must miss"
        );
        pool.disable();
        assert!(
            !pool.admitted_bytes(site, 8, 5, &mut served),
            "a disabled pool must miss"
        );
    }

    #[test]
    fn the_admission_cache_is_off_unless_its_option_is_set() {
        let mut pool = NativePool::new(GuestIsa::X86_64, &plan(crate::options::Options::default()), None);
        assert!(!pool.admission_cache);
        let site = (hl_task::ProcessId::from_wire(7, 1).unwrap(), 3, 11, 0x1000);
        pool.record_admission(site, 8, 5, &[0xcc_u8; 8]);
        assert!(pool.admitted.is_none());
        assert!(!pool.admitted_bytes(site, 8, 5, &mut [0_u8; 8]));
    }

    #[test]
    fn a_compute_bound_activation_stops_extending_and_returns_to_the_scheduler() {
        // The C run loop admits one solo grant per accepted extension, so the
        // chain a spinning guest thread can build must terminate.
        let mut admitted = NATIVE_SOLO_BUDGET;
        let mut extensions = 0u32;
        while GuestExecutor::may_extend_activation(admitted) {
            admitted += NATIVE_SOLO_BUDGET;
            extensions += 1;
            assert!(extensions < 1_000_000, "activation extended without bound");
        }
        assert!(extensions > 0, "a solo grant must still be extendable");
        assert!(admitted <= NATIVE_ACTIVATION_BUDGET + NATIVE_SOLO_BUDGET);
    }

    #[test]
    fn native_budget_preserves_shared_fairness() {
        assert_eq!(GuestExecutor::native_budget(false), SLICE_BUDGET);
        assert_eq!(GuestExecutor::native_budget(true), NATIVE_SOLO_BUDGET);
    }

    #[test]
    fn zero_progress_x86_yield_selects_interpreter() {
        assert!(GuestExecutor::x86_yield_needs_interpreter(
            crate::native::NativeExit::Yield,
            0,
        ));
        assert!(!GuestExecutor::x86_yield_needs_interpreter(
            crate::native::NativeExit::Yield,
            1,
        ));
        assert!(!GuestExecutor::x86_yield_needs_interpreter(
            crate::native::NativeExit::Interrupt,
            0,
        ));
    }

    #[test]
    fn cpu_timer_calls_preserve_outer_boundaries() {
        for number in [103, 107, 110] {
            assert!(GuestExecutor::cpu_boundary_call(GuestIsa::Aarch64, number));
        }
        for number in [38, 222, 223] {
            assert!(GuestExecutor::cpu_boundary_call(GuestIsa::X86_64, number));
        }
        assert!(!GuestExecutor::cpu_boundary_call(GuestIsa::Aarch64, 178));
        assert!(!GuestExecutor::cpu_boundary_call(GuestIsa::X86_64, 186));
        for number in [153, 165] {
            assert!(GuestExecutor::cpu_accounting_call(GuestIsa::Aarch64, number));
        }
        for number in [98, 100] {
            assert!(GuestExecutor::cpu_accounting_call(GuestIsa::X86_64, number));
        }
        assert!(!GuestExecutor::cpu_accounting_call(GuestIsa::Aarch64, 178));
        assert!(!GuestExecutor::cpu_accounting_call(GuestIsa::X86_64, 186));
    }

    #[test]
    fn native_interrupt_pc() {
        let original = hl_execution::Aarch64CpuState {
            pc: 0x4000,
            ..hl_execution::Aarch64CpuState::default()
        };
        let mut interrupted = original.clone();
        interrupted.registers[19] = 0xfeed_face;
        interrupted.pc = 0x4010;
        assert_eq!(
            GuestExecutor::native_boundary(
                &mut interrupted,
                original.clone(),
                crate::native::NativeExit::Interrupt,
                0x4010,
                0,
            ),
            Some(StepOutcome::Yield),
        );
        assert_eq!(interrupted.pc, 0x4010);
        assert_eq!(interrupted.registers[19], 0xfeed_face);

        let mut stale = interrupted;
        assert_eq!(
            GuestExecutor::native_boundary(
                &mut stale,
                original.clone(),
                crate::native::NativeExit::Epoch,
                0x4010,
                0,
            ),
            Some(StepOutcome::Yield),
        );
        assert_eq!(stale, original);

        let mut committed = original.clone();
        committed.registers[19] = 0xfeed_face;
        committed.pc = 0x4010;
        assert_eq!(
            GuestExecutor::native_boundary(&mut committed, original, crate::native::NativeExit::Epoch, 0x4010, 1,),
            Some(StepOutcome::Yield),
        );
        assert_eq!(committed.pc, 0x4010);
        assert_eq!(committed.registers[19], 0xfeed_face);
    }

    #[test]
    fn fallback_epoch() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let site = (process, 2, 4, 0x5000);
        let changed = (process, 2, 5, 0x5000);
        let mut sites = BTreeSet::new();
        sites.insert(site);
        assert!(sites.contains(&site));
        assert!(!sites.contains(&changed));
    }

    #[test]
    fn fallback_site_is_distinct_from_suppressed_entry() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let entry = (process, 2, 4, 0x400990);
        let instruction = (process, 2, 4, 0x40b0c0);
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        pool.record_fallback(entry, instruction, 0, SLICE_BUDGET, false);
        assert_eq!(
            pool.suppressed.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([entry])
        );
        assert_eq!(pool.fallbacks, BTreeSet::from([instruction]));
    }

    /// A run that retired at least half its budget is worth re-entering, because declining
    /// the entry would have bought only a `SLICE_BUDGET` interpreter slice instead.
    #[test]
    fn a_fallback_that_retired_most_of_its_budget_keeps_the_entry_but_records_the_site() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let entry = (process, 2, 4, 0x1055286);
        let instruction = (process, 2, 4, 0x40b0c0);
        let mut pool = NativePool::new(GuestIsa::X86_64, &plan(crate::options::Options::default()), None);
        pool.record_fallback(entry, instruction, NATIVE_SOLO_BUDGET / 2, NATIVE_SOLO_BUDGET, false);
        assert!(pool.suppressed.is_empty());
        assert_eq!(pool.fallbacks, BTreeSet::from([instruction]));
        // One instruction short of half a slice is not worth the entry, whatever the native budget.
        pool.record_fallback(entry, instruction, SLICE_BUDGET / 2 - 1, NATIVE_SOLO_BUDGET, false);
        assert_eq!(
            pool.suppressed.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([entry])
        );
    }

    /// Direct authority runs without the operand resolver, so a memory fallback under it is
    /// a limit of the run mode. The entry buys exactly one retry without it and then latches,
    /// bounding the extra interpreter slices at one per entry key for the life of the process.
    #[test]
    fn a_direct_authority_memory_fallback_buys_one_retry_before_it_suppresses() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let entry = (process, 2, 4, 0x1008a80);
        let instruction = (process, 2, 4, 0x1008abc);
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        pool.record_fallback(entry, instruction, 0, SLICE_BUDGET, true);
        assert!(pool.suppressed.is_empty());
        assert_eq!(pool.direct_declined, BTreeSet::from([entry]));
        pool.record_fallback(entry, instruction, 0, SLICE_BUDGET, true);
        assert_eq!(
            pool.suppressed.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([entry])
        );
        // A run that retired most of its budget still keeps its entry either way.
        let kept = (process, 2, 4, 0x1008b00);
        pool.record_fallback(kept, instruction, SLICE_BUDGET, SLICE_BUDGET, true);
        assert!(!pool.suppressed.contains_key(&kept));
        assert!(!pool.direct_declined.contains(&kept));
    }

    /// A latch serves a bounded span of refusals and then lets the entry back in. One
    /// short run must not condemn an entry for the life of the process: under a SIGURG
    /// storm a signal invalidates the operand projection and the next run guard-faults
    /// early, which is the storm's verdict on the entry, not the entry's own.
    #[test]
    fn a_latched_entry_is_retried_once_its_span_of_refusals_is_spent() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let entry = (process, 2, 4, 0x10182a8);
        let instruction = (process, 2, 4, 0x10090b8);
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        // The storm target is the guest's hottest loop: it retired full budgets before the
        // first signal landed, which is what earns it a span to recover in.
        pool.mark_productive(entry, SLICE_BUDGET, SLICE_BUDGET);
        pool.record_fallback(entry, instruction, 399, SLICE_BUDGET, false);
        assert_eq!(pool.counters.suppress_latches, 1);
        for spent in 1..=super::pool::SUPPRESSION_SPAN {
            assert!(pool.refuses(entry), "refusal {spent} of the span must still decline");
        }
        assert_eq!(pool.counters.suppress_clears, 1);
        assert!(!pool.refuses(entry), "the span is spent, so the entry is retried");
        // A retry that falls short again re-arms to another span rather than to a
        // permanent latch, so a productive entry can always recover.
        pool.record_fallback(entry, instruction, 399, SLICE_BUDGET, false);
        assert_eq!(pool.counters.suppress_rearms, 1);
        assert_eq!(pool.suppressed[&entry].remaining, super::pool::SUPPRESSION_SPAN);
        assert!(!pool.suppressed[&entry].permanent);
    }

    /// The two populations a refusal count cannot separate. An entry that has retired real
    /// native work before earns another span, because a short run is an anomaly. An entry
    /// that has fallen short on every run it has ever had latches permanently after its one
    /// probationary span, so the exit-heavy phases stop paying a re-entry and a rebuild to
    /// rediscover that there is nothing to recover.
    #[test]
    fn only_an_entry_that_has_been_productive_earns_a_second_span() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let instruction = (process, 2, 4, 0x10090b8);
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);

        // Never productive: one span, then a permanent latch no refusal expires.
        let barren = (process, 2, 4, 0x1009000);
        pool.mark_productive(barren, 20, SLICE_BUDGET);
        assert!(
            !pool.productive.contains(&barren),
            "a 20-instruction run is not productive"
        );
        pool.record_fallback(barren, instruction, 20, SLICE_BUDGET, false);
        pool.record_fallback(barren, instruction, 20, SLICE_BUDGET, false);
        assert_eq!(pool.counters.suppress_permanent, 1);
        assert_eq!(pool.counters.suppress_rearms, 0);
        for _ in 0..super::pool::SUPPRESSION_SPAN * 4 {
            assert!(pool.refuses(barren), "a permanent latch never expires");
        }

        // Productive once: the latch keeps expiring, so the entry can recover.
        let hot = (process, 2, 4, 0x10182a8);
        pool.mark_productive(hot, SLICE_BUDGET, SLICE_BUDGET);
        assert!(pool.productive.contains(&hot));
        pool.record_fallback(hot, instruction, 399, SLICE_BUDGET, false);
        pool.record_fallback(hot, instruction, 399, SLICE_BUDGET, false);
        assert_eq!(pool.counters.suppress_rearms, 1);
        assert_eq!(pool.suppressed[&hot].remaining, super::pool::SUPPRESSION_SPAN);
        assert!(
            !pool.suppressed[&hot].permanent,
            "a productive entry never latches permanently"
        );
    }

    /// Absence from the productive table is only evidence while the table can still record.
    /// Every other capped set here fails towards not suppressing; this one would fail
    /// towards a permanent latch, which is why saturation has to disarm permanence.
    #[test]
    fn a_saturated_productive_table_stops_making_latches_permanent() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let instruction = (process, 2, 4, 0x10090b8);
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        pool.productive_saturated = true;
        let entry = (process, 2, 4, 0x1009000);
        pool.record_fallback(entry, instruction, 20, SLICE_BUDGET, false);
        pool.record_fallback(entry, instruction, 20, SLICE_BUDGET, false);
        assert_eq!(pool.counters.suppress_permanent, 0, "saturation disarms permanence");
        assert_eq!(pool.counters.suppress_rearms, 1);
        assert!(!pool.suppressed[&entry].permanent);
    }

    /// The threshold is a share of the budget, so a short-budget run cannot dodge
    /// suppression by retiring a handful of instructions.
    #[test]
    fn a_fallback_that_retired_a_handful_of_instructions_still_suppresses_the_entry() {
        assert!(NativePool::fallback_suppresses(0, SLICE_BUDGET));
        assert!(NativePool::fallback_suppresses(37, SLICE_BUDGET));
        assert!(NativePool::fallback_suppresses(37, NATIVE_SOLO_BUDGET));
        assert!(!NativePool::fallback_suppresses(SLICE_BUDGET, SLICE_BUDGET));
        // A solo run is measured against the slice a decline buys, not against its own budget.
        assert!(!NativePool::fallback_suppresses(SLICE_BUDGET / 2, NATIVE_SOLO_BUDGET));
    }

    /// Warm-up survives mapping churn elsewhere in the space but not a change to the
    /// entry's own bytes: the site carries the range version, never the ledger generation.
    #[test]
    fn warm_up_survives_unrelated_mapping_churn_and_resets_on_a_range_change() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        let entry = (process, 1, 7, 0x400990);
        assert_eq!(pool.observe(entry), 1);
        assert_eq!(pool.observe(entry), 2);
        assert_eq!(pool.observe((process, 1, 8, 0x400990)), 1);
    }

    #[test]
    fn a_full_observation_table_reclaims_stale_generations_instead_of_disabling() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let mut pool = NativePool::new(GuestIsa::X86_64, &plan(crate::options::Options::default()), None);
        pool.enabled = true;
        for index in 0..NATIVE_SITE_LIMIT {
            pool.observations.insert((process, index as u64, 4, 0x400000), 2);
        }
        assert_eq!(pool.observations.len(), NATIVE_SITE_LIMIT);
        let fresh = (process, u64::MAX, 4, 0x403340);
        assert_eq!(pool.observe(fresh), 1);
        assert_eq!(pool.observe(fresh), 2);
        assert!(pool.enabled);
        assert!(pool.observations.len() < NATIVE_SITE_LIMIT);
    }

    #[test]
    fn process_metadata_reset_and_retirement_are_exact() {
        let retired = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let retained = hl_task::ProcessId::from_wire(2, 1).unwrap();
        let token = hl_memory::ExecutableToken {
            incarnation: 1,
            version: 1,
        };
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        for process in [retired, retained] {
            pool.sources.insert((process, 1, 0x1000, 0x1100), token);
            pool.observations.insert((process, 1, 1, 0x1000), 1);
            pool.suppressed.insert(
                (process, 1, 1, 0x1000),
                super::pool::Probation {
                    remaining: 1,
                    permanent: false,
                },
            );
            pool.direct_declined.insert((process, 1, 1, 0x1000));
            pool.fallbacks.insert((process, 1, 1, 0x1004));
            pool.source_incarnations.insert(process, 1);
            pool.instruction_epochs.insert(process, 1);
            pool.direct_modes.insert(process, (true, 0));
            pool.direct_holds.insert(process, 1);
            pool.boundary_sensitive.insert(process);
        }

        pool.purge_process_metadata(retired);
        assert!(pool.sources.keys().all(|(process, _, _, _)| *process == retained));
        assert!(pool.observations.keys().all(|(process, _, _, _)| *process == retained));
        assert!(!pool.source_incarnations.contains_key(&retired));
        assert!(pool.source_incarnations.contains_key(&retained));

        pool.retain_processes(&BTreeSet::new());
        assert!(pool.sources.is_empty());
        assert!(pool.observations.is_empty());
        assert!(pool.suppressed.is_empty());
        assert!(pool.direct_declined.is_empty());
        assert!(pool.fallbacks.is_empty());
        assert!(pool.source_incarnations.is_empty());
        assert!(pool.instruction_epochs.is_empty());
        assert!(pool.direct_modes.is_empty());
        assert!(pool.direct_holds.is_empty());
        assert!(pool.boundary_sensitive.is_empty());
    }

    /// The unchanged-live-set memo must not suppress the sweep once the set really changes.
    #[test]
    fn retain_processes_memo_still_drops_a_newly_dead_process() {
        let dying = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let live = hl_task::ProcessId::from_wire(2, 1).unwrap();
        let token = hl_memory::ExecutableToken {
            incarnation: 1,
            version: 1,
        };
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        for process in [dying, live] {
            pool.sources.insert((process, 1, 0x1000, 0x1100), token);
            pool.observations.insert((process, 1, 1, 0x1000), 1);
            pool.boundary_sensitive.insert(process);
        }

        let both = BTreeSet::from([dying, live]);
        pool.retain_processes(&both);
        pool.retain_processes(&both);
        assert_eq!(pool.sources.len(), 2);
        assert_eq!(pool.observations.len(), 2);

        pool.retain_processes(&BTreeSet::from([live]));
        assert!(pool.sources.keys().all(|(process, _, _, _)| *process == live));
        assert!(pool.observations.keys().all(|(process, _, _, _)| *process == live));
        assert_eq!(pool.boundary_sensitive, BTreeSet::from([live]));
    }

    /// Sustained run-mode alternation must trip a hold, while a steady mode never does.
    #[test]
    fn direct_run_mode_holds_only_on_sustained_alternation() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        for index in 0..1024 {
            pool.observe_direct_mode(process, index % 3 == 0);
        }
        assert!(pool.direct_holds.contains_key(&process));
        assert!(!pool.direct_admitted(process));

        let steady = hl_task::ProcessId::from_wire(2, 1).unwrap();
        for index in 0..1024 {
            pool.observe_direct_mode(steady, index != 512);
        }
        assert!(!pool.direct_holds.contains_key(&steady));
        assert!(pool.direct_admitted(steady));
    }

    /// Direct authority is earned by a run, never taken on the first entry, and a held
    /// process still serves its hold out rather than waiting on a warm-up it cannot reach.
    #[test]
    fn direct_authority_is_earned_by_a_completed_run() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        assert!(
            !pool.direct_earned(process),
            "first entry must run the operand resolver"
        );
        pool.observe_direct_mode(process, false);
        assert!(pool.direct_earned(process), "a completed run earns direct authority");

        // A hold entered while warm is spent by the same call, so it expires on schedule.
        pool.direct_holds.insert(process, 2);
        pool.direct_modes.remove(&process);
        assert!(!pool.direct_earned(process));
        assert!(!pool.direct_earned(process));
        assert!(!pool.direct_holds.contains_key(&process), "the hold must retire");
        pool.observe_direct_mode(process, false);
        assert!(pool.direct_earned(process));
    }

    #[test]
    fn source_tracking_prunes_incarnations_and_disables_at_live_limit() {
        let first = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let second = hl_task::ProcessId::from_wire(2, 1).unwrap();
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        pool.enabled = true;
        let token = |incarnation, version| hl_memory::ExecutableToken { incarnation, version };

        assert_eq!(
            pool.track_source(first, 0x1000, 0x1100, token(7, 1), 3),
            SourceChange::Stable
        );
        assert_eq!(
            pool.track_source(first, 0x1000, 0x1100, token(7, 2), 3),
            SourceChange::Changed
        );
        assert_eq!(pool.sources[&(first, 7, 0x1000, 0x1100)], token(7, 2));
        assert_eq!(
            pool.track_source(first, 0x1000, 0x1100, token(7, 2), 3),
            SourceChange::Stable
        );
        assert_eq!(
            pool.track_source(first, 0x2000, 0x2100, token(7, 1), 3),
            SourceChange::Stable
        );
        assert_eq!(
            pool.track_source(second, 0x3000, 0x3100, token(4, 1), 3),
            SourceChange::Stable
        );
        assert_eq!(pool.sources.len(), 3);

        assert_eq!(
            pool.track_source(first, 0x4000, 0x4100, token(8, 1), 3),
            SourceChange::Stable
        );
        assert_eq!(pool.sources.len(), 2);
        assert!(
            pool.sources
                .keys()
                .all(|(owner, incarnation, _, _)| { *owner != first || *incarnation == 8 })
        );

        assert_eq!(
            pool.track_source(first, 0x5000, 0x5100, token(8, 1), 3),
            SourceChange::Stable
        );
        assert_eq!(pool.sources.len(), 3);
        assert_eq!(
            pool.track_source(first, 0x6000, 0x6100, token(8, 1), 3),
            SourceChange::Disabled
        );
        assert!(!pool.enabled && pool.sources.is_empty());
    }

    #[test]
    fn source_tracking_churn_is_bounded_at_production_limit() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        pool.enabled = true;
        let token = hl_memory::ExecutableToken {
            incarnation: 1,
            version: 1,
        };
        for index in 0..NATIVE_SOURCE_LIMIT {
            let first = 0x1000 + index as u64 * 16;
            assert_eq!(
                pool.track_source(process, first, first + 16, token, NATIVE_SOURCE_LIMIT),
                SourceChange::Stable
            );
        }
        assert_eq!(pool.sources.len(), NATIVE_SOURCE_LIMIT);
        assert_eq!(
            pool.track_source(process, u64::MAX - 15, u64::MAX, token, NATIVE_SOURCE_LIMIT),
            SourceChange::Disabled
        );
        assert!(!pool.enabled && pool.sources.is_empty() && pool.source_incarnations.is_empty());
    }

    #[test]
    fn resolved_source_participates_in_epoch_refresh() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let mut pool = NativePool::new(GuestIsa::X86_64, &plan(crate::options::Options::default()), None);
        pool.enabled = true;
        let token = |version| hl_memory::ExecutableToken {
            incarnation: 7,
            version,
        };
        pool.executor(process).unwrap().reset(7).unwrap();
        pool.instruction_epochs.insert(process, 1);
        pool.merge_observed_sources(process, 7, vec![(0x8000, 0x8100, token(1))], true)
            .unwrap();

        assert!(
            pool.prepare_source(process, 0x4000, 0x4100, token(0), 2, |first, last| {
                assert_eq!((first, last), (0x8000, 0x8100));
                Some(token(2))
            })
            .is_some()
        );

        assert_eq!(pool.sources[&(process, 7, 0x8000, 0x8100)], token(2));
        assert_eq!(pool.sources[&(process, 7, 0x4000, 0x4100)], token(0));
    }

    #[test]
    fn bus_classification() {
        let arena = Arc::new(super::super::VirtualMemory::reserve(8192).unwrap());
        let mappings = Arc::new(hl_memory::MappingCoordinator::new(
            super::super::MappingHostAdapter::new(Arc::clone(&arena)),
        ));
        mappings
            .map(hl_memory::MapRequest {
                placement: hl_memory::Placement::Fixed(hl_isa::GuestAddress::new(0)),
                length: 4096,
                alignment: 4096,
                protection: hl_memory::Protection::READ,
                backing: hl_memory::Backing::Anonymous {
                    identity: 71,
                    shared: false,
                },
                backing_offset: 0,
            })
            .unwrap();
        let space = super::super::space::AddressSpace::new(Arc::clone(&arena), mappings);
        let memory = space.arena_memory();
        let path = std::env::temp_dir().join(format!("hl-scheduler-bus-{}", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_len(1).unwrap();
        let identity = hl_memory::FileIdentity { device: 51, object: 52 };
        arena.register_file(identity, &file).unwrap();
        assert!(arena.validate_file(identity, 4096, 8, 4096).is_err());

        let lease = space.lease();
        assert_eq!(GuestExecutor::classify(&memory, &lease, 4096, Some(8)), (7, 2, 4096));
        assert_eq!(GuestExecutor::classify(&memory, &lease, 4096, Some(8)), (11, 1, 4096));
        assert_eq!(GuestExecutor::classify(&memory, &lease, 4096, None), (11, 1, 4096));
        assert_eq!(GuestExecutor::classify(&memory, &lease, 0, None), (11, 2, 0));

        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn descriptor_parking() {
        for number in [32, 63, 64, 65, 66, 182, 183, 202, 203, 206, 207, 211, 212, 242] {
            assert!(GuestExecutor::descriptor_blocks(GuestIsa::Aarch64, number));
        }
        for number in [0, 1, 19, 20, 42, 43, 44, 45, 46, 47, 73, 242, 243, 288] {
            assert!(GuestExecutor::descriptor_blocks(GuestIsa::X86_64, number));
        }
        assert!(!GuestExecutor::descriptor_blocks(GuestIsa::Aarch64, 62));
        assert!(!GuestExecutor::descriptor_blocks(GuestIsa::X86_64, 2));
    }

    #[test]
    fn record_lock_parking() {
        assert!(GuestExecutor::record_lock_blocks(GuestIsa::Aarch64, 25, 7));
        assert!(GuestExecutor::record_lock_blocks(GuestIsa::X86_64, 72, 7));
        assert!(!GuestExecutor::record_lock_blocks(GuestIsa::Aarch64, 25, 6));
        assert!(!GuestExecutor::record_lock_blocks(GuestIsa::X86_64, 72, 5));
        assert!(!GuestExecutor::record_lock_blocks(GuestIsa::Aarch64, 72, 7));
        assert!(!GuestExecutor::record_lock_blocks(GuestIsa::X86_64, 25, 7));
    }

    #[test]
    fn ipc_parking() {
        for number in [188, 189, 192, 193] {
            assert!(GuestExecutor::ipc_blocks(GuestIsa::Aarch64, number));
        }
        for number in [65, 69, 70, 220] {
            assert!(GuestExecutor::ipc_blocks(GuestIsa::X86_64, number));
        }
        assert!(!GuestExecutor::ipc_blocks(GuestIsa::Aarch64, 194));
        assert!(!GuestExecutor::ipc_blocks(GuestIsa::X86_64, 64));
    }

    #[test]
    fn signal_wait_parking() {
        for number in [133, 137] {
            assert!(GuestExecutor::signal_blocks(GuestIsa::Aarch64, number));
        }
        for number in [34, 128, 130] {
            assert!(GuestExecutor::signal_blocks(GuestIsa::X86_64, number));
        }
        assert!(!GuestExecutor::signal_blocks(GuestIsa::Aarch64, 128));
        assert!(!GuestExecutor::signal_blocks(GuestIsa::X86_64, 137));
    }

    #[test]
    fn readiness_wait_parking() {
        for number in [22, 72, 73, 441] {
            assert!(GuestExecutor::readiness_blocks(GuestIsa::Aarch64, number));
        }
        for number in [7, 23, 232, 270, 271, 281, 441] {
            assert!(GuestExecutor::readiness_blocks(GuestIsa::X86_64, number));
        }
        assert!(!GuestExecutor::readiness_blocks(GuestIsa::Aarch64, 232));
        assert!(!GuestExecutor::readiness_blocks(GuestIsa::X86_64, 22));
    }
}
