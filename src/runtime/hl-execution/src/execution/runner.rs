use crate::{
    Aarch64Decoder, Aarch64ExecutionExit, Aarch64Interpreter, Aarch64Ir, AccessKind, BlockIdentity, CacheObservation,
    DispatchDecision, ExclusiveMemory, ExecutionCpuSnapshot, ExecutionExit, ExecutionMachine, GuestOperandMemory,
    GuestSystemPort, MemoryFault, PcCoordinatePort, ScalarInterpreter, ScalarIr, X86ScalarDecoder,
    aarch64::register::RegisterExecutor,
};
const X86_MAXIMUM_INSTRUCTION: usize = 15;
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
}
#[derive(Debug)]
pub(super) struct Aarch64BlockCache {
    epoch: Option<InstructionEpoch>,
    generation: u64,
    blocks: Vec<Option<(u64, Aarch64Block)>>,
}
impl Default for Aarch64BlockCache {
    fn default() -> Self {
        Self {
            epoch: None,
            generation: 1,
            blocks: vec![None; BLOCK_LIMIT],
        }
    }
}
impl Aarch64BlockCache {
    pub(super) fn clear(&mut self) {
        self.epoch = None;
        self.blocks.fill(None);
    }

    fn synchronize(&mut self, epoch: InstructionEpoch) {
        if self.epoch != Some(epoch) {
            self.clear();
            self.epoch = Some(epoch);
            self.generation = self.generation.wrapping_add(1).max(1);
        }
    }

    fn invalidate_line(&mut self, address: u64, epoch: InstructionEpoch) {
        let line_start = address & !63;
        let line_end = line_start.saturating_add(64);
        for slot in &mut self.blocks {
            let Some((start, block)) = slot else { continue };
            let end = start.saturating_add(
                u64::try_from(block.instructions.len())
                    .unwrap_or(u64::MAX)
                    .saturating_mul(4),
            );
            if *start < line_end && end > line_start {
                *slot = None;
            }
        }
        self.epoch = Some(epoch);
        self.generation = self.generation.wrapping_add(1).max(1);
    }

    fn insert(&mut self, address: u64, block: Aarch64Block) {
        let index = Self::index(address);
        self.blocks[index] = Some((address, block));
    }

    fn get(&self, address: u64) -> Option<&Aarch64Block> {
        let (stored, block) = self.blocks[Self::index(address)].as_ref()?;
        (*stored == address).then_some(block)
    }

    fn observe(&self, address: u64, epoch: InstructionEpoch) -> CacheObservation {
        if self.epoch != Some(epoch) {
            return CacheObservation::MappingEpochMismatch;
        }
        let Some(block) = self.get(address) else {
            return CacheObservation::Missing;
        };
        let bytes = u64::try_from(block.instructions.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(4);
        CacheObservation::Available(BlockIdentity::new(bytes, self.generation).expect("nonempty cached block"))
    }

    fn index(address: u64) -> usize {
        (address as usize >> 2) & (BLOCK_LIMIT - 1)
    }
}

#[cfg(test)]
mod cache_test {
    use super::*;

    #[test]
    fn instruction_flush_is_terminal_and_discards_only_its_line() {
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
                },
            );
        }

        cache.invalidate_line(0x103f, published);

        assert!(cache.get(0x1000).is_none());
        assert!(matches!(
            cache.observe(0x2000, published),
            CacheObservation::Available(_)
        ));
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
#[derive(Default)]
struct RunnerSystem {
    invalidated: Option<u64>,
    counter: u64,
}

