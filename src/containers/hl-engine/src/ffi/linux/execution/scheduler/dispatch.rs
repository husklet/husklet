//! Syscall dispatch and signal-boundary turns for the scheduler.

use hl_execution::{ExecutionCpuSnapshot, ExecutionMachine, StepOutcome};
use hl_runtime::{RuntimeSyscallRouter, TraceBoundary};

use super::super::{ArenaMemory, GuestExecutor, threads, waiter};
use super::pool::NativePool;
use super::{CompletionTurn, SignalTurn, ThreadTerminal};
use crate::activation::GuestIsa;
use crate::engine::{EngineError, EngineExit};
use crate::launch_plan::RuntimeLaunchPlan;

impl GuestExecutor {
    pub(super) fn dispatch_ready(
        isa: GuestIsa,
        plan: &RuntimeLaunchPlan,
        threads: &threads::ThreadSet,
        waiters: &waiter::Pool,
        run: threads::ThreadRun,
        native: &mut NativePool,
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
            if native.reset_process(run.process).is_none() {
                native.disable();
            }
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

    pub(super) fn trace_start(run: &threads::ThreadRun, exit: bool) -> Result<TraceBoundary, EngineError> {
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

    pub(super) fn trace_resume(run: &threads::ThreadRun) -> Result<TraceBoundary, EngineError> {
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

    pub(super) fn dispatch(
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

    pub(super) fn replace(router: &RuntimeSyscallRouter, generation: u64) -> Result<(), EngineError> {
        router
            .take_exec(generation)
            .ok_or(EngineError::WaitFailed)?
            .commit()
            .map_err(|_| EngineError::LaunchFailed)
    }

    pub(super) fn signal_boundary(
        threads: &threads::ThreadSet,
        run: &threads::ThreadRun,
    ) -> Result<SignalTurn, EngineError> {
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

    pub(super) fn idle(
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

    pub(super) fn complete(
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
            Err(threads::ResumeReject::Retired) => return Ok(CompletionTurn::Continue),
            Err(threads::ResumeReject::Live(ownership)) => {
                // Dropping this completion strands a live thread: nothing else
                // will ever return it to the ready set.
                hl_log::hl_error!(
                    hl_log::tag::TASK,
                    "completion dropped for live thread thread={:?} process={:?} generation={} ownership={:?} lost={}",
                    completion.run.thread,
                    completion.run.process,
                    completion.run.generation,
                    ownership,
                    threads.lost_completions(),
                );
                return Ok(CompletionTurn::Continue);
            }
            Err(threads::ResumeReject::Invalid) => {
                return Err(Self::thread_error(hl_runtime::RuntimeThreadError::Invalid));
            }
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
}
