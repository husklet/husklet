//! Execution-memory, trap, and run-loop adapters.

mod r#loop;
mod memory;
mod trap;

pub use r#loop::{RuntimeExecutionLoop, RuntimeExecutionOutcome};
pub use memory::RuntimeExecutionMemory;
pub use trap::{RuntimeSyscallTrap, RuntimeTrapOutcome, dispatch_runtime_syscall};

#[cfg(test)]
mod lifecycle_test;
