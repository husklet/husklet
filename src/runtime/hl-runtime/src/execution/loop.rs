use std::sync::{Arc, Mutex};

use hl_execution::{ExecutionFault, ExecutionInstructionMemory, ExecutionMachine, StepOutcome};
use hl_task::{TaskRegistry, ThreadId};

use crate::{RuntimeSafepoint, RuntimeSyscallTrap, dispatch_runtime_syscall};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeExecutionOutcome {
    Yield,
    SignalPending,
    ReplaceImage,
    Exit(i32),
    Fault(ExecutionFault),
}

pub struct RuntimeExecutionLoop<M: ExecutionInstructionMemory> {
    machine: Arc<ExecutionMachine>,
    memory: Mutex<M>,
    trap: Arc<dyn RuntimeSyscallTrap>,
    tasks: Arc<TaskRegistry>,
    thread: ThreadId,
    cache_epoch: u64,
    slice_budget: u64,
    trace: Option<Arc<RuntimeSafepoint>>,
}

impl<M: ExecutionInstructionMemory> RuntimeExecutionLoop<M> {
    pub fn new(
        machine: Arc<ExecutionMachine>,
        memory: M,
        trap: Arc<dyn RuntimeSyscallTrap>,
        tasks: Arc<TaskRegistry>,
        thread: ThreadId,
        cache_epoch: u64,
        slice_budget: u64,
    ) -> Result<Self, ExecutionFault> {
        if cache_epoch == 0 || slice_budget == 0 {
            return Err(ExecutionFault::CacheEpoch);
        }
        Ok(Self {
            machine,
            memory: Mutex::new(memory),
            trap,
            tasks,
            thread,
            cache_epoch,
            slice_budget,
            trace: None,
        })
    }

    #[must_use]
    pub fn with_trace(mut self, trace: Arc<RuntimeSafepoint>) -> Self {
        self.trace = Some(trace);
        self
    }

    pub fn run_slice(&self) -> RuntimeExecutionOutcome {
        let mut memory = self.memory.lock().unwrap_or_else(|error| error.into_inner());
        match self
            .machine
            .run_slice(self.cache_epoch, self.slice_budget, &mut *memory)
        {
            StepOutcome::Syscall { .. } => self.run_syscall(),
            StepOutcome::Yield | StepOutcome::Continue => self.safe_point(),
            StepOutcome::ReplaceImage { .. } => RuntimeExecutionOutcome::ReplaceImage,
            StepOutcome::Exit { status } => RuntimeExecutionOutcome::Exit(status),
            StepOutcome::Fault(fault) => RuntimeExecutionOutcome::Fault(fault),
        }
    }

    fn run_syscall(&self) -> RuntimeExecutionOutcome {
        let original = match self.trace.as_ref() {
            Some(_) => match self.syscall_number() {
                Ok(number) => Some(number),
                Err(fault) => return RuntimeExecutionOutcome::Fault(fault),
            },
            None => None,
        };
        if let Some(outcome) = self.trace_boundary(original, false) {
            return outcome;
        }
        match dispatch_runtime_syscall(&self.machine, self.cache_epoch, self.trap.as_ref()) {
            StepOutcome::Continue => self.trace_boundary(original, true).unwrap_or_else(|| self.safe_point()),
            StepOutcome::ReplaceImage { .. } => RuntimeExecutionOutcome::ReplaceImage,
            StepOutcome::Exit { status } => RuntimeExecutionOutcome::Exit(status),
            StepOutcome::Fault(fault) => RuntimeExecutionOutcome::Fault(fault),
            _ => RuntimeExecutionOutcome::Fault(ExecutionFault::Protocol),
        }
    }

    fn trace_boundary(&self, original: Option<u64>, exit: bool) -> Option<RuntimeExecutionOutcome> {
        self.trace_syscall(original?, exit)
    }

    fn syscall_number(&self) -> Result<u64, ExecutionFault> {
        let mut number = 0;
        match self.machine.handle_syscall(self.cache_epoch, |cpu| {
            number = cpu.syscall_number();
            StepOutcome::Continue
        }) {
            StepOutcome::Continue => Ok(number),
            StepOutcome::Fault(fault) => Err(fault),
            _ => Err(ExecutionFault::Protocol),
        }
    }

    fn trace_syscall(&self, original: u64, exit: bool) -> Option<RuntimeExecutionOutcome> {
        let trace = self.trace.as_ref()?;
        let result = self
            .machine
            .handle_syscall(self.cache_epoch, |cpu| match trace.syscall(cpu, original, exit) {
                Ok(Some(hl_task::TraceResume::Kill)) => StepOutcome::Exit { status: 137 },
                Ok(_) => StepOutcome::Continue,
                Err(_) => StepOutcome::Fault(ExecutionFault::Protocol),
            });
        match result {
            StepOutcome::Continue => None,
            StepOutcome::Exit { status } => Some(RuntimeExecutionOutcome::Exit(status)),
            StepOutcome::Fault(fault) => Some(RuntimeExecutionOutcome::Fault(fault)),
            _ => Some(RuntimeExecutionOutcome::Fault(ExecutionFault::Protocol)),
        }
    }

    fn safe_point(&self) -> RuntimeExecutionOutcome {
        if let Some(signal) = self.tasks.prepare_forced_delivery(self.thread) {
            drop(signal);
            RuntimeExecutionOutcome::SignalPending
        } else {
            RuntimeExecutionOutcome::Yield
        }
    }
}

trait CpuSyscallNumber {
    fn syscall_number(&self) -> u64;
}

impl CpuSyscallNumber for hl_execution::ExecutionCpuSnapshot {
    fn syscall_number(&self) -> u64 {
        match self {
            Self::X86_64(cpu) => cpu.registers[0],
            Self::Aarch64(cpu) => cpu.registers[8],
        }
    }
}
