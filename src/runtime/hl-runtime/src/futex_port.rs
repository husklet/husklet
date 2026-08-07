use std::sync::{Arc, Mutex};

use hl_isa::GuestAddress;
use hl_linux::{Errno, FutexOperation, FutexPlan, FutexWaitVector, LinuxResult};
use hl_memory::{AtomicOperation, AtomicOrder, FutexAccess, FutexIdentity, MappingCoordinator, MemoryAccessHost};
use hl_sync::{
    FUTEX_TID_MASK, FutexAtomicOperation, FutexDeadline, FutexError, FutexKey, FutexLimits, FutexMemory, FutexTable,
    FutexWaitMultipleOutcome, FutexWaitOutcome, FutexWaitTarget, Interruption, PiFutexOutcome, PiFutexTable,
};
use hl_time::Clock;

#[path = "futex/keys.rs"]
mod keys;
#[path = "futex/result.rs"]
mod result;

use keys::{Binding, KeyState};

use crate::RuntimeFutexPort;
pub trait FutexInterruptionSource: Send + Sync {
    fn interruption(&self, thread: hl_task::ThreadId) -> Option<Arc<Interruption>>;

    fn identity(&self, _number: u32) -> Option<hl_task::ThreadId> {
        None
    }
}
struct FutexAccessMemory<H: MemoryAccessHost> {
    keys: Mutex<KeyState<H>>,
}
impl<H: MemoryAccessHost> FutexAccessMemory<H> {
    fn same_memory(current: Option<&Arc<MappingCoordinator<H>>>, candidate: &Arc<MappingCoordinator<H>>) -> bool {
        current.is_none_or(|current| Arc::ptr_eq(current, candidate))
    }

    fn resolve(
        &self,
        memory: &Arc<MappingCoordinator<H>>,
        address: u64,
        private: bool,
        access: FutexAccess,
    ) -> Result<FutexKey, FutexError> {
        let address_space = memory.address_space().ok_or(FutexError::Fault)?;
        let address = GuestAddress::new(address);
        let identity = memory
            .ledger()
            .futex_identity(address_space, address, private, access)
            .ok_or(FutexError::Fault)?;
        let mut keys = self.keys.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = match identity {
            private @ FutexIdentity::Private { address, .. } => {
                let owner = match keys.shared.get(&private).copied() {
                    Some(owner) => owner,
                    None => {
                        keys.next_shared = keys.next_shared.wrapping_add(1).max(1);
                        let owner = keys.next_shared;
                        keys.shared.insert(private, owner);
                        owner
                    }
                };
                FutexKey::private(owner, address)?
            }
            shared => {
                let backing = match keys.shared.get(&shared).copied() {
                    Some(backing) => backing,
                    None => {
                        keys.next_shared = keys.next_shared.wrapping_add(1).max(1);
                        let backing = keys.next_shared;
                        keys.shared.insert(shared, backing);
                        backing
                    }
                };
                FutexKey::shared(backing, Self::offset(shared))?
            }
        };
        let binding = Binding {
            memory: Arc::downgrade(memory),
            address,
            private,
            identity,
        };
        let bindings = keys.bindings.entry(key).or_default();
        bindings.retain(|candidate| candidate.memory.strong_count() != 0);
        if !bindings.iter().any(|candidate| candidate.matches(memory, address)) {
            bindings.push(binding);
        }
        Ok(key)
    }

    fn address(
        &self,
        key: FutexKey,
        access: FutexAccess,
    ) -> Result<(Arc<MappingCoordinator<H>>, GuestAddress), FutexError> {
        let keys = self.keys.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let bindings = keys.bindings.get(&key).ok_or(FutexError::Fault)?;
        bindings
            .iter()
            .find_map(|binding| {
                let memory = binding.memory.upgrade()?;
                let current = memory.ledger().futex_identity(
                    memory.address_space()?,
                    binding.address,
                    binding.private,
                    access,
                )?;
                (current == binding.identity).then_some((memory, binding.address))
            })
            .ok_or(FutexError::Fault)
    }