impl GuestSystemPort for RunnerSystem {
    fn barrier(&mut self, _kind: crate::BarrierKind, _option: u8) {}
    fn counter_frequency(&self) -> u64 {
        1_000_000_000
    }
    fn counter_value(&self) -> u64 {
        self.counter
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
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
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
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
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
                let decoded = match retained {
                    Some(decoded) => decoded,
                    None => {
                        let mut bytes = [0_u8; X86_MAXIMUM_INSTRUCTION];
                        let length = match memory.fetch(instruction, &mut bytes) {
                            Ok(length) if length > 0 && length <= bytes.len() => length,
                            _ => {
                                return StepOutcome::Fault(ExecutionFault::Fetch(MemoryFault {
                                    instruction,
                                    address: instruction,
                                    access: AccessKind::Execute,
                                }));
                            }
                        };
                        match X86ScalarDecoder::decode(&bytes[..length], instruction) {
                            Ok(decoded) => decoded,
                            Err(error) => {
                                eprintln!(
                                    "x86-decode-frontier pc={instruction:#x} error={error:?} bytes={:02x?}",
                                    &bytes[..length.min(15)]
                                );
                                return StepOutcome::Fault(ExecutionFault::Signal(SynchronousTrap {
                                    signal: TrapSignal::Illegal,
                                    code: 2,
                                    address: instruction,
                                    instruction,
                                    state: TrapState::Faulting,
                                }));
                            }
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
        let word = match memory.read(instruction, 4) {
            Ok(value) => value as u32,
            Err(()) => {
                return StepOutcome::Fault(ExecutionFault::Fetch(MemoryFault {
                    instruction,
                    address: instruction,
                    access: AccessKind::Execute,
                }));
            }
        };
        let mut system = RunnerSystem {
            counter: self.architectural_counter.read(),
            ..RunnerSystem::default()
        };
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
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if self.frozen.load(std::sync::atomic::Ordering::Acquire) {
            return Some(StepOutcome::Fault(ExecutionFault::Frozen));
        }
        if state.cache_epoch != expected_epoch {
            return Some(StepOutcome::Fault(ExecutionFault::CacheEpoch));
        }
        if !matches!(state.cpu, ExecutionCpuSnapshot::X86_64(_)) {
            return None;
        }
        let Some(epoch) = memory.instruction_epoch() else {
            for _ in 0..budget {
                let outcome = self.run_locked_step(&mut state.cpu, memory, None);
                if outcome != StepOutcome::Continue {
                    return Some(outcome);
                }
            }
            return Some(StepOutcome::Yield);
        };
        let mut cache = self.x86_blocks.lock().unwrap_or_else(|error| error.into_inner());
        cache.synchronize(epoch);
        let mut remaining = budget;
        while remaining > 0 {
            let current = memory.instruction_epoch()?;
            cache.synchronize(current);
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

    fn decode_x86_block<M: ExecutionInstructionMemory>(start: u64, memory: &M) -> Option<X86Block> {
        let mut instructions = Vec::with_capacity(BLOCK_INSTRUCTIONS);
        let mut address = start;
        for _ in 0..BLOCK_INSTRUCTIONS {
            let mut bytes = [0_u8; X86_MAXIMUM_INSTRUCTION];
            let length = memory.fetch(address, &mut bytes).ok()?;
            if length == 0 || length > bytes.len() {
                break;
            }
            let ir = X86ScalarDecoder::decode(&bytes[..length], address).ok()?;
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
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
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
        let mut cache = self.blocks.lock().unwrap_or_else(|error| error.into_inner());
        cache.synchronize(epoch);
        let mut remaining = budget;
        while remaining > 0 {
            let current_epoch = match memory.instruction_epoch() {
                Some(current) => {
                    cache.synchronize(current);
                    current
                }
                None => {
                    cache.clear();
                    let outcome = self.step_aarch64(cpu, memory);
                    if outcome != StepOutcome::Continue {
                        return outcome;
                    }
                    remaining -= 1;
                    continue;
                }
            };
            let address = cpu.pc;
            match DispatchDecision::from(cache.observe(address, current_epoch)) {
                DispatchDecision::Translate => match self.prepare_block(&mut cache, address, cpu, memory) {
                    Ok(true) => {
                        remaining -= 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(outcome) => return outcome,
                },
                DispatchDecision::Enter(_) => {}
                DispatchDecision::RetryMappingEpoch => return StepOutcome::Fault(ExecutionFault::CacheEpoch),
            }
            let instructions = &cache.get(address).expect("inserted block").instructions;
            let maximum = instructions.len().min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let mut invalidated = None;
            for &ir in &instructions[..maximum] {
                let exit = if RegisterExecutor::supports(ir.instruction) {
                    RegisterExecutor::execute(cpu, &IdentityCoordinates, ir).expect("supported register instruction")
                } else {
                    let mut system = RunnerSystem {
                        counter: self.architectural_counter.read(),
                        ..RunnerSystem::default()
                    };
                    let exit =
                        Aarch64Interpreter::execute_runtime_ir(cpu, memory, &mut system, &IdentityCoordinates, ir);
                    if let Some(address) = system.invalidated {
                        memory.invalidate_instruction(address);
                        invalidated = Some(address);
                    }
                    exit
                };
                remaining -= 1;
                match exit {
                    Aarch64ExecutionExit::Continue => {}
                    Aarch64ExecutionExit::Branch { .. } => break,
                    exit => return Self::aarch64_exit(cpu, exit),
                }
            }
            if let Some(address) = invalidated
                && let Some(epoch) = memory.instruction_epoch()
            {
                cache.invalidate_line(address, epoch);
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
        let mut address = start;
        for _ in 0..BLOCK_INSTRUCTIONS {
            let mut bytes = [0_u8; 4];
            if memory.fetch(address, &mut bytes).ok()? != bytes.len() {
                return None;
            }
            let ir = Aarch64Decoder::decode(u32::from_le_bytes(bytes)).ok()?;
            let terminal = Self::ends_block(ir.instruction);
            instructions.push(ir);
            if terminal {
                break;
            }
            address = address.wrapping_add(4);
        }
        (!instructions.is_empty()).then_some(Aarch64Block { instructions })
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
                | crate::Aarch64Instruction::Store { .. }
                | crate::Aarch64Instruction::VectorStore { .. }
                | crate::Aarch64Instruction::VectorStorePair { .. }
                | crate::Aarch64Instruction::VectorStoreGroup { .. }
                | crate::Aarch64Instruction::StorePair { .. }
                | crate::Aarch64Instruction::ExclusiveStore { .. }
                | crate::Aarch64Instruction::AtomicCompareExchange { .. }
                | crate::Aarch64Instruction::AtomicUpdate { .. }
                | crate::Aarch64Instruction::CacheZero { .. }
                | crate::Aarch64Instruction::InstructionCache { .. }
        ) || matches!(
            instruction,
            crate::Aarch64Instruction::OrderedAccess { load: false, .. }
                | crate::Aarch64Instruction::VectorStructureGroup { load: false, .. }
                | crate::Aarch64Instruction::VectorStructureLane { load: false, .. }
        )
    }
}
