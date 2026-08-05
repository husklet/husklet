use hl_execution::{ExecutionCpuSnapshot, ExecutionMachine, StepOutcome};
use hl_isa::GuestAddress;
use hl_memory::Protection;
use hl_runtime::{RuntimeSyscallRouter, TraceBoundary};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::{ArenaMemory, GuestExecutor, threads, waiter};
const SLICE_BUDGET: u64 = 4096;
const NATIVE_SOLO_BUDGET: u64 = 65_536;
const INLINE_SERVICE_LIMIT: u64 = 64;
const QUANTUM_TURNS: u64 = 4096;
const NATIVE_SITE_LIMIT: usize = 65_536;
const NATIVE_SOURCE_LIMIT: usize = 65_536;
const NATIVE_BOUNDARY_CAPACITY: usize = 16;

#[derive(Clone, Copy)]
struct NativeBoundary {
    process: hl_task::ProcessId,
    start: u64,
    exit: crate::native::NativeExit,
    instruction: u64,
    next: u64,
    rip: u64,
    stack: u64,
    registers: [u64; 8],
    vectors: [u128; 2],
    flags: u16,
    mxcsr: u32,
    fault_address: u64,
    provenance: u64,
}

struct NativeBoundaries {
    records: [Option<NativeBoundary>; NATIVE_BOUNDARY_CAPACITY],
    next: usize,
}

impl NativeBoundaries {
    fn new() -> Self {
        Self {
            records: [None; NATIVE_BOUNDARY_CAPACITY],
            next: 0,
        }
    }

    fn push(&mut self, record: NativeBoundary) {
        self.records[self.next] = Some(record);
        self.next = (self.next + 1) % NATIVE_BOUNDARY_CAPACITY;
    }

    fn report(&self, process: hl_task::ProcessId) {
        for offset in 0..NATIVE_BOUNDARY_CAPACITY {
            let index = (self.next + offset) % NATIVE_BOUNDARY_CAPACITY;
            let Some(record) = self.records[index].filter(|record| record.process == process) else {
                continue;
            };
            eprintln!(
                "hl-native-boundary: start={:#x} exit={:?} instruction={:#x} next={:#x} rip={:#x} rsp={:#x} rax={:#x} rcx={:#x} rdx={:#x} rbx={:#x} rbp={:#x} rsi={:#x} rdi={:#x} xmm0={:#034x} xmm1={:#034x} rflags={:#x} mxcsr={:#x} fault={:#x} provenance={:#x}",
                record.start,
                record.exit,
                record.instruction,
                record.next,
                record.rip,
                record.stack,
                record.registers[0],
                record.registers[1],
                record.registers[2],
                record.registers[3],
                record.registers[5],
                record.registers[6],
                record.registers[7],
                record.vectors[0],
                record.vectors[1],
                record.flags,
                record.mxcsr,
                record.fault_address,
                record.provenance,
            );
        }
    }
}

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

enum CompletionTurn {
    Idle,
    Continue,
    Exit(EngineExit),
}

enum SignalTurn {
    Continue,
    Park,
    GatePark,
    Terminal(ThreadTerminal),
}

#[derive(Default)]
struct NativeCounters {
    builds: u64,
    hits: u64,
    fallbacks: u64,
    runs: u64,
    services: u64,
}