    const fn offset(identity: FutexIdentity) -> u64 {
        match identity {
            FutexIdentity::SharedObject { offset, .. }
            | FutexIdentity::SharedAnonymous { offset, .. }
            | FutexIdentity::SharedFile { offset, .. } => offset,
            FutexIdentity::Private { address, .. } => address,
        }
    }
}
impl<H: MemoryAccessHost> FutexMemory for FutexAccessMemory<H> {
    fn load(&self, key: FutexKey) -> Result<u32, FutexError> {
        let (memory, address) = self.address(key, FutexAccess::Read)?;
        let mut bytes = [0_u8; 4];
        memory
            .read(address, &mut bytes, hl_memory::Protection::READ)
            .map_err(|_| FutexError::Fault)?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn compare_and_apply(
        &self,
        key: FutexKey,
        expected: u32,
        mismatch: FutexError,
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<(), FutexError> {
        let (memory, address) = self.address(key, FutexAccess::Read)?;
        match memory
            .compare_apply_u32(address, expected, apply)
            .map_err(|_| FutexError::Fault)?
        {
            Ok(true) => Ok(()),
            Ok(false) => Err(mismatch),
            Err(error) => Err(error),
        }
    }

    fn compare_apply_many(
        &self,
        targets: &[FutexWaitTarget],
        apply: &mut dyn FnMut() -> Result<(), FutexError>,
    ) -> Result<Option<usize>, FutexError> {
        let mut memory = None;
        let mut comparisons = Vec::with_capacity(targets.len());
        for target in targets {
            let (candidate, address) = self.address(target.key, FutexAccess::Read)?;
            if !Self::same_memory(memory.as_ref(), &candidate) {
                return Err(FutexError::Fault);
            }
            memory = Some(candidate);
            comparisons.push((address, target.expected));
        }
        let memory = memory.ok_or(FutexError::Fault)?;
        let generation = memory.ledger().generation();
        if memory.ledger().generation() != generation {
            return Err(FutexError::Fault);
        }
        match memory
            .compare_apply_many(generation, &comparisons, apply)
            .map_err(|_| FutexError::Fault)?
        {
            Ok(mismatch) => Ok(mismatch),
            Err(error) => Err(error),
        }
    }

    fn atomic_update(&self, key: FutexKey, operation: FutexAtomicOperation) -> Result<i32, FutexError> {
        let (memory, address) = self.address(key, FutexAccess::Write)?;
        let (operation, operand) = match operation {
            FutexAtomicOperation::Set(value) => (AtomicOperation::Swap, value),
            FutexAtomicOperation::Add(value) => (AtomicOperation::Add, value),
            FutexAtomicOperation::Or(value) => (AtomicOperation::Set, value),
            FutexAtomicOperation::AndNot(value) => (AtomicOperation::Clear, value),
            FutexAtomicOperation::Xor(value) => (AtomicOperation::ExclusiveOr, value),
        };
        memory
            .fetch_update(
                address,
                4,
                operation,
                operand as u32 as u64,
                AtomicOrder::AcquireRelease,
            )
            .map(|value| value as u32 as i32)
            .map_err(|_| FutexError::Fault)
    }

    fn compare_exchange(&self, key: FutexKey, expected: u32, replacement: u32) -> Result<u32, FutexError> {
        let (memory, address) = self.address(key, FutexAccess::Write)?;
        memory
            .compare_exchange(
                address,
                4,
                false,
                hl_memory::AtomicValue {
                    low: u64::from(expected),
                    high: 0,
                },
                hl_memory::AtomicValue {
                    low: u64::from(replacement),
                    high: 0,
                },
                AtomicOrder::AcquireRelease,
            )
            .map(|value| value.low as u32)
            .map_err(|_| FutexError::Fault)
    }
}
struct FutexDomain<H: MemoryAccessHost> {
    access: Arc<FutexAccessMemory<H>>,
    table: Arc<FutexTable>,
    pi: Arc<PiFutexTable<hl_task::ThreadId>>,
}

pub struct SafeRuntimeFutex<H: MemoryAccessHost> {
    memory: Arc<MappingCoordinator<H>>,
    domain: Arc<FutexDomain<H>>,
    clock: Arc<dyn Clock + Send + Sync>,
    interruptions: Arc<dyn FutexInterruptionSource>,
}
impl<H: MemoryAccessHost + 'static> SafeRuntimeFutex<H> {
    pub fn new(
        memory: Arc<MappingCoordinator<H>>,
        clock: Arc<dyn Clock + Send + Sync>,
        interruptions: Arc<dyn FutexInterruptionSource>,
        limits: FutexLimits,
    ) -> Result<Self, FutexError> {
        if memory.address_space().is_none() {
            return Err(FutexError::InvalidArgument);
        }
        let access = Arc::new(FutexAccessMemory {
            keys: Mutex::new(KeyState::default()),
        });
        let port: Arc<dyn FutexMemory> = access.clone();
        let table = Arc::new(FutexTable::new(limits, port.clone())?);
        let pi = Arc::new(PiFutexTable::new(port, hl_task::ThreadId::number, limits));
        Ok(Self {
            memory,
            domain: Arc::new(FutexDomain { access, table, pi }),
            clock,
            interruptions,
        })
    }

    /// Creates a fork peer in the same Linux futex namespace.
    ///
    /// Shared mappings retain one key and wait queue across address spaces,
    /// while private keys remain isolated by their address-space identity.
    pub fn fork(
        &self,
        memory: Arc<MappingCoordinator<H>>,
        interruptions: Arc<dyn FutexInterruptionSource>,
    ) -> Result<Self, FutexError> {
        if memory.address_space().is_none() {
            return Err(FutexError::InvalidArgument);
        }
        Ok(Self {
            memory,
            domain: Arc::clone(&self.domain),
            clock: Arc::clone(&self.clock),
            interruptions,
        })
    }

    fn key(&self, address: u64, private: bool, access: FutexAccess) -> Result<FutexKey, FutexError> {
        self.domain.access.resolve(&self.memory, address, private, access)
    }

    fn wake_robust(&self, address: GuestAddress) -> Result<(), ()> {
        let key = self
            .domain
            .access
            .resolve(&self.memory, address.get(), false, FutexAccess::Read)
            .map_err(|_| ())?;
        self.domain.table.wake(key, 1, u32::MAX).map(|_| ()).map_err(|_| ())
    }

    fn execute_result(&self, thread: hl_task::ThreadId, plan: FutexPlan) -> LinuxResult {
        let source_access = match plan.operation {
            FutexOperation::Wake | FutexOperation::WakeBitset | FutexOperation::Requeue => FutexAccess::KeyOnly,
            _ => FutexAccess::Read,
        };
        let source = match self.key(plan.address, plan.private, source_access) {
            Ok(key) => key,
            Err(error) => return LinuxResult::Error(Self::errno(error)),
        };
        if matches!(plan.operation, FutexOperation::WaitBitset | FutexOperation::WakeBitset) && plan.bitset == 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        match plan.operation {
            FutexOperation::Wait | FutexOperation::WaitBitset => {
                let Some(interruption) = self.interruptions.interruption(thread) else {
                    return LinuxResult::Error(Errno::ESRCH);
                };
                let deadline = match self.deadline(plan) {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(Self::errno(error)),
                };
                match self.domain.table.wait(
                    source,
                    plan.value,
                    plan.bitset,
                    deadline,
                    &interruption,
                    self.clock.as_ref(),
                ) {
                    Ok(FutexWaitOutcome::Woken) => LinuxResult::Value(0),
                    Ok(FutexWaitOutcome::Interrupted) => LinuxResult::Error(Errno::EINTR),
                    Ok(FutexWaitOutcome::TimedOut) => LinuxResult::Error(Errno::ETIMEDOUT),
                    Err(error) => LinuxResult::Error(Self::errno(error)),
                }
            }
            FutexOperation::Wake | FutexOperation::WakeBitset => {
                Self::result(self.domain.table.wake(source, plan.value as usize, plan.bitset))
            }
            FutexOperation::Requeue | FutexOperation::CompareRequeue => {
                let target = match self.key(plan.secondary_address, plan.private, FutexAccess::KeyOnly) {
                    Ok(key) => key,
                    Err(error) => return LinuxResult::Error(Self::errno(error)),
                };
                Self::result(self.domain.table.requeue(
                    source,
                    target,
                    plan.value as usize,
                    plan.secondary_count as usize,
                    matches!(plan.operation, FutexOperation::CompareRequeue).then_some(plan.secondary_value),
                ))
            }
            FutexOperation::WakeOperation(operation) => {
                let target = match self.key(plan.secondary_address, plan.private, FutexAccess::Write) {
                    Ok(key) => key,
                    Err(error) => return LinuxResult::Error(Self::errno(error)),
                };
                Self::result(self.domain.table.wake_op(
                    source,
                    target,
                    plan.value as usize,
                    plan.secondary_count as usize,
                    operation,
                    Self::wake_predicate(plan.secondary_value),
                ))
            }
            FutexOperation::LockPriorityInheritance
            | FutexOperation::LockPriorityInheritance2
            | FutexOperation::TryLockPriorityInheritance => {
                let observed = match self.domain.access.load(source) {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(Self::errno(error)),
                };
                let owner_number = observed & FUTEX_TID_MASK;
                let observed_owner = if owner_number == 0 {
                    None
                } else {
                    let Some(owner) = self.interruptions.identity(owner_number) else {
                        return LinuxResult::Error(Errno::ESRCH);
                    };
                    Some(owner)
                };
                let Some(interruption) = self.interruptions.interruption(thread) else {
                    return LinuxResult::Error(Errno::ESRCH);
                };
                let deadline = match self.deadline(plan) {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(Self::errno(error)),
                };
                match self.domain.pi.lock(
                    source,
                    thread,
                    observed_owner,
                    matches!(plan.operation, FutexOperation::TryLockPriorityInheritance),
                    deadline,
                    &interruption,
                    self.clock.as_ref(),
                ) {
                    Ok(PiFutexOutcome::Acquired) => LinuxResult::Value(0),
                    Ok(PiFutexOutcome::Interrupted) => LinuxResult::Error(Errno::EINTR),
                    Ok(PiFutexOutcome::TimedOut) => LinuxResult::Error(Errno::ETIMEDOUT),
                    Err(error) => LinuxResult::Error(Self::pi_errno(error)),
                }
            }
            FutexOperation::UnlockPriorityInheritance => match self.domain.pi.unlock(source, thread) {
                Ok(()) => LinuxResult::Value(0),
                Err(error) => LinuxResult::Error(Self::pi_errno(error)),
            },
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}

impl<H: MemoryAccessHost + 'static> crate::RobustWake for SafeRuntimeFutex<H> {
    fn wake(&self, address: GuestAddress) -> Result<(), ()> {
        self.wake_robust(address)
    }
}

impl<H: MemoryAccessHost + 'static> RuntimeFutexPort for SafeRuntimeFutex<H> {
    fn owner_exit(&self, thread: hl_task::ThreadId) {
        let _ = self.domain.pi.owner_exit(thread);
    }

    fn checkpoint_quiescent(&self) -> bool {
        self.domain.table.snapshot().waits.is_empty() && self.domain.pi.is_quiescent()
    }

    fn execute(&self, _process: hl_task::ProcessId, thread: hl_task::ThreadId, plan: FutexPlan) -> LinuxResult {
        let result = self.execute_result(thread, plan);
        hl_log::hl_debug!(
            hl_log::tag::SYNC,
            "futex thread={} operation={:?} address={:#x} value={} result={:#x}",
            thread.number(),
            plan.operation,
            plan.address,
            plan.value,
            result.encode(),
        );
        result
    }

    fn wait_multiple(
        &self,
        thread: hl_task::ThreadId,
        vectors: &[FutexWaitVector],
        deadline: Option<FutexDeadline>,
    ) -> LinuxResult {
        let Some(interruption) = self.interruptions.interruption(thread) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let targets = vectors
            .iter()
            .map(|vector| {
                self.key(vector.address, vector.private, FutexAccess::Read)
                    .map(|key| FutexWaitTarget {
                        key,
                        expected: vector.value as u32,
                    })
            })
            .collect::<Result<Vec<_>, _>>();
        let targets = match targets {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(Self::errno(error)),
        };
        match self
            .domain
            .table
            .wait_multiple(&targets, deadline, &interruption, self.clock.as_ref())
        {
            Ok(FutexWaitMultipleOutcome::Woken(index)) => LinuxResult::Value(index as u64),
            Ok(FutexWaitMultipleOutcome::Interrupted) => LinuxResult::Error(Errno::EINTR),
            Ok(FutexWaitMultipleOutcome::TimedOut) => LinuxResult::Error(Errno::ETIMEDOUT),
            Err(error) => LinuxResult::Error(Self::errno(error)),
        }
    }
}

impl<H: MemoryAccessHost + 'static> crate::PrivateFutexReset for SafeRuntimeFutex<H> {
    fn reset_private_futexes(&self, _: crate::ForkContext) -> Result<(), ()> {
        self.domain.table.reset_private();
        self.domain.pi.reset_private().map_err(|_| ())?;
        Ok(())
    }
}
