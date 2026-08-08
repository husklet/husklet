use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use hl_isa::GuestArchitecture;

use crate::{Aarch64CpuState, CpuState, MemoryFault};

pub const EXECUTION_SNAPSHOT_VERSION: u32 = 6;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CpuSnapshot {
    Aarch64(Aarch64CpuState),
    X86_64(CpuState),
}
pub type ExecutionCpuSnapshot = CpuSnapshot;

/// The one frequency `cntfrq_el0` reports to the guest, on every execution path.
/// It is 1GHz because [`ArchitecturalCounter`] ticks are nanoseconds.
pub const GUEST_COUNTER_FREQUENCY_HZ: u64 = 1_000_000_000;

/// Supplies the host monotonic timeline projected as the guest architectural
/// counter. Values are nanoseconds and must never decrease.
pub trait ArchitecturalCounter: std::fmt::Debug + Send + Sync {
    fn read(&self) -> u64;
}

#[derive(Debug, Default)]
struct ProcessCounter {
    origin: OnceLock<Instant>,
}

impl ArchitecturalCounter for ProcessCounter {
    fn read(&self) -> u64 {
        let elapsed = self.origin.get_or_init(Instant::now).elapsed().as_nanos();
        u64::try_from(elapsed).unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub version: u32,
    pub cpu: ExecutionCpuSnapshot,
    pub cache_epoch: u64,
    pub fault: Option<MemoryFault>,
}
pub type ExecutionSnapshot = Snapshot;

impl ExecutionSnapshot {
    pub fn encode(&self) -> Result<Vec<u8>, ExecutionStateError> {
        super::codec::SnapshotCodec::encode(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, ExecutionStateError> {
        super::codec::SnapshotCodec::decode(bytes)
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
        self.validate()?;
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
pub struct Machine {
    pub(super) state: Mutex<ExecutionSnapshot>,
    pub(super) frozen: AtomicBool,
    pub(super) timestamp_counter: AtomicU64,
    pub(super) architectural_counter: Arc<dyn ArchitecturalCounter>,
    pub(super) blocks: Mutex<super::runner::Aarch64BlockCache>,
    pub(super) x86_blocks: Mutex<super::runner::X86BlockCache>,
}
pub type ExecutionMachine = Machine;

impl ExecutionMachine {
    pub fn new(snapshot: ExecutionSnapshot) -> Result<Self, ExecutionStateError> {
        Self::new_with_counter(snapshot, Arc::new(ProcessCounter::default()))
    }

    pub fn new_with_counter(
        snapshot: ExecutionSnapshot,
        architectural_counter: Arc<dyn ArchitecturalCounter>,
    ) -> Result<Self, ExecutionStateError> {
        snapshot.validate()?;
        Ok(Self {
            state: Mutex::new(snapshot),
            frozen: AtomicBool::new(false),
            timestamp_counter: AtomicU64::new(0),
            architectural_counter,
            blocks: Mutex::new(super::runner::Aarch64BlockCache::default()),
            x86_blocks: Mutex::new(super::runner::X86BlockCache::default()),
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
        self.blocks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        if state.architecture() != replacement.architecture() {
            return Err(ExecutionStateError::Architecture);
        }
        Ok(std::mem::replace(&mut *state, replacement))
    }

    /// Swaps the architectural state without discarding translated blocks. Signal
    /// delivery and `rt_sigreturn` change registers and the program counter only, so
    /// the instructions the cache holds are still the instructions the guest will run.
    /// A replacement that carries a different cache epoch is a new image, not a new
    /// register file, and is refused rather than silently kept stale.
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
        let child = Self::new_with_counter(self.snapshot()?.fork_child()?, Arc::clone(&self.architectural_counter))?;
        child
            .timestamp_counter
            .store(self.timestamp_counter.load(Ordering::Relaxed), Ordering::Relaxed);
        Ok(child)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateError {
    InvalidSnapshot,
    ResourceLimit,
    Architecture,
    Frozen,
    NotFrozen,
}
pub type ExecutionStateError = StateError;
