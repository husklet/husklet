use crate::{
    Aarch64Decoder, Aarch64ExecutionExit, Aarch64Interpreter, Aarch64Ir, AccessKind, DecodeError, ExclusiveMemory,
    ExecutionCpuSnapshot, ExecutionExit, ExecutionMachine, GuestOperandMemory, GuestSystemPort, MemoryFault,
    PcCoordinatePort, ScalarInterpreter, ScalarIr, ScalarIrError, X86ScalarDecoder,
    aarch64::register::RegisterExecutor,
};
/// Guest instructions and basic blocks the Rust interpreter retired, as against the
/// `completed` counter the native executor reports for translated code. Accumulated once
/// per slice so the per-instruction path stays free of atomics.
pub static INTERPRETED_INSTRUCTIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static INTERPRETED_BLOCKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static INTERPRETED_SLICES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Publishes a slice's interpreter tally on every exit path, including the early returns
/// a fault or an epoch mismatch takes.
struct InterpreterTally {
    instructions: u64,
    blocks: u64,
}

impl Drop for InterpreterTally {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        INTERPRETED_SLICES.fetch_add(1, Relaxed);
        INTERPRETED_INSTRUCTIONS.fetch_add(self.instructions, Relaxed);
        INTERPRETED_BLOCKS.fetch_add(self.blocks, Relaxed);
    }
}

const X86_MAXIMUM_INSTRUCTION: usize = 15;
const GUEST_PAGE: usize = 4096;

/// Why an x86 instruction could not be turned into IR: bytes it needs are unmapped,
/// or the bytes are present and are not an instruction.
enum X86FetchFailure {
    Fetch,
    Decode,
}

const BLOCK_INSTRUCTIONS: usize = 64;
const BLOCK_LIMIT: usize = 4096;
#[derive(Clone, Debug)]
struct X86Block {
    instructions: Vec<ScalarIr>,
}
#[derive(Debug)]
pub(super) struct X86BlockCache {
    epoch: Option<InstructionEpoch>,
    blocks: Vec<Option<(u64, X86Block)>>,
}
impl Default for X86BlockCache {
    fn default() -> Self {
        Self {
            epoch: None,
            blocks: vec![None; BLOCK_LIMIT],
        }
    }
}
impl X86BlockCache {
    fn synchronize(&mut self, epoch: InstructionEpoch) {
        if self.epoch != Some(epoch) {
            self.epoch = Some(epoch);
            self.blocks.fill(None);
        }
    }

    fn get(&self, address: u64) -> Option<&X86Block> {
        let (stored, block) = self.blocks[(address as usize >> 1) & (BLOCK_LIMIT - 1)].as_ref()?;
        (*stored == address).then_some(block)
    }

