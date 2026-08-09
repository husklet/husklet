mod cache_stats;
mod codec;
mod dispatch;
mod runner;
mod state;

pub use dispatch::{
    BlockIdentity, CacheObservation, DispatchDecision, DispatchError, TranslationEmission, TranslationRequest,
};
pub use runner::{
    ExecutionFault, ExecutionInstructionMemory, INTERPRETED_BLOCKS, INTERPRETED_INSTRUCTIONS, INTERPRETED_SLICES,
    InstructionEpoch, StepOutcome, SynchronousTrap, TrapSignal, TrapState,
};
pub use state::{
    ArchitecturalCounter, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionMachine, ExecutionSnapshot,
    ExecutionStateError, GUEST_COUNTER_FREQUENCY_HZ,
};

#[cfg(test)]
mod runner_test;

#[cfg(test)]
mod state_test;
