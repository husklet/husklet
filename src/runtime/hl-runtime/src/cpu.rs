//! Architectural register state shared with the retained C execution boundary.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use hl_isa::GuestArchitecture;

pub const EXECUTION_SNAPSHOT_VERSION: u32 = 6;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessKind {
    Read,
    Write,
    Execute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryFault {
    pub instruction: u64,
    pub address: u64,
    pub access: AccessKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappingGeneration(u64);

impl MappingGeneration {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExclusiveReservation {
    address: u64,
    element_bytes: u8,
    pair: bool,
    generation: MappingGeneration,
}

impl ExclusiveReservation {
    #[must_use]
    pub const fn new(address: u64, element_bytes: u8, pair: bool, generation: MappingGeneration) -> Self {
        Self {
            address,
            element_bytes,
            pair,
            generation,
        }
    }

    #[must_use]
    pub const fn address(self) -> u64 {
        self.address
    }

    #[must_use]
    pub const fn element_bytes(self) -> u8 {
        self.element_bytes
    }

    #[must_use]
    pub const fn pair(self) -> bool {
        self.pair
    }

    #[must_use]
    pub const fn generation(self) -> MappingGeneration {
        self.generation
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Nzcv(u32);

impl Nzcv {
    const MASK: u32 = 0xf000_0000;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits & Self::MASK)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Aarch64CpuState {
    pub registers: [u64; 31],
    pub vectors: [u128; 32],
    pub sp: u64,
    pub pc: u64,
    pub nzcv: Nzcv,
    pub tls: u64,
    pub fpcr: u64,
    pub fpsr: u64,
    pub exclusive: Option<ExclusiveReservation>,
}

impl Aarch64CpuState {
    pub fn clear_exclusive_reservation(&mut self) {
        self.exclusive = None;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FlagState(u16);

impl FlagState {
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtendedClass {
    Empty,
    Zero,
    Denormal,
    Normal,
    Infinity,
    QuietNan,
    SignalingNan,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtendedReal(u128);

impl ExtendedReal {
    #[must_use]
    pub const fn from_bits(bits: u128) -> Self {
        Self(bits & ((1_u128 << 80) - 1))
    }

    #[must_use]
    pub const fn bits(self) -> u128 {
        self.0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScalarState {
    pub registers: [u64; 16],
    pub flags: FlagState,
    pub rip: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub direction: bool,
    pub alignment_check: bool,
    pub id_flag: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CpuState {
    pub scalar: ScalarState,
    pub vectors: [u128; 16],
    pub vector_upper: [u128; 16],
    pub x87_control: u16,
    pub x87_status: u16,
    pub x87_values: [ExtendedReal; 8],
    pub x87_classes: [ExtendedClass; 8],
    pub mxcsr: u32,
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            scalar: ScalarState::default(),
            vectors: [0; 16],
            vector_upper: [0; 16],
            x87_control: 0x037f,
            x87_status: 0,
            x87_values: [ExtendedReal::from_bits(0); 8],
            x87_classes: [ExtendedClass::Empty; 8],
            mxcsr: 0x1f80,
        }
    }
}

impl core::ops::Deref for CpuState {
    type Target = ScalarState;

    fn deref(&self) -> &Self::Target {
        &self.scalar
    }
}

impl core::ops::DerefMut for CpuState {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.scalar
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionCpuSnapshot {
    Aarch64(Aarch64CpuState),
    X86_64(CpuState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionSnapshot {
    pub version: u32,
    pub cpu: ExecutionCpuSnapshot,
    pub cache_epoch: u64,
    pub fault: Option<MemoryFault>,
}

impl ExecutionSnapshot {
    pub fn encode(&self) -> Result<Vec<u8>, ExecutionStateError> {
        SnapshotCodec::encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ExecutionStateError> {
        SnapshotCodec::decode(bytes)
    }

    pub fn validate(&self) -> Result<(), ExecutionStateError> {
        if self.version != EXECUTION_SNAPSHOT_VERSION || self.cache_epoch == 0 {
            return Err(ExecutionStateError::InvalidSnapshot);
        }
        Ok(())
    }

    #[must_use]
    pub const fn architecture(&self) -> GuestArchitecture {
        match self.cpu {
            ExecutionCpuSnapshot::Aarch64(_) => GuestArchitecture::Aarch64,
            ExecutionCpuSnapshot::X86_64(_) => GuestArchitecture::X86_64,
        }
    }

    pub fn fork_child(&self) -> Result<Self, ExecutionStateError> {
        let mut child = self.fork_parent()?;
        child.fault = None;
        Ok(child)
    }

    pub fn fork_parent(&self) -> Result<Self, ExecutionStateError> {
        self.validate()?;
        let mut parent = self.clone();
        if let ExecutionCpuSnapshot::Aarch64(cpu) = &mut parent.cpu {
            cpu.clear_exclusive_reservation();
        }
        parent.cache_epoch = parent
            .cache_epoch
            .checked_add(1)
            .ok_or(ExecutionStateError::ResourceLimit)?;
        Ok(parent)
    }
}

#[derive(Debug)]
pub struct ExecutionMachine {
    state: Mutex<ExecutionSnapshot>,
    frozen: AtomicBool,
}

impl ExecutionMachine {
    pub fn new(snapshot: ExecutionSnapshot) -> Result<Self, ExecutionStateError> {
        snapshot.validate()?;
        Ok(Self {
            state: Mutex::new(snapshot),
            frozen: AtomicBool::new(false),
        })
    }

    pub fn freeze(&self) -> Result<(), ExecutionStateError> {
        self.frozen
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| ExecutionStateError::Frozen)
    }

    pub fn thaw(&self) -> Result<(), ExecutionStateError> {
        self.frozen
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| ExecutionStateError::NotFrozen)
    }

    pub fn snapshot(&self) -> Result<ExecutionSnapshot, ExecutionStateError> {
        if !self.frozen.load(Ordering::Acquire) {
            return Err(ExecutionStateError::NotFrozen);
        }
        Ok(self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    pub fn replace(&self, replacement: ExecutionSnapshot) -> Result<ExecutionSnapshot, ExecutionStateError> {
        replacement.validate()?;
        if !self.frozen.load(Ordering::Acquire) {
            return Err(ExecutionStateError::NotFrozen);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.architecture() != replacement.architecture() {
            return Err(ExecutionStateError::Architecture);
        }
        Ok(std::mem::replace(&mut *state, replacement))
    }

    pub fn replace_context(&self, replacement: ExecutionSnapshot) -> Result<ExecutionSnapshot, ExecutionStateError> {
        replacement.validate()?;
        if !self.frozen.load(Ordering::Acquire) {
            return Err(ExecutionStateError::NotFrozen);
        }
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.architecture() != replacement.architecture() {
            return Err(ExecutionStateError::Architecture);
        }
        if state.cache_epoch != replacement.cache_epoch {
            return Err(ExecutionStateError::InvalidSnapshot);
        }
        Ok(std::mem::replace(&mut *state, replacement))
    }

    pub fn fork_child(&self) -> Result<Self, ExecutionStateError> {
        Self::new(self.snapshot()?.fork_child()?)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionStateError {
    InvalidSnapshot,
    ResourceLimit,
    Architecture,
    Frozen,
    NotFrozen,
}

#[path = "cpu_codec.rs"]
mod codec;
use codec::SnapshotCodec;

pub const TRACE_REGISTER_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoppedRegisterImage {
    version: u32,
    registers: StoppedRegisters,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoppedRegisters {
    X86(X86Prstatus),
    Aarch64(Aarch64Prstatus),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct X86Prstatus {
    words: [u64; 27],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Aarch64Prstatus {
    words: [u64; 34],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TraceRegisterError {
    Length,
    Architecture,
    Version,
}

pub trait TraceSafepointPort: Send + Sync {
    fn publish(&self, image: StoppedRegisterImage) -> Result<(), TraceRegisterError>;
    fn restore(&self) -> Result<StoppedRegisterImage, TraceRegisterError>;
}

impl StoppedRegisterImage {
    #[must_use]
    pub const fn new(registers: StoppedRegisters) -> Self {
        Self {
            version: TRACE_REGISTER_VERSION,
            registers,
        }
    }

    pub fn restore(self) -> Result<StoppedRegisters, TraceRegisterError> {
        if self.version != TRACE_REGISTER_VERSION {
            return Err(TraceRegisterError::Version);
        }
        Ok(self.registers)
    }

    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    #[must_use]
    pub const fn registers(&self) -> &StoppedRegisters {
        &self.registers
    }
}

impl X86Prstatus {
    pub const BYTES: usize = 27 * 8;

    #[must_use]
    pub fn capture(cpu: &CpuState, original_syscall: u64) -> Self {
        let r = &cpu.registers;
        let mut words = [0; 27];
        words[..16].copy_from_slice(&[
            r[15],
            r[14],
            r[13],
            r[12],
            r[5],
            r[3],
            r[11],
            r[10],
            r[9],
            r[8],
            r[0],
            r[1],
            r[2],
            r[6],
            r[7],
            original_syscall,
        ]);
        words[16] = cpu.rip;
        words[17] = 0x33;
        words[18] = u64::from(cpu.flags.bits()) | 2;
        words[19] = r[4];
        words[20] = 0x2b;
        words[21] = cpu.fs_base;
        words[22] = cpu.gs_base;
        Self { words }
    }

    pub fn apply(&self, cpu: &mut CpuState) {
        let g = &self.words;
        let r = &mut cpu.registers;
        r[15] = g[0];
        r[14] = g[1];
        r[13] = g[2];
        r[12] = g[3];
        r[5] = g[4];
        r[3] = g[5];
        r[11] = g[6];
        r[10] = g[7];
        r[9] = g[8];
        r[8] = g[9];
        r[0] = g[10];
        r[1] = g[11];
        r[2] = g[12];
        r[6] = g[13];
        r[7] = g[14];
        r[4] = g[19];
        cpu.rip = g[16];
        cpu.flags = FlagState::from_bits(g[18] as u16);
        cpu.fs_base = g[21];
        cpu.gs_base = g[22];
    }

    #[must_use]
    pub const fn words(&self) -> &[u64; 27] {
        &self.words
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, TraceRegisterError> {
        if bytes.len() != Self::BYTES {
            return Err(TraceRegisterError::Length);
        }
        let mut words = [0; 27];
        for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
            *word = u64::from_le_bytes(chunk.try_into().map_err(|_| TraceRegisterError::Length)?);
        }
        Ok(Self { words })
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }
}

impl Aarch64Prstatus {
    pub const BYTES: usize = 34 * 8;

    #[must_use]
    pub fn capture(cpu: &Aarch64CpuState) -> Self {
        let mut words = [0; 34];
        words[..31].copy_from_slice(&cpu.registers);
        words[31] = cpu.sp;
        words[32] = cpu.pc;
        words[33] = u64::from(cpu.nzcv.bits());
        Self { words }
    }

    pub fn apply(&self, cpu: &mut Aarch64CpuState) {
        cpu.registers.copy_from_slice(&self.words[..31]);
        cpu.sp = self.words[31];
        cpu.pc = self.words[32];
        cpu.nzcv = Nzcv::from_bits(self.words[33] as u32);
    }

    #[must_use]
    pub const fn words(&self) -> &[u64; 34] {
        &self.words
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, TraceRegisterError> {
        if bytes.len() != Self::BYTES {
            return Err(TraceRegisterError::Length);
        }
        let mut words = [0; 34];
        for (word, chunk) in words.iter_mut().zip(bytes.chunks_exact(8)) {
            *word = u64::from_le_bytes(chunk.try_into().map_err(|_| TraceRegisterError::Length)?);
        }
        Ok(Self { words })
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.words.iter().flat_map(|word| word.to_le_bytes()).collect()
    }
}