    fn insert(&mut self, address: u64, block: X86Block) {
        self.blocks[(address as usize >> 1) & (BLOCK_LIMIT - 1)] = Some((address, block));
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InstructionEpoch {
    pub incarnation: u64,
    pub mappings: u64,
    pub writes: u64,
}
#[derive(Clone, Debug)]
struct Aarch64Block {
    instructions: Vec<Aarch64Ir>,
    /// Bit `n` marks instruction `n` as a plain store, whose commit is the only
    /// thing inside a block that can advance the instruction epoch. `BLOCK_INSTRUCTIONS`
    /// is 64, so one word covers every position.
    stores: u64,
}
#[derive(Debug)]
pub(super) struct Aarch64BlockCache {
    epoch: Option<InstructionEpoch>,
    generation: u64,
    blocks: Vec<Option<(u64, Aarch64Block)>>,
    mask: usize,
    stats: Option<Box<super::cache_stats::CacheStats>>,
}
impl Default for Aarch64BlockCache {
    fn default() -> Self {
        // Measurement hook: sizing the cache from the environment lets one binary
        // supply both arms of a capacity comparison.
        let entries = std::env::var("HL_BLOCK_CACHE_ENTRIES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| value.is_power_of_two())
            .unwrap_or(BLOCK_LIMIT);
        Self {
            epoch: None,
            generation: 1,
            blocks: vec![None; entries],
            mask: entries - 1,
            stats: super::cache_stats::CacheStats::enabled(entries).map(Box::new),
        }
    }
}
impl Aarch64BlockCache {
    pub(super) fn clear(&mut self) {
        self.epoch = None;
        self.blocks.fill(None);
        if let Some(stats) = self.stats.as_mut() {
            stats.flush();
        }
    }

    fn synchronize(&mut self, epoch: InstructionEpoch) {
        if self.epoch != Some(epoch) {
            self.clear();
            self.epoch = Some(epoch);
            self.generation = self.generation.wrapping_add(1).max(1);
        }
    }

    fn insert(&mut self, address: u64, block: Aarch64Block) {
        let index = self.index(address);
        if let Some(stats) = self.stats.as_mut() {
            let occupied = self.blocks[index].as_ref().map(|(stored, _)| *stored);
            stats.insert(address, index, occupied, block.instructions.len());
        }
        self.blocks[index] = Some((address, block));
    }

    fn get(&self, address: u64) -> Option<&Aarch64Block> {
        let (stored, block) = self.blocks[self.index(address)].as_ref()?;
        (*stored == address).then_some(block)
    }

    /// The one lookup per executed block, which is where the hit rate is defined.
    fn probe(&mut self, address: u64) -> bool {
        let hit = self.get(address).is_some();
        if let Some(stats) = self.stats.as_mut() {
            stats.lookup(address, hit);
        }
        hit
    }

    fn index(&self, address: u64) -> usize {
        (address as usize >> 2) & self.mask
    }
}

#[cfg(test)]
mod cache_test {
    use super::*;

    /// `ic ivau` ends the block and publishes through the memory port; the publication it
    /// causes is what discards translations, wholesale, on this executor and on every peer.
    #[test]
    fn instruction_flush_is_terminal_and_a_publication_discards_every_block() {
        let initial = InstructionEpoch {
            incarnation: 1,
            mappings: 2,
            writes: 3,
        };
        let published = InstructionEpoch { writes: 4, ..initial };
        let instruction = Aarch64Decoder::decode(0xd503_201f).unwrap();
        assert!(ExecutionMachine::ends_block(
            crate::Aarch64Instruction::InstructionCache { source: 0 }
        ));
        let mut cache = Aarch64BlockCache::default();
        cache.synchronize(initial);
        for address in [0x1000, 0x2000] {
            cache.insert(
                address,
                Aarch64Block {
                    instructions: vec![instruction],
                    stores: 0,
                },
            );
        }

        cache.synchronize(published);

        assert!(cache.get(0x1000).is_none());
        assert!(cache.get(0x2000).is_none());
    }
}
pub trait InstructionMemory: GuestOperandMemory + ExclusiveMemory {
    fn fetch(&self, address: u64, bytes: &mut [u8]) -> Result<usize, ()>;

    /// Changes whenever executable mappings or their bytes can have changed.
    /// `None` keeps execution correct by disabling decoded-block retention.
    fn instruction_epoch(&self) -> Option<InstructionEpoch> {
        None
    }

    /// Publishes guest instruction-cache maintenance after prior writes.
    fn invalidate_instruction(&mut self, _address: u64) {}
}
pub use InstructionMemory as ExecutionInstructionMemory;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Fault {
    Frozen,
    CacheEpoch,
    Protocol,
    /// A native engine invariant failed; `code` names which one.
    NativeFatal {
        code: u64,
    },
    Fetch(MemoryFault),
    Memory(MemoryFault),
    Operand(crate::FaultAccess),
    Alignment {
        instruction: u64,
        address: u64,
        access: AccessKind,
    },
    Decode {
        instruction: u64,
    },
    Unsupported {
        instruction: u64,
    },
    Signal(SynchronousTrap),
}
pub type ExecutionFault = Fault;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrapSignal {
    Illegal,
    Divide,
    Breakpoint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrapState {
    Faulting,
    Completed { next: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SynchronousTrap {
    pub signal: TrapSignal,
    pub code: i32,
    pub address: u64,
    pub instruction: u64,
    pub state: TrapState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    Continue,
    Syscall { instruction: u64, next: u64 },
    ReplaceImage { generation: u64 },
    Yield,
    Exit { status: i32 },
    Fault(ExecutionFault),
}

struct IdentityCoordinates;
/// Samples the architectural counter on first guest read and memoizes it, so an
/// instruction that never reads `CounterValue` costs no host clock read.
struct RunnerSystem<'a> {
    invalidated: Option<u64>,
    counter: &'a dyn crate::ArchitecturalCounter,
    sampled: std::cell::Cell<Option<u64>>,
}

impl<'a> RunnerSystem<'a> {
    fn new(counter: &'a dyn crate::ArchitecturalCounter) -> Self {
        Self {
            invalidated: None,
            counter,
            sampled: std::cell::Cell::new(None),
        }
    }
}

impl GuestSystemPort for RunnerSystem<'_> {
    fn barrier(&mut self, _kind: crate::BarrierKind, _option: u8) {}
    fn counter_frequency(&self) -> u64 {
        crate::GUEST_COUNTER_FREQUENCY_HZ
    }
    fn counter_value(&self) -> u64 {
        if let Some(value) = self.sampled.get() {
            return value;
        }
        let value = self.counter.read();
        self.sampled.set(Some(value));
        value
    }
    fn invalidate_instruction(&mut self, address: u64) {
        self.invalidated = Some(address);
    }
}

impl PcCoordinatePort for IdentityCoordinates {
    fn architectural_pc(&self, execution_pc: u64) -> u64 {
        execution_pc
    }
}

impl ExecutionMachine {
    pub fn handle_syscall<F>(&self, expected_epoch: u64, handler: F) -> StepOutcome
    where
        F: FnOnce(&mut ExecutionCpuSnapshot) -> StepOutcome,
    {
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return StepOutcome::Fault(ExecutionFault::Frozen);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return StepOutcome::Fault(ExecutionFault::Frozen);
        }
        if state.cache_epoch != expected_epoch {
            return StepOutcome::Fault(ExecutionFault::CacheEpoch);
        }
        handler(&mut state.cpu)
    }

    pub fn run_step<M: ExecutionInstructionMemory>(&self, expected_epoch: u64, memory: &mut M) -> StepOutcome {
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return StepOutcome::Fault(ExecutionFault::Frozen);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return StepOutcome::Fault(ExecutionFault::Frozen);
        }
        if state.cache_epoch != expected_epoch {
            return StepOutcome::Fault(ExecutionFault::CacheEpoch);
        }
        self.run_locked_step(&mut state.cpu, memory, None)
    }

    fn run_locked_step<M: ExecutionInstructionMemory>(
        &self,
        cpu: &mut ExecutionCpuSnapshot,
        memory: &mut M,
        retained: Option<ScalarIr>,
    ) -> StepOutcome {
        match cpu {
            ExecutionCpuSnapshot::Aarch64(cpu) => self.step_aarch64(cpu, memory),
            ExecutionCpuSnapshot::X86_64(cpu) => {
                let instruction = cpu.rip;
                let decoded = if let Some(decoded) = retained {
                    decoded
                } else {
                    match Self::decode_x86_at(instruction, memory) {
                        Ok(decoded) => decoded,
                        Err(X86FetchFailure::Fetch) => {
                            return StepOutcome::Fault(ExecutionFault::Fetch(MemoryFault {
                                instruction,
                                address: instruction,
                                access: AccessKind::Execute,
                            }));
                        }
                        Err(X86FetchFailure::Decode) => {
                            return StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                                signal: TrapSignal::Illegal,
                                code: 2,
                                address: instruction,
                                instruction,
                                state: TrapState::Faulting,
                            }));
                        }
                    }
                };
                match ScalarInterpreter::execute(cpu, memory, decoded) {
                    ExecutionExit::Continue => StepOutcome::Continue,
                    ExecutionExit::Syscall { instruction, next } => {
                        cpu.rip = next;
                        StepOutcome::Syscall { instruction, next }
                    }
                    ExecutionExit::TimestampCounter { next, auxiliary, .. } => {
                        let value = self
                            .timestamp_counter
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        cpu.write_register(crate::ScalarRegister::General(0), crate::ScalarWidth::Dword, value);
                        cpu.write_register(
                            crate::ScalarRegister::General(2),
                            crate::ScalarWidth::Dword,
                            value >> 32,
                        );
                        if auxiliary {
                            cpu.write_register(crate::ScalarRegister::General(1), crate::ScalarWidth::Dword, 0);
                        }
                        cpu.rip = next;
                        StepOutcome::Continue
                    }
                    ExecutionExit::Yield { .. } => StepOutcome::Yield,
                    ExecutionExit::MemoryFault(fault) => StepOutcome::Fault(ExecutionFault::Memory(fault)),
                    ExecutionExit::OperandFault(fault) => StepOutcome::Fault(ExecutionFault::Operand(fault)),
                    ExecutionExit::NonCanonical {
                        instruction,
                        address,
                        access,
                    } => StepOutcome::Fault(ExecutionFault::Memory(MemoryFault {
                        instruction,
                        address,
                        access,
                    })),
                    ExecutionExit::AlignmentFault {
                        instruction,
                        address,
                        access,
                    } => StepOutcome::Fault(ExecutionFault::Alignment {
                        instruction,
                        address,
                        access,
                    }),
                    ExecutionExit::UndefinedInstruction { instruction } => {
                        StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                            signal: TrapSignal::Illegal,
                            code: 2,
                            address: instruction,
                            instruction,
                            state: TrapState::Faulting,
                        }))
                    }
                    ExecutionExit::DivideError { instruction, .. } => {
                        StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                            signal: TrapSignal::Divide,
                            code: 1,
                            address: instruction,
                            instruction,
                            state: TrapState::Faulting,
                        }))
                    }
                    ExecutionExit::Breakpoint { instruction, next } => {
                        cpu.rip = next;
                        StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                            signal: TrapSignal::Breakpoint,
                            code: 128,
                            address: 0,
                            instruction,
                            state: TrapState::Completed { next },
                        }))
                    }
                }
            }
        }
    }

    fn step_aarch64<M: ExecutionInstructionMemory>(
        &self,
        cpu: &mut crate::Aarch64CpuState,
        memory: &mut M,
    ) -> StepOutcome {
        let instruction = cpu.pc;
        // Instruction fetch must demand EXECUTE; an operand read would only demand READ and defeat NX.
        let mut encoded = [0_u8; 4];
        let word = match memory.fetch(instruction, &mut encoded) {
            Ok(4) => u32::from_le_bytes(encoded),
            _ => {
                return StepOutcome::Fault(ExecutionFault::Fetch(MemoryFault {
                    instruction,
                    address: instruction,
                    access: AccessKind::Execute,
                }));
            }
        };
        let mut system = RunnerSystem::new(self.architectural_counter.as_ref());
        let exit = Aarch64Interpreter::execute_runtime(cpu, memory, &mut system, &IdentityCoordinates, word);
        if let Some(address) = system.invalidated {
            memory.invalidate_instruction(address);
        }
        Self::aarch64_exit(cpu, exit)
    }

    fn aarch64_exit(cpu: &mut crate::Aarch64CpuState, exit: Aarch64ExecutionExit) -> StepOutcome {
        match exit {
            Aarch64ExecutionExit::Continue | Aarch64ExecutionExit::Branch { .. } => StepOutcome::Continue,
            Aarch64ExecutionExit::Syscall { instruction, .. } => {
                let next = instruction.wrapping_add(4);
                cpu.pc = next;
                StepOutcome::Syscall { instruction, next }
            }
            Aarch64ExecutionExit::MemoryFault(fault) => StepOutcome::Fault(ExecutionFault::Memory(fault)),
            Aarch64ExecutionExit::OperandFault(fault) => StepOutcome::Fault(ExecutionFault::Operand(fault)),
            Aarch64ExecutionExit::UndefinedInstruction { instruction, .. } => {
                StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                    signal: TrapSignal::Illegal,
                    code: 1,
                    address: instruction,
                    instruction,
                    state: TrapState::Faulting,
                }))
            }
            Aarch64ExecutionExit::UnsupportedInstruction { instruction, .. } => {
                StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                    signal: TrapSignal::Illegal,
                    code: 1,
                    address: instruction,
                    instruction,
                    state: TrapState::Faulting,
                }))
            }
            Aarch64ExecutionExit::Breakpoint { instruction, .. } => {
                StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                    signal: TrapSignal::Breakpoint,
                    code: 1,
                    address: instruction,
                    instruction,
                    state: TrapState::Faulting,
                }))
            }
            Aarch64ExecutionExit::AlignmentFault {
                instruction,
                target,
                access,
            } => StepOutcome::Fault(ExecutionFault::Alignment {
                instruction,
                address: target,
                access,
            }),
        }
    }

    pub fn run_slice<M: ExecutionInstructionMemory>(
        &self,
        expected_epoch: u64,
        budget: u64,
        memory: &mut M,
    ) -> StepOutcome {
        if let Some(outcome) = self.run_x86_slice(expected_epoch, budget, memory) {
            return outcome;
        }
        if let Some(epoch) = memory.instruction_epoch() {
            return self.run_aarch64_blocks(expected_epoch, budget, epoch, memory);
        }
        for _ in 0..budget {
            let outcome = self.run_step(expected_epoch, memory);
            if outcome != StepOutcome::Continue {
                return outcome;
            }
        }
        StepOutcome::Yield
    }

    fn run_x86_slice<M: ExecutionInstructionMemory>(
        &self,
        expected_epoch: u64,
        budget: u64,
        memory: &mut M,
    ) -> Option<StepOutcome> {
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return Some(StepOutcome::Fault(ExecutionFault::Frozen));
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return Some(StepOutcome::Fault(ExecutionFault::Frozen));
        }
        if state.cache_epoch != expected_epoch {
            return Some(StepOutcome::Fault(ExecutionFault::CacheEpoch));
        }
        if !matches!(state.cpu, ExecutionCpuSnapshot::X86_64(_)) {
            return None;
        }
        // Constructed after the ISA test so an aarch64 slice never books an x86 tally.
        let mut tally = InterpreterTally {
            instructions: 0,
            blocks: 0,
        };
        let Some(epoch) = memory.instruction_epoch() else {
            for index in 0..budget {
                let outcome = self.run_locked_step(&mut state.cpu, memory, None);
                if outcome != StepOutcome::Continue {
                    return Some(outcome);
                }
                tally.instructions = index + 1;
            }
            return Some(StepOutcome::Yield);
        };
        let mut cache = self
            .x86_blocks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.synchronize(epoch);
        let mut remaining = budget;
        while remaining > 0 {
            let current = memory.instruction_epoch()?;
            cache.synchronize(current);
            tally.blocks += 1;
            let ExecutionCpuSnapshot::X86_64(cpu) = &state.cpu else {
                unreachable!();
            };
            let address = cpu.rip;
            if cache.get(address).is_none() {
                if let Some(block) = Self::decode_x86_block(address, memory) {
                    cache.insert(address, block);
                } else {
                    let outcome = self.run_locked_step(&mut state.cpu, memory, None);
                    remaining -= 1;
                    tally.instructions = budget - remaining;
                    if outcome != StepOutcome::Continue {
                        return Some(outcome);
                    }
                    continue;
                }
            }
            let instructions = &cache.get(address).expect("inserted block").instructions;
            for &ir in instructions {
                if remaining == 0 {
                    break;
                }
                let ExecutionCpuSnapshot::X86_64(cpu) = &state.cpu else {
                    unreachable!();
                };
                let instruction = cpu.rip;
                let next = instruction.wrapping_add(u64::from(ir.length));
                let outcome = self.run_locked_step(&mut state.cpu, memory, Some(ir));
                remaining -= 1;
                tally.instructions = budget - remaining;
                if outcome != StepOutcome::Continue {
                    return Some(outcome);
                }
                let ExecutionCpuSnapshot::X86_64(cpu) = &state.cpu else {
                    unreachable!();
                };
                if cpu.rip != next {
                    break;
                }
            }
        }
        Some(StepOutcome::Yield)
    }

    /// Classifies the second decode of one instruction, the one made with the full 15-byte window.
    ///
    /// Truncation there is no longer ambiguous: the window is as wide as any instruction can be, so
    /// the missing bytes are absent from guest memory rather than badly encoded.
    fn retry_failure(error: &ScalarIrError) -> X86FetchFailure {
        match error {
            ScalarIrError::Structural(DecodeError::Truncated) => X86FetchFailure::Fetch,
            _ => X86FetchFailure::Decode,
        }
    }

    /// Asks only for the bytes up to the end of the instruction's page, because a mapping ends on a
    /// page boundary and demanding a whole 15-byte window there refuses code the hardware would run.
    /// An instruction that genuinely needs more asks again, and faults if those bytes are absent.
    fn decode_x86_at<M: ExecutionInstructionMemory>(address: u64, memory: &M) -> Result<ScalarIr, X86FetchFailure> {
        let mut bytes = [0_u8; X86_MAXIMUM_INSTRUCTION];
        let read = |window: &mut [u8]| match memory.fetch(address, window) {
            Ok(length) if length > 0 && length <= window.len() => Ok(length),
            _ => Err(X86FetchFailure::Fetch),
        };
        let page = GUEST_PAGE - (address as usize & (GUEST_PAGE - 1));
        let available = page.min(X86_MAXIMUM_INSTRUCTION);
        let length = read(&mut bytes[..available])?;
        match X86ScalarDecoder::decode(&bytes[..length], address) {
            Err(ScalarIrError::Structural(DecodeError::Truncated)) if length < X86_MAXIMUM_INSTRUCTION => {
                let length = read(&mut bytes)?;
                X86ScalarDecoder::decode(&bytes[..length], address).map_err(|error| Self::retry_failure(&error))
            }
            result => result.map_err(|_| X86FetchFailure::Decode),
        }
    }

    fn decode_x86_block<M: ExecutionInstructionMemory>(start: u64, memory: &M) -> Option<X86Block> {
        let mut instructions = Vec::with_capacity(BLOCK_INSTRUCTIONS);
        let mut address = start;
        for _ in 0..BLOCK_INSTRUCTIONS {
            let Ok(ir) = Self::decode_x86_at(address, memory) else {
                break;
            };
            address = address.wrapping_add(u64::from(ir.length));
            instructions.push(ir);
        }
        (!instructions.is_empty()).then_some(X86Block { instructions })
    }

    fn run_aarch64_blocks<M: ExecutionInstructionMemory>(
        &self,
        expected_epoch: u64,
        budget: u64,
        epoch: InstructionEpoch,
        memory: &mut M,
    ) -> StepOutcome {
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return StepOutcome::Fault(ExecutionFault::Frozen);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return StepOutcome::Fault(ExecutionFault::Frozen);
        }
        if state.cache_epoch != expected_epoch {
            return StepOutcome::Fault(ExecutionFault::CacheEpoch);
        }
        let ExecutionCpuSnapshot::Aarch64(cpu) = &mut state.cpu else {
            drop(state);
            for _ in 0..budget {
                let outcome = self.run_step(expected_epoch, memory);
                if outcome != StepOutcome::Continue {
                    return outcome;
                }
            }
            return StepOutcome::Yield;
        };
        let mut cache = self.blocks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.synchronize(epoch);
        let mut remaining = budget;
        let mut tally = InterpreterTally {
            instructions: 0,
            blocks: 0,
        };
        while remaining > 0 {
            let current_epoch = if let Some(current) = memory.instruction_epoch() {
                cache.synchronize(current);
                current
            } else {
                cache.clear();
                let outcome = self.step_aarch64(cpu, memory);
                if outcome != StepOutcome::Continue {
                    return outcome;
                }
                remaining -= 1;
                tally.instructions = budget - remaining;
                continue;
            };
            tally.blocks += 1;
            let address = cpu.pc;
            // `synchronize` above has already established that the cache holds this
            // exact epoch, so a hit needs one lookup and no second identity.
            if !cache.probe(address) {
                match self.prepare_block(&mut cache, address, cpu, memory) {
                    Ok(true) => {
                        remaining -= 1;
                        tally.instructions = budget - remaining;
                        continue;
                    }
                    Ok(false) => {}
                    Err(outcome) => return outcome,
                }
            }
            let block = cache.get(address).expect("inserted block");
            let instructions = &block.instructions;
            let mut stores = block.stores;
            let maximum = instructions.len().min(usize::try_from(remaining).unwrap_or(usize::MAX));
            for ir in &instructions[..maximum] {
                let store = stores & 1 == 1;
                stores >>= 1;
                let exit = if RegisterExecutor::supports(&ir.instruction) {
                    RegisterExecutor::execute(cpu, &IdentityCoordinates, ir).expect("supported register instruction")
                } else {
                    let mut system = RunnerSystem::new(self.architectural_counter.as_ref());
                    let exit =
                        Aarch64Interpreter::execute_runtime_ir(cpu, memory, &mut system, &IdentityCoordinates, ir);
                    if let Some(address) = system.invalidated {
                        memory.invalidate_instruction(address);
                    }
                    exit
                };
                remaining -= 1;
                tally.instructions = budget - remaining;
                match exit {
                    Aarch64ExecutionExit::Continue => {}
                    Aarch64ExecutionExit::Branch { .. } => break,
                    exit => return Self::aarch64_exit(cpu, exit),
                }
                // A committed store into an executable page advances the epoch. Leave the
                // block before its already-decoded tail runs, exactly where terminating the
                // block at the store used to hand the same decision to the outer loop.
                if store && memory.instruction_epoch() != Some(current_epoch) {
                    break;
                }
            }
        }
        StepOutcome::Yield
    }

    fn prepare_block<M: ExecutionInstructionMemory>(
        &self,
        cache: &mut Aarch64BlockCache,
        address: u64,
        cpu: &mut crate::Aarch64CpuState,
        memory: &mut M,
    ) -> Result<bool, StepOutcome> {
        if cache.get(address).is_some() {
            return Ok(false);
        }
        if let Some(block) = Self::decode_block(address, memory) {
            cache.insert(address, block);
            return Ok(false);
        }
        match self.step_aarch64(cpu, memory) {
            StepOutcome::Continue => Ok(true),
            outcome => Err(outcome),
        }
    }

    fn decode_block<M: ExecutionInstructionMemory>(start: u64, memory: &M) -> Option<Aarch64Block> {
        let mut instructions = Vec::with_capacity(BLOCK_INSTRUCTIONS);
        let mut stores = 0_u64;
        let mut address = start;
        for index in 0..BLOCK_INSTRUCTIONS {
            let mut bytes = [0_u8; 4];
            if memory.fetch(address, &mut bytes).ok()? != bytes.len() {
                return None;
            }
            let ir = Aarch64Decoder::decode(u32::from_le_bytes(bytes)).ok()?;
            let terminal = Self::ends_block(ir.instruction);
            if Self::stores_memory(ir.instruction) {
                stores |= 1 << index;
            }
            instructions.push(ir);
            if terminal {
                break;
            }
            address = address.wrapping_add(4);
        }
        (!instructions.is_empty()).then_some(Aarch64Block { instructions, stores })
    }

    fn ends_block(instruction: crate::Aarch64Instruction) -> bool {
        matches!(
            instruction,
            crate::Aarch64Instruction::BranchImmediate { .. }
                | crate::Aarch64Instruction::BranchRegister { .. }
                | crate::Aarch64Instruction::Return { .. }
                | crate::Aarch64Instruction::BranchConditional { .. }
                | crate::Aarch64Instruction::CompareBranch { .. }
                | crate::Aarch64Instruction::TestBranch { .. }
                | crate::Aarch64Instruction::SupervisorCall { .. }
                | crate::Aarch64Instruction::Breakpoint { .. }
                | crate::Aarch64Instruction::Undefined
                | crate::Aarch64Instruction::ExclusiveStore { .. }
                | crate::Aarch64Instruction::AtomicCompareExchange { .. }
                | crate::Aarch64Instruction::AtomicUpdate { .. }
                | crate::Aarch64Instruction::CacheZero { .. }
                | crate::Aarch64Instruction::InstructionCache { .. }
        )
    }

    /// A store whose commit can advance the instruction epoch, and which therefore
    /// needs the epoch rechecked before the block's already-decoded tail runs.
    /// Terminating the block instead is equivalent but three times more expensive:
    /// stores end 39% of blocks on sqlite, against 13% for direct branches.
    fn stores_memory(instruction: crate::Aarch64Instruction) -> bool {
        matches!(
            instruction,
            crate::Aarch64Instruction::Store { .. }
                | crate::Aarch64Instruction::VectorStore { .. }
                | crate::Aarch64Instruction::VectorStorePair { .. }
                | crate::Aarch64Instruction::VectorStoreGroup { .. }
                | crate::Aarch64Instruction::StorePair { .. }
                | crate::Aarch64Instruction::OrderedAccess { load: false, .. }
                | crate::Aarch64Instruction::VectorStructureGroup { load: false, .. }
                | crate::Aarch64Instruction::VectorStructureLane { load: false, .. }
        )
    }
}