type NativeSite = (hl_task::ProcessId, u64, u64, u64, u64);
type NativeSource = (hl_task::ProcessId, u64, u64, u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceChange {
    Stable,
    Changed,
    Disabled,
}

pub(super) struct NativePool {
    enabled: bool,
    diagnostics: bool,
    executors: BTreeMap<hl_task::ProcessId, crate::native::NativeExecutor>,
    suppressed: BTreeSet<NativeSite>,
    fallbacks: BTreeSet<NativeSite>,
    observations: BTreeMap<NativeSite, u8>,
    sources: BTreeMap<NativeSource, hl_memory::ExecutableToken>,
    source_incarnations: BTreeMap<hl_task::ProcessId, u64>,
    instruction_epochs: BTreeMap<hl_task::ProcessId, u64>,
    boundary_sensitive: BTreeSet<hl_task::ProcessId>,
    boundaries: Option<NativeBoundaries>,
    counters: NativeCounters,
    host_faults: Option<Arc<dyn crate::native::HostFaultOwner>>,
}

impl NativePool {
    fn selected(isa: GuestIsa, plan: &RuntimeLaunchPlan) -> bool {
        cfg!(target_arch = "aarch64")
            && matches!(isa, GuestIsa::Aarch64 | GuestIsa::X86_64)
            && plan.options.get("HL_NATIVE_EXECUTION") == Some("1")
    }

    pub(super) fn new(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        host_faults: Option<Arc<dyn crate::native::HostFaultOwner>>,
    ) -> Self {
        Self {
            enabled: Self::selected(isa, plan),
            diagnostics: plan.options.get("HL_NATIVE_DIAGNOSTICS") == Some("1"),
            executors: BTreeMap::new(),
            suppressed: BTreeSet::new(),
            fallbacks: BTreeSet::new(),
            observations: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_incarnations: BTreeMap::new(),
            instruction_epochs: BTreeMap::new(),
            boundary_sensitive: BTreeSet::new(),
            boundaries: (plan.options.get("HL_NATIVE_DIAGNOSTICS") == Some("1")).then(NativeBoundaries::new),
            counters: NativeCounters::default(),
            host_faults,
        }
    }

    fn production_faults(
        selected: Option<Arc<dyn crate::native::HostFaultOwner>>,
    ) -> Option<Arc<dyn crate::native::HostFaultOwner>> {
        if selected.is_some() {
            return selected;
        }
        #[cfg(target_os = "linux")]
        return crate::native::NativeFaultOwner::create().ok();
        #[cfg(not(target_os = "linux"))]
        None
    }

    fn executor(&mut self, process: hl_task::ProcessId) -> Option<&crate::native::NativeExecutor> {
        if !self.enabled {
            return None;
        }
        if !self.executors.contains_key(&process) {
            self.executors.insert(
                process,
                crate::native::NativeExecutor::create_with_fault_owner(self.diagnostics, self.host_faults.clone())
                    .ok()?,
            );
        }
        self.executors.get(&process)
    }

    fn record_fallback(&mut self, entry: NativeSite, instruction: NativeSite) {
        if self.suppressed.len() < NATIVE_SITE_LIMIT {
            self.suppressed.insert(entry);
        }
        if self.fallbacks.len() < NATIVE_SITE_LIMIT {
            self.fallbacks.insert(instruction);
        }
    }

    fn disable(&mut self) {
        self.enabled = false;
        self.executors.clear();
        self.sources.clear();
        self.source_incarnations.clear();
        self.instruction_epochs.clear();
        self.observations.clear();
        self.suppressed.clear();
        self.fallbacks.clear();
    }

    fn reset_observed_sources(&mut self, process: hl_task::ProcessId, incarnation: u64) -> Option<()> {
        self.executor(process)?.reset(incarnation).ok()?;
        self.sources.retain(|(owner, _, _, _), _| *owner != process);
        self.observations.retain(|(owner, _, _, _, _), _| *owner != process);
        self.suppressed.retain(|(owner, _, _, _, _)| *owner != process);
        self.fallbacks.retain(|(owner, _, _, _, _)| *owner != process);
        Some(())
    }

    fn merge_observed_sources(
        &mut self,
        process: hl_task::ProcessId,
        incarnation: u64,
        observed: Vec<(u64, u64, hl_memory::ExecutableToken)>,
        complete: bool,
    ) -> Option<()> {
        let conflict = observed.iter().any(|(first, last, token)| {
            token.incarnation != incarnation
                || last <= first
                || self
                    .sources
                    .get(&(process, incarnation, *first, *last))
                    .is_some_and(|existing| existing != token)
        });
        let additions = observed
            .iter()
            .filter(|(first, last, _)| !self.sources.contains_key(&(process, incarnation, *first, *last)))
            .count();
        if !complete || conflict || self.sources.len().saturating_add(additions) > NATIVE_SOURCE_LIMIT {
            self.reset_observed_sources(process, incarnation)?;
        }
        for (first, last, token) in observed {
            if token.incarnation != incarnation || last <= first {
                continue;
            }
            self.sources.insert((process, incarnation, first, last), token);
        }
        Some(())
    }

    fn track_source(
        &mut self,
        process: hl_task::ProcessId,
        first: u64,
        last: u64,
        token: hl_memory::ExecutableToken,
        limit: usize,
    ) -> SourceChange {
        if self
            .source_incarnations
            .insert(process, token.incarnation)
            .is_some_and(|previous| previous != token.incarnation)
        {
            self.sources.retain(|(owner, _, _, _), _| *owner != process);
        }
        let key = (process, token.incarnation, first, last);
        if let Some(previous) = self.sources.get_mut(&key) {
            let changed = *previous != token;
            *previous = token;
            return if changed {
                SourceChange::Changed
            } else {
                SourceChange::Stable
            };
        }
        if self.sources.len() >= limit {
            self.disable();
            return SourceChange::Disabled;
        }
        self.sources.insert(key, token);
        SourceChange::Stable
    }

    fn prepare_source<F>(
        &mut self,
        process: hl_task::ProcessId,
        first: u64,
        last: u64,
        token: hl_memory::ExecutableToken,
        instruction_epoch: u64,
        mut current_token: F,
    ) -> Option<()>
    where
        F: FnMut(u64, u64) -> Option<hl_memory::ExecutableToken>,
    {
        let published = self.instruction_epochs.get(&process).copied();
        if published.is_some_and(|previous| previous != instruction_epoch) {
            let observed: Vec<_> = self
                .sources
                .iter()
                .filter(|((owner, incarnation, _, _), _)| *owner == process && *incarnation == token.incarnation)
                .map(|(key @ (_, _, first, last), previous)| (*key, *previous, *first, *last))
                .collect();
            if observed.len() > 1024 {
                self.reset_observed_sources(process, token.incarnation)?;
            } else {
                let mut changed = Vec::new();
                let mut refreshed = Vec::new();
                let mut gap = false;
                for (key, previous, source_first, source_last) in observed {
                    let Some(current) = current_token(source_first, source_last) else {
                        gap = true;
                        break;
                    };
                    if current.incarnation != token.incarnation {
                        gap = true;
                        break;
                    }
                    if current != previous {
                        changed.push((source_first, source_last));
                    }
                    refreshed.push((key, current));
                }
                if gap {
                    self.reset_observed_sources(process, token.incarnation)?;
                } else {
                    if !changed.is_empty()
                        && self
                            .executor(process)?
                            .invalidate_ranges(&changed, token.incarnation)
                            .is_err()
                    {
                        self.reset_observed_sources(process, token.incarnation)?;
                        refreshed.clear();
                    }
                    for (key, current) in refreshed {
                        self.sources.insert(key, current);
                    }
                }
            }
        }
        self.instruction_epochs.insert(process, instruction_epoch);
        let change = self.track_source(process, first, last, token, NATIVE_SOURCE_LIMIT);
        if change == SourceChange::Disabled {
            return None;
        }
        let executor = self.executor(process)?;
        if change == SourceChange::Changed {
            executor.invalidate(first, last, token.incarnation).ok()?;
        }
        Some(())
    }
}

impl Drop for NativePool {
    fn drop(&mut self) {
        if self.enabled && self.diagnostics {
            eprintln!(
                "hl-native: runs={} builds={} hits={} fallbacks={} sites={} services={}",
                self.counters.runs,
                self.counters.builds,
                self.counters.hits,
                self.counters.fallbacks,
                self.fallbacks.len(),
                self.counters.services,
            );
        }
    }
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

    fn cpu_boundary_call(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 103 | 107 | 110),
            GuestIsa::X86_64 => matches!(number, 38 | 222 | 223),
        }
    }

    fn configures_cpu_boundary(isa: GuestIsa, machine: &ExecutionMachine) -> bool {
        let mut configures = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            let number = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8],
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
            };
            configures = Self::cpu_boundary_call(isa, number);
            StepOutcome::Continue
        });
        outcome == StepOutcome::Continue && configures
    }

    fn cpu_accounting_call(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 153 | 165),
            GuestIsa::X86_64 => matches!(number, 98 | 100),
        }
    }

    fn observes_cpu_accounting(isa: GuestIsa, machine: &ExecutionMachine) -> bool {
        let mut observes = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            let number = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8],
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
            };
            observes = Self::cpu_accounting_call(isa, number);
            StepOutcome::Continue
        });
        outcome == StepOutcome::Continue && observes
    }

    fn descriptor_blocks(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(
                number,
                32 | 63..=66 | 182 | 183 | 202 | 203 | 206 | 207 | 211 | 212 | 242
            ),
            GuestIsa::X86_64 => matches!(number, 0 | 1 | 19 | 20 | 42..=47 | 73 | 242 | 243 | 288),
        }
    }

    fn record_lock_blocks(isa: GuestIsa, number: u64, command: u64) -> bool {
        command == 7
            && match isa {
                GuestIsa::Aarch64 => number == 25,
                GuestIsa::X86_64 => number == 72,
            }
    }

    fn ipc_blocks(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 188 | 189 | 192 | 193),
            GuestIsa::X86_64 => matches!(number, 65 | 69 | 70 | 220),
        }
    }

    fn signal_blocks(isa: GuestIsa, number: u64) -> bool {
        match isa {
            GuestIsa::Aarch64 => matches!(number, 133 | 137),
            GuestIsa::X86_64 => matches!(number, 34 | 128 | 130),
        }
    }

    fn readiness_blocks(isa: GuestIsa, number: u64) -> bool {
        // The scheduler classifies before it can safely inspect guest timeout
        // pointers. Route the whole wait family through the waiter pool: zero
        // timeouts complete there immediately, while finite and infinite waits
        // cannot pin the sole guest scheduling thread.
        match isa {
            GuestIsa::Aarch64 => matches!(number, 22 | 72 | 73 | 441),
            GuestIsa::X86_64 => matches!(number, 7 | 23 | 232 | 270 | 271 | 281 | 441),
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

        let mut turns = 0_u64;
        let host_faults = NativePool::selected(isa, plan)
            .then(|| NativePool::production_faults(host_faults))
            .flatten();
        let attachment = host_faults
            .as_ref()
            .and_then(|owner| owner.attach().ok().map(|()| FaultAttachment(Arc::clone(owner))));
        let active_faults = attachment.as_ref().and(host_faults);
        let mut native = NativePool::new(isa, plan, active_faults);
        loop {
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
            let process = run.process;
            let mut charged_at = Self::thread_cpu();
            let advanced = Self::advance(isa, plan, threads, waiters, run, &mut native, &mut charged_at);
            Self::charge_elapsed(threads, process, &mut charged_at);
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

    fn charge_elapsed(threads: &threads::ThreadSet, process: hl_task::ProcessId, charged_at: &mut Option<u64>) {
        let Some(finished) = Self::thread_cpu() else { return };
        if let Some(started) = charged_at.replace(finished) {
            threads.charge_cpu(process, finished.saturating_sub(started));
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
                return Self::dispatch_ready(isa, plan, threads, waiters, run);
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
        let interrupted_boundary = run.interrupt.is_set();
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
        if interrupted_boundary {
            native.boundary_sensitive.insert(run.process);
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
            let native_budget = Self::native_budget(threads.is_only_runnable(run.thread));
            let result = Self::execute_turn(isa, run, native, native_budget);
            run = result.run;
            if matches!(result.action, TurnAction::Dispatch) && Self::observes_cpu_accounting(isa, &run.machine) {
                Self::charge_elapsed(threads, run.process, charged_at);
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
                && !run.interrupt.is_set()
                && threads.is_only_runnable(run.thread)
                && !threads.has_parked()
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
                    Ok(_) => {}
                    Err(error) => return Self::apply_error(threads, run, error),
                }
                if run.interrupt.is_set() {
                    native.boundary_sensitive.insert(run.process);
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
    ) -> TurnResult {
        let action = match run.space.with_execution_memory(|memory| {
            let outcome = Self::native_slice(&run, memory, native, native_budget)
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
            if let Some(boundaries) = &native.boundaries {
                eprintln!("hl-native-terminal-sigsegv: code={code} address={address:#x}");
                boundaries.report(run.process);
            }
        }
        TurnResult { run, action }
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
                        return Self::dispatch_ready(isa, plan, threads, waiters, run);
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

    fn dispatch_ready(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        waiters: &waiter::Pool,
        run: threads::ThreadRun,
    ) -> Result<Option<EngineExit>, EngineError> {
        let solo = threads.is_only_runnable(run.thread) && !threads.has_parked();
        let blocks = match Self::blocks(isa, &run.machine, &run.router, solo) {
            Ok(blocks) => blocks,
            Err(error) => return Self::apply_error(threads, run, error),
        };
        if blocks {
            if let Err(error) = threads.park_syscall(&run) {
                return Self::apply_error(threads, run, Self::thread_error(error));
            }
            if let Err(run) = waiters.dispatch(run) {
                if let Err(error) = threads.abort_waiter(&run) {
                    return Err(Self::thread_error(error));
                }
                return Self::apply_error(threads, run, EngineError::Busy);
            }
            return Ok(None);
        }
        let memory = run.space.arena_memory();
        let (terminal, replaced) = match Self::dispatch(isa, &memory, &run) {
            Ok(result) => result,
            Err(error) => return Self::apply_error(threads, run, error),
        };
        if replaced {
            return Ok(None);
        }
        // Vfork dispatch transfers the parent from Running to Ready+parked.
        // Child exec or exit owns the matching resume; the syscall-exit path
        // must not release or trace the parent before that boundary.
        if threads.is_parked(&run) {
            return Ok(None);
        }
        if terminal.is_none() {
            let boundary = match Self::trace_start(&run, true) {
                Ok(boundary) => boundary,
                Err(error) => return Self::apply_error(threads, run, error),
            };
            if boundary == TraceBoundary::Park {
                if let Err(error) = threads.park(run.thread) {
                    return Self::apply_error(threads, run, Self::thread_error(error));
                }
                return Ok(None);
            }
        }
        if threads.has_parked() {
            std::thread::yield_now();
        }
        if terminal.is_none() {
            threads.release(&run).map_err(Self::thread_error)?;
        }
        Self::finish_optional(plan, threads, run, terminal)
    }

    fn trace_start(run: &threads::ThreadRun, exit: bool) -> Result<TraceBoundary, EngineError> {
        let Some(trace) = run.router.ptrace() else {
            return Ok(TraceBoundary::Continue);
        };
        let mut boundary = Err(EngineError::WaitFailed);
        let outcome = run.machine.handle_syscall(1, |cpu| {
            let original = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8],
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
            };
            boundary = trace
                .syscall_boundary(cpu, original, exit)
                .map_err(|_| EngineError::WaitFailed);
            StepOutcome::Continue
        });
        if outcome != StepOutcome::Continue {
            return Err(EngineError::WaitFailed);
        }
        boundary
    }

    fn trace_resume(run: &threads::ThreadRun) -> Result<TraceBoundary, EngineError> {
        let Some(trace) = run.router.ptrace() else {
            return Ok(TraceBoundary::Continue);
        };
        let mut boundary = Err(EngineError::WaitFailed);
        let outcome = run.machine.handle_syscall(1, |cpu| {
            boundary = trace.resume_boundary(cpu).map_err(|_| EngineError::WaitFailed);
            StepOutcome::Continue
        });
        if outcome != StepOutcome::Continue {
            return Err(EngineError::WaitFailed);
        }
        boundary
    }

    fn dispatch(
        isa: GuestIsa,
        memory: &ArenaMemory,
        run: &threads::ThreadRun,
    ) -> Result<(Option<ThreadTerminal>, bool), EngineError> {
        if Self::sigreturn(&run.machine)? {
            run.router.restore_signal().map_err(|()| EngineError::LaunchFailed)?;
            return Ok((None, false));
        }
        let outcome = hl_runtime::dispatch_runtime_syscall(&run.machine, 1, run.router.as_ref());
        if let StepOutcome::ReplaceImage { generation } = outcome {
            return Self::replace(&run.router, generation).map(|()| (None, true));
        }
        Self::dispatch_outcome(isa, memory, &run.router, outcome).map(|terminal| (terminal, false))
    }

    fn replace(router: &RuntimeSyscallRouter, generation: u64) -> Result<(), EngineError> {
        router
            .take_exec(generation)
            .ok_or(EngineError::WaitFailed)?
            .commit()
            .map_err(|_| EngineError::LaunchFailed)
    }

    fn signal_boundary(threads: &threads::ThreadSet, run: &threads::ThreadRun) -> Result<SignalTurn, EngineError> {
        let outcome = run.router.deliver_signal().map_err(|()| EngineError::LaunchFailed)?;
        if matches!(outcome, hl_runtime::SignalBoundaryOutcome::Handled) {
            run.cancellation.drain();
        }
        Ok(match outcome {
            hl_runtime::SignalBoundaryOutcome::Terminate { signal, dumped_core } => {
                Self::terminate_signal(run, signal, dumped_core)?
            }
            hl_runtime::SignalBoundaryOutcome::Trace { event, .. } => Self::trace_signal(run, event)?,
            hl_runtime::SignalBoundaryOutcome::None
            | hl_runtime::SignalBoundaryOutcome::Handled
            | hl_runtime::SignalBoundaryOutcome::Continue => SignalTurn::Continue,
            hl_runtime::SignalBoundaryOutcome::Stop { control_epoch } => {
                if threads
                    .install_stop_gate(run, control_epoch)
                    .map_err(Self::thread_error)?
                {
                    SignalTurn::GatePark
                } else {
                    SignalTurn::Continue
                }
            }
        })
    }

    fn terminate_signal(run: &threads::ThreadRun, signal: u8, dumped_core: bool) -> Result<SignalTurn, EngineError> {
        run.router
            .terminate_signal(signal, dumped_core)
            .map_err(|()| EngineError::LaunchFailed)?;
        Ok(SignalTurn::Terminal(ThreadTerminal::Group(Self::signal(i32::from(
            signal,
        )))))
    }

    fn trace_signal(run: &threads::ThreadRun, event: hl_task::TraceEvent) -> Result<SignalTurn, EngineError> {
        let trace = run.router.ptrace().ok_or(EngineError::WaitFailed)?;
        let mut boundary = Err(EngineError::WaitFailed);
        let result = run.machine.handle_syscall(1, |cpu| {
            let original = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8],
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0],
            };
            boundary = trace
                .signal_boundary(cpu, original, event)
                .map_err(|_| EngineError::WaitFailed);
            StepOutcome::Continue
        });
        if result != StepOutcome::Continue {
            return Err(EngineError::WaitFailed);
        }
        match boundary? {
            TraceBoundary::Park => Ok(SignalTurn::Park),
            _ => Err(EngineError::WaitFailed),
        }
    }

    fn sigreturn(machine: &ExecutionMachine) -> Result<bool, EngineError> {
        let mut selected = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            selected = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => cpu.registers[8] == 139,
                ExecutionCpuSnapshot::X86_64(cpu) => cpu.registers[0] == 15,
            };
            StepOutcome::Continue
        });
        match outcome {
            StepOutcome::Continue => Ok(selected),
            _ => Err(EngineError::WaitFailed),
        }
    }

    fn idle(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        waiters: &waiter::Pool,
    ) -> Result<CompletionTurn, EngineError> {
        let Some(event) = waiters.wait() else {
            return Ok(CompletionTurn::Idle);
        };
        Self::apply_event(isa, plan, threads, event)
    }

    fn finish_optional(
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        run: threads::ThreadRun,
        terminal: Option<ThreadTerminal>,
    ) -> Result<Option<EngineExit>, EngineError> {
        match terminal {
            Some(terminal) => Self::finish(plan, threads, run, terminal),
            None => Ok(None),
        }
    }

    fn complete(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        waiters: &waiter::Pool,
    ) -> Result<CompletionTurn, EngineError> {
        let Some(event) = waiters.completed() else {
            return Ok(CompletionTurn::Idle);
        };
        Self::apply_event(isa, plan, threads, event)
    }

    fn apply_event(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        event: waiter::Event,
    ) -> Result<CompletionTurn, EngineError> {
        match event {
            waiter::Event::Completion(completion) => Self::apply(isa, plan, threads, completion),
            waiter::Event::OrdinarySignal => {
                threads.interrupt_signals();
                Ok(CompletionTurn::Continue)
            }
            waiter::Event::Signal(activity) => {
                threads.process_control(activity);
                Ok(CompletionTurn::Continue)
            }
        }
    }

    fn apply(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        completion: waiter::Completion,
    ) -> Result<CompletionTurn, EngineError> {
        match threads.resume_run(&completion.run) {
            Ok(()) => {}
            Err(hl_runtime::RuntimeThreadError::Missing) => return Ok(CompletionTurn::Continue),
            Err(error) => return Err(Self::thread_error(error)),
        }
        let memory = completion.run.space.arena_memory();
        let terminal = Self::dispatch_outcome(isa, &memory, &completion.run.router, completion.outcome)?;
        if let Some(terminal) = terminal {
            if let Some(exit) = Self::finish(plan, threads, completion.run, terminal)? {
                return Ok(CompletionTurn::Exit(exit));
            }
        } else {
            threads.release(&completion.run).map_err(Self::thread_error)?;
        }
        Ok(CompletionTurn::Continue)
    }

    fn native_slice(
        run: &threads::ThreadRun,
        memory: &mut super::operand::SliceMemory<'_>,
        pool: &mut NativePool,
        native_budget: u64,
    ) -> Option<StepOutcome> {
        enum NativeCoordinates {
            Aarch64 { pc: u64, stack: u64 },
            X86_64,
        }
        let mut coordinates = None;
        let observed = run.machine.handle_syscall(1, |snapshot| {
            coordinates = Some(match snapshot {
                ExecutionCpuSnapshot::Aarch64(cpu) => NativeCoordinates::Aarch64 {
                    pc: cpu.pc,
                    stack: cpu.sp,
                },
                ExecutionCpuSnapshot::X86_64(_) => NativeCoordinates::X86_64,
            });
            StepOutcome::Continue
        });
        if observed != StepOutcome::Continue {
            return Some(observed);
        }
        let NativeCoordinates::Aarch64 { pc, stack } = coordinates? else {
            return Self::native_x86(run, memory, pool, native_budget);
        };
        let lease = super::operand::ImageMemory::lease(memory);
        let mappings = lease.mappings();
        let executable = mappings
            .ledger()
            .resolve(GuestAddress::new(pc), Protection::EXECUTE)?
            .region;
        let available = executable.range().end().get().saturating_sub(pc).min(256);
        let length = available - available % 4;
        if length == 0 {
            return None;
        }
        let source_range = hl_isa::AddressRange::nonempty(GuestAddress::new(pc), length).ok()?;
        let token = mappings.executable_token(source_range, lease.generation());
        let fallback_key = (
            run.process,
            lease.generation(),
            mappings.ledger().generation(),
            token.version,
            pc,
        );
        if pool.suppressed.contains(&fallback_key) {
            return None;
        }
        if !pool.observations.contains_key(&fallback_key) && pool.observations.len() == NATIVE_SITE_LIMIT {
            pool.enabled = false;
            return None;
        }
        let observations = pool.observations.entry(fallback_key).or_default();
        *observations = observations.saturating_add(1);
        if *observations < 2 {
            return None;
        }
        let mut bytes = [0_u8; 256];
        let bytes = &mut bytes[..usize::try_from(length).ok()?];
        mappings
            .read_spans(GuestAddress::new(pc), bytes, Protection::EXECUTE)
            .ok()?;
        let projection = if let Some((first, last)) =
            crate::native::InstructionWord::read(bytes).and_then(|instruction| instruction.literal_interval(pc))
        {
            mappings
                .project_contiguous(
                    GuestAddress::new(first),
                    last.checked_sub(first)?,
                    Protection::READ,
                    lease.generation(),
                )
                .ok()?
        } else {
            let stack_region = mappings
                .ledger()
                .resolve(
                    GuestAddress::new(stack.saturating_sub(1)),
                    Protection::READ.union(Protection::WRITE),
                )?
                .region;
            let stack_first = stack.saturating_sub(8 << 10).max(stack_region.range().start().get()) & !4095;
            let stack_last = stack_first
                .saturating_add(16 << 10)
                .min(stack_region.range().end().get());
            mappings
                .project_contiguous(
                    GuestAddress::new(stack_first),
                    stack_last.checked_sub(stack_first)?,
                    Protection::READ.union(Protection::WRITE),
                    lease.generation(),
                )
                .ok()?
        };
        pool.prepare_source(
            run.process,
            pc,
            pc + length,
            token,
            super::operand::ImageMemory::epoch(memory).writes,
            |first, last| {
                let range = hl_isa::AddressRange::nonempty(GuestAddress::new(first), last - first).ok()?;
                Some(mappings.executable_token(range, lease.generation()))
            },
        )?;
        let diagnostics = pool.diagnostics;
        let executor = pool.executor(run.process)?;
        let source = [crate::native::NativeSource { guest_first: pc, bytes }];
        let mut fallback = false;
        let mut fallback_pc = None;
        let mut statistics = None;
        let mut resolve = |target: u64, output: &mut [u8]| {
            let region = mappings
                .ledger()
                .resolve(GuestAddress::new(target), Protection::EXECUTE)?
                .region;
            let available = region
                .range()
                .end()
                .get()
                .saturating_sub(target)
                .min(output.len() as u64);
            let length = usize::try_from(available - available % 4).ok()?;
            if length == 0 {
                return None;
            }
            mappings
                .read_spans(GuestAddress::new(target), &mut output[..length], Protection::EXECUTE)
                .ok()?;
            let range = hl_isa::AddressRange::nonempty(GuestAddress::new(target), length as u64).ok()?;
            Some((length, mappings.executable_token(range, lease.generation())))
        };
        let outcome = run.machine.handle_syscall(1, |snapshot| {
            let ExecutionCpuSnapshot::Aarch64(cpu) = snapshot else {
                return StepOutcome::Fault(hl_execution::ExecutionFault::CacheEpoch);
            };
            let original = cpu.clone();
            let Ok((result, stats)) =
                executor.run_lease(cpu, &source, projection, token, &run.interrupt, native_budget, &mut resolve)
            else {
                *cpu = original;
                return StepOutcome::Fault(hl_execution::ExecutionFault::Frozen);
            };
            statistics = Some(stats);
            match result.exit {
                crate::native::NativeExit::Branch => StepOutcome::Continue,
                crate::native::NativeExit::Syscall => {
                    let instruction = result.instruction;
                    let next = instruction.wrapping_add(4);
                    cpu.pc = next;
                    StepOutcome::Syscall { instruction, next }
                }
                crate::native::NativeExit::Yield => StepOutcome::Yield,
                crate::native::NativeExit::Fallback | crate::native::NativeExit::Fault => {
                    fallback = true;
                    fallback_pc = Some(result.instruction);
                    StepOutcome::Continue
                }
                crate::native::NativeExit::Epoch | crate::native::NativeExit::Interrupt => {
                    Self::native_boundary(cpu, original, result.exit, result.instruction, result.executed)
                        .expect("native boundary exit")
                }
                crate::native::NativeExit::Fatal => {
                    if diagnostics {
                        eprintln!(
                            "hl-native-fatal: isa=aarch64 pc={pc:#x} instruction={:#x} code={} remaining={} executed={}",
                            result.instruction, result.code, result.remaining, result.executed,
                        );
                    }
                    StepOutcome::Fault(hl_execution::ExecutionFault::Frozen)
                }
            }
        });
        if let Some(stats) = statistics {
            pool.counters.runs += 1;
            pool.counters.builds += stats.builds;
            pool.counters.hits += stats.hits;
            pool.merge_observed_sources(run.process, lease.generation(), stats.sources, stats.sources_complete)?;
        }
        if fallback {
            pool.counters.fallbacks += 1;
            if let Some(pc) = fallback_pc {
                let instruction = (
                    run.process,
                    lease.generation(),
                    mappings.ledger().generation(),
                    token.version,
                    pc,
                );
                pool.record_fallback(fallback_key, instruction);
            }
            Some(run.machine.run_step(1, memory))
        } else {
            Some(outcome)
        }
    }

    fn native_x86(
        run: &threads::ThreadRun,
        memory: &mut super::operand::SliceMemory<'_>,
        pool: &mut NativePool,
        native_budget: u64,
    ) -> Option<StepOutcome> {
        let mut coordinates = None;
        let observed = run.machine.handle_syscall(1, |snapshot| {
            if let ExecutionCpuSnapshot::X86_64(cpu) = snapshot {
                coordinates = Some((cpu.rip, cpu.registers[4]));
            }
            StepOutcome::Continue
        });
        if observed != StepOutcome::Continue {
            return Some(observed);
        }
        let (pc, stack) = coordinates?;
        let lease = super::operand::ImageMemory::lease(memory);
        let mappings = lease.mappings();
        let snapshot = mappings.snapshot();
        let executable = snapshot.regions.iter().copied().find(|region| {
            region.range().contains(GuestAddress::new(pc)) && region.protection().contains(Protection::EXECUTE)
        })?;
        let length = usize::try_from(executable.range().end().get().saturating_sub(pc).min(256)).ok()?;
        if length == 0 {
            return None;
        }
        let source_range = hl_isa::AddressRange::nonempty(GuestAddress::new(pc), length as u64).ok()?;
        let token = mappings.executable_token(source_range, lease.generation());
        let key = (
            run.process,
            lease.generation(),
            mappings.ledger().generation(),
            token.version,
            pc,
        );
        if pool.suppressed.contains(&key) {
            return None;
        }
        if !pool.observations.contains_key(&key) && pool.observations.len() == NATIVE_SITE_LIMIT {
            pool.enabled = false;
            return None;
        }
        let observations = pool.observations.entry(key).or_default();
        *observations = observations.saturating_add(1);
        if *observations < 2 {
            return None;
        }
        let mut bytes = vec![0_u8; length];
        mappings
            .read_spans(GuestAddress::new(pc), &mut bytes, Protection::EXECUTE)
            .ok()?;
        let stack_region = snapshot.regions.iter().copied().find(|region| {
            region.range().contains(GuestAddress::new(stack.saturating_sub(1)))
                && region.protection().contains(Protection::READ.union(Protection::WRITE))
        })?;
        let stack_first = stack.saturating_sub(8 << 10).max(stack_region.range().start().get()) & !4095;
        let stack_last = stack_first
            .saturating_add(16 << 10)
            .min(stack_region.range().end().get());
        let stack_projection = mappings
            .project_contiguous(
                GuestAddress::new(stack_first),
                stack_last.checked_sub(stack_first)?,
                Protection::READ.union(Protection::WRITE),
                lease.generation(),
            )
            .ok()?;
        pool.prepare_source(
            run.process,
            pc,
            pc + length as u64,
            token,
            super::operand::ImageMemory::epoch(memory).writes,
            |first, last| {
                let range = hl_isa::AddressRange::nonempty(GuestAddress::new(first), last - first).ok()?;
                Some(mappings.executable_token(range, lease.generation()))
            },
        )?;
        let diagnostics = pool.boundaries.is_some();
        let executor = pool.executor(run.process)?;
        let source = [crate::native::NativeSource {
            guest_first: pc,
            bytes: &bytes,
        }];
        let mut resolve = |target: u64, output: &mut [u8]| {
            let region = snapshot.regions.iter().copied().find(|region| {
                region.range().contains(GuestAddress::new(target)) && region.protection().contains(Protection::EXECUTE)
            })?;
            let length = usize::try_from(
                region
                    .range()
                    .end()
                    .get()
                    .saturating_sub(target)
                    .min(output.len() as u64),
            )
            .ok()?;
            if length == 0 {
                return None;
            }
            mappings
                .read_spans(GuestAddress::new(target), &mut output[..length], Protection::EXECUTE)
                .ok()?;
            let range = hl_isa::AddressRange::nonempty(GuestAddress::new(target), length as u64).ok()?;
            Some((length, mappings.executable_token(range, lease.generation())))
        };
        let mut fallback = None;
        let mut statistics = None;
        let mut boundary = None;
        let mut stack_projection = Some(stack_projection);
        let outcome = run.machine.handle_syscall(1, |snapshot| {
            let ExecutionCpuSnapshot::X86_64(cpu) = snapshot else {
                return StepOutcome::Fault(hl_execution::ExecutionFault::CacheEpoch);
            };
            let original = cpu.clone();
            let Some(projection) = stack_projection.take() else {
                return StepOutcome::Fault(hl_execution::ExecutionFault::Frozen);
            };
            let Ok((result, stats)) =
                executor.run_x86_lease(cpu, &source, projection, token, native_budget, false, &mut resolve)
            else {
                *cpu = original;
                return StepOutcome::Fault(hl_execution::ExecutionFault::Frozen);
            };
            statistics = Some(stats);
            boundary = diagnostics.then_some(NativeBoundary {
                process: run.process,
                start: pc,
                exit: result.exit,
                instruction: result.instruction,
                next: result.next,
                rip: cpu.rip,
                stack: cpu.registers[4],
                registers: [
                    cpu.registers[0],
                    cpu.registers[1],
                    cpu.registers[2],
                    cpu.registers[3],
                    cpu.registers[4],
                    cpu.registers[5],
                    cpu.registers[6],
                    cpu.registers[7],
                ],
                vectors: [cpu.vectors[0], cpu.vectors[1]],
                flags: cpu.flags.bits(),
                mxcsr: cpu.mxcsr,
                fault_address: result.address,
                provenance: token.version,
            });
            match result.exit {
                crate::native::NativeExit::Branch => StepOutcome::Continue,
                crate::native::NativeExit::Syscall => {
                    cpu.rip = result.next;
                    StepOutcome::Syscall {
                        instruction: result.instruction,
                        next: result.next,
                    }
                }
                crate::native::NativeExit::Yield if Self::x86_yield_needs_interpreter(result.exit, result.executed) => {
                    fallback = Some(result.instruction);
                    StepOutcome::Continue
                }
                crate::native::NativeExit::Yield => StepOutcome::Yield,
                crate::native::NativeExit::Fallback | crate::native::NativeExit::Fault => {
                    fallback = Some(result.instruction);
                    StepOutcome::Continue
                }
                crate::native::NativeExit::Epoch => {
                    if Self::epoch_rewinds(result.executed) {
                        *cpu = original;
                    }
                    StepOutcome::Yield
                }
                crate::native::NativeExit::Interrupt => {
                    *cpu = original;
                    StepOutcome::Yield
                }
                crate::native::NativeExit::Fatal => StepOutcome::Fault(hl_execution::ExecutionFault::Frozen),
            }
        });
        if let Some(stats) = statistics {
            pool.counters.runs += 1;
            pool.counters.builds += stats.builds;
            pool.counters.hits += stats.hits;
            pool.merge_observed_sources(run.process, lease.generation(), stats.sources, stats.sources_complete)?;
        }
        if let Some(record) = boundary
            && let Some(boundaries) = &mut pool.boundaries
        {
            boundaries.push(record);
        }
        drop(stack_projection);
        if let Some(fallback_pc) = fallback {
            pool.counters.fallbacks += 1;
            pool.record_fallback(
                key,
                (
                    run.process,
                    lease.generation(),
                    mappings.ledger().generation(),
                    token.version,
                    fallback_pc,
                ),
            );
            Some(run.machine.run_step(1, memory))
        } else {
            Some(outcome)
        }
    }

    fn step<M>(isa: GuestIsa, memory: &M, outcome: StepOutcome) -> Result<TurnAction, EngineError>
    where
        M: hl_execution::ExecutionInstructionMemory + super::operand::ImageMemory,
    {
        match outcome {
            StepOutcome::Yield | StepOutcome::Continue => Ok(TurnAction::Continue),
            StepOutcome::ReplaceImage { generation } => Ok(TurnAction::Replace(generation)),
            StepOutcome::Syscall { .. } => Ok(TurnAction::Dispatch),
            StepOutcome::Exit { status } => Ok(TurnAction::Terminal(ThreadTerminal::Thread(Self::code(status)))),
            StepOutcome::Fault(fault) => Ok(Self::fault_signal(memory, fault).map_or_else(
                || TurnAction::Terminal(ThreadTerminal::Thread(Self::fault(isa, memory, fault))),
                |(signal, code, address)| TurnAction::Signal {
                    signal,
                    code,
                    address,
                    fallback: Self::fault(isa, memory, fault),
                },
            )),
        }
    }

    fn fault_signal<M>(memory: &M, fault: hl_execution::ExecutionFault) -> Option<(u8, i32, u64)>
    where
        M: super::operand::ImageMemory,
    {
        let (address, span) = match fault {
            hl_execution::ExecutionFault::Memory(value) => (value.address, None),
            hl_execution::ExecutionFault::Operand(value) => (value.address(), Some(value.length())),
            hl_execution::ExecutionFault::Alignment { address, .. } => {
                return Some((7, 1, address));
            }
            hl_execution::ExecutionFault::Decode { instruction }
            | hl_execution::ExecutionFault::Unsupported { instruction } => {
                return Some((4, 2, instruction));
            }
            hl_execution::ExecutionFault::Signal(trap) => {
                let signal = match trap.signal {
                    hl_execution::TrapSignal::Illegal => 4,
                    hl_execution::TrapSignal::Divide => 8,
                    hl_execution::TrapSignal::Breakpoint => 5,
                };
                return Some((signal, trap.code, trap.address));
            }
            _ => return None,
        };
        let lease = memory.selected_lease();
        let (signal, code, reported) = Self::classify(memory, &lease, address, span);
        Some((signal, code, reported))
    }

    fn classify(
        memory: &impl super::operand::ImageMemory,
        lease: &super::space::SpaceLease,
        address: u64,
        span: Option<u64>,
    ) -> (u8, i32, u64) {
        let bus = span.and_then(|length| memory.address_space().resolve_bus(lease, address, length));
        if let Some(fault) = bus.filter(|fault| lease.arena().take_bus(*fault)) {
            (7, 2, fault)
        } else {
            let mapped = lease
                .mappings_ref()
                .ledger()
                .regions()
                .iter()
                .any(|region| region.range().contains(hl_isa::GuestAddress::new(address)));
            (11, if mapped { 2 } else { 1 }, address)
        }
    }

    fn blocks(
        isa: GuestIsa,
        machine: &ExecutionMachine,
        router: &RuntimeSyscallRouter,
        solo: bool,
    ) -> Result<bool, EngineError> {
        let mut blocking = false;
        let outcome = machine.handle_syscall(1, |cpu| {
            let architecture = match isa {
                GuestIsa::Aarch64 => hl_isa::GuestArchitecture::Aarch64,
                GuestIsa::X86_64 => hl_isa::GuestArchitecture::X86_64,
            };
            let (number, operation) = match cpu {
                ExecutionCpuSnapshot::Aarch64(cpu) => (cpu.registers[8], cpu.registers[1]),
                ExecutionCpuSnapshot::X86_64(cpu) => (cpu.registers[0], cpu.registers[6]),
            };
            blocking = matches!((number, operation & 127), (98 | 202, 0 | 6 | 9 | 11 | 13))
                || matches!(number, 35 | 61 | 101 | 115 | 230 | 260)
                || Self::record_lock_blocks(isa, number, operation)
                || (Self::descriptor_blocks(isa, number) && (!solo || router.descriptor_may_block(architecture, cpu)))
                || Self::ipc_blocks(isa, number)
                || Self::signal_blocks(isa, number)
                || Self::readiness_blocks(isa, number);
            blocking = blocking || router.filesystem_may_block(architecture, cpu);
            StepOutcome::Continue
        });
        match outcome {
            StepOutcome::Continue => Ok(blocking),
            _ => Err(EngineError::WaitFailed),
        }
    }

    fn dispatch_outcome(
        isa: GuestIsa,
        memory: &ArenaMemory,
        router: &RuntimeSyscallRouter,
        outcome: StepOutcome,
    ) -> Result<Option<ThreadTerminal>, EngineError> {
        match outcome {
            StepOutcome::Continue | StepOutcome::Yield => Ok(None),
            StepOutcome::ReplaceImage { generation } => Self::replace(router, generation).map(|()| None),
            StepOutcome::Exit { status } => {
                let exit = Self::code(status);
                Ok(Some(match router.take_terminal() {
                    Some(hl_runtime::RuntimeTerminal::Group(_)) => ThreadTerminal::Group(exit),
                    _ => ThreadTerminal::Thread(exit),
                }))
            }
            StepOutcome::Fault(fault) => Ok(Some(ThreadTerminal::Thread(Self::fault(isa, memory, fault)))),
            StepOutcome::Syscall { .. } => Err(EngineError::WaitFailed),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::sync::Arc;

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
        assert!(!NativePool::new(GuestIsa::Aarch64, &disabled, None).enabled);
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
        let site = (process, 2, 3, 4, 0x5000);
        let changed = (process, 2, 3, 5, 0x5000);
        let mut sites = BTreeSet::new();
        sites.insert(site);
        assert!(sites.contains(&site));
        assert!(!sites.contains(&changed));
    }

    #[test]
    fn fallback_site_is_distinct_from_suppressed_entry() {
        let process = hl_task::ProcessId::from_wire(1, 1).unwrap();
        let entry = (process, 2, 3, 4, 0x400990);
        let instruction = (process, 2, 3, 4, 0x40b0c0);
        let mut pool = NativePool::new(GuestIsa::Aarch64, &plan(crate::options::Options::default()), None);
        pool.record_fallback(entry, instruction);
        assert_eq!(pool.suppressed, BTreeSet::from([entry]));
        assert_eq!(pool.fallbacks, BTreeSet::from([instruction]));
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
