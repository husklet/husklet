use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use hl_isa::GuestAddress;
use hl_memory::{ATOMIC_U32_WRITE_BATCH_MAXIMUM, AtomicU32Write, MappingCoordinator, Protection, SharedAtomicBatch};
use hl_runtime::{PreparedExecParticipant, RobustWake, RuntimeExecError};
use hl_sync::{FUTEX_OWNER_DIED, FUTEX_TID_MASK, FUTEX_WAITERS};
use hl_task::{ProcessId, RobustListRegistration, TaskRegistry, ThreadId};

use super::super::MappingHostAdapter;

const LIST_LIMIT: usize = 2_048;

/// Reversible old-image cleanup staged before task identity publication.
///
/// Futex words are changed atomically during `publish`, but wakeups are held
/// until `finish`. A later participant can therefore fail and restore every
/// word without exposing a wake from an exec that did not happen.
pub(super) struct RetireImage {
    memory: Arc<MappingCoordinator<MappingHostAdapter>>,
    wake: Arc<dyn RobustWake>,
    forward: Vec<SharedAtomicBatch<MappingHostAdapter>>,
    reverse: Vec<Option<SharedAtomicBatch<MappingHostAdapter>>>,
    addresses: Vec<GuestAddress>,
    committed: usize,
}

impl RetireImage {
    pub(super) fn prepare(
        tasks: &TaskRegistry,
        process: ProcessId,
        caller: ThreadId,
        memory: Arc<MappingCoordinator<MappingHostAdapter>>,
        wake: Arc<dyn RobustWake>,
    ) -> Result<Self, RuntimeExecError> {
        let obligations = tasks
            .snapshot()
            .threads
            .into_iter()
            .filter(|thread| thread.process == process && thread.id != caller)
            .collect::<Vec<_>>();
        let mut words = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for thread in &obligations {
            if let Some(registration) = thread.robust_list {
                Self::collect(&memory, thread.id, registration, &mut seen, &mut words);
            }
        }
        for thread in obligations {
            let Some(address) = thread.clear_tid else { continue };
            let address = GuestAddress::new(address);
            if address.get() % 4 != 0 || words.contains_key(&address) {
                continue;
            }
            if let Ok(value) = Self::read_u32(&memory, address) {
                words.insert(address, (value, 0));
            }
        }
        let writes = words
            .iter()
            .filter_map(|(address, (before, after))| {
                (before != after).then_some(AtomicU32Write {
                    address: *address,
                    expected: *before,
                    replacement: *after,
                })
            })
            .collect::<Vec<_>>();
        if writes.is_empty() {
            return Ok(Self {
                memory,
                wake,
                forward: Vec::new(),
                reverse: Vec::new(),
                addresses: Vec::new(),
                committed: 0,
            });
        }
        let reverse = writes
            .iter()
            .map(|write| AtomicU32Write {
                address: write.address,
                expected: write.replacement,
                replacement: write.expected,
            })
            .collect::<Vec<_>>();
        let forward = writes
            .chunks(ATOMIC_U32_WRITE_BATCH_MAXIMUM)
            .map(|chunk| memory.prepare_shared_batch(chunk))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RuntimeExecError::Failed)?;
        let reverse = reverse
            .chunks(ATOMIC_U32_WRITE_BATCH_MAXIMUM)
            .map(|chunk| memory.prepare_shared_batch(chunk).map(Some))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| RuntimeExecError::Failed)?;
        Ok(Self {
            memory,
            wake,
            forward,
            reverse,
            addresses: writes.iter().map(|write| write.address).collect(),
            committed: 0,
        })
    }

    fn collect(
        memory: &Arc<MappingCoordinator<MappingHostAdapter>>,
        thread: ThreadId,
        registration: RobustListRegistration,
        seen: &mut BTreeSet<GuestAddress>,
        words: &mut BTreeMap<GuestAddress, (u32, u32)>,
    ) {
        let head = registration.head;
        let Some(offset_address) = head.checked_add(8) else {
            return;
        };
        let Ok(offset) = Self::read_u64(memory, offset_address) else {
            return;
        };
        let offset = offset as i64;
        let pending = head
            .checked_add(16)
            .and_then(|address| Self::read_u64(memory, address).ok())
            .unwrap_or(0);
        let mut node = Self::read_u64(memory, head).unwrap_or(head);
        for _ in 0..LIST_LIMIT {
            if node == head {
                break;
            }
            let current = node;
            if !Self::append(memory, current, offset, thread, seen, words) {
                break;
            }
            let Ok(next) = Self::read_u64(memory, current & !1) else {
                break;
            };
            node = next;
        }
        if pending != 0 {
            Self::append(memory, pending, offset, thread, seen, words);
        }
    }

    fn append(
        memory: &Arc<MappingCoordinator<MappingHostAdapter>>,
        node: u64,
        offset: i64,
        thread: ThreadId,
        seen: &mut BTreeSet<GuestAddress>,
        words: &mut BTreeMap<GuestAddress, (u32, u32)>,
    ) -> bool {
        let address = if offset < 0 {
            (node & !1).checked_sub(offset.unsigned_abs())
        } else {
            (node & !1).checked_add(offset as u64)
        };
        let Some(address) = address else { return false };
        if address % 4 != 0 {
            return false;
        }
        let address = GuestAddress::new(address);
        if !seen.insert(address) {
            return true;
        }
        let Ok(value) = Self::read_u32(memory, address) else {
            return false;
        };
        if value & FUTEX_TID_MASK == thread.number() {
            words.insert(address, (value, (value & FUTEX_WAITERS) | FUTEX_OWNER_DIED));
        }
        true
    }

    fn read_u64(memory: &MappingCoordinator<MappingHostAdapter>, address: u64) -> Result<u64, ()> {
        let mut bytes = [0; 8];
        memory
            .read(GuestAddress::new(address), &mut bytes, Protection::READ)
            .map_err(|_| ())?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn read_u32(memory: &MappingCoordinator<MappingHostAdapter>, address: GuestAddress) -> Result<u32, ()> {
        let mut bytes = [0; 4];
        memory.read(address, &mut bytes, Protection::READ).map_err(|_| ())?;
        Ok(u32::from_le_bytes(bytes))
    }
}

impl PreparedExecParticipant for RetireImage {
    fn publish(&mut self) -> Result<(), RuntimeExecError> {
        if self.committed != 0 {
            return Err(RuntimeExecError::Failed);
        }
        let forward = std::mem::take(&mut self.forward);
        for batch in forward {
            self.memory
                .commit_shared_batch(batch)
                .map_err(|_| RuntimeExecError::Failed)?;
            self.committed += 1;
        }
        Ok(())
    }

    fn rollback(&mut self) {
        while self.committed != 0 {
            self.committed -= 1;
            if let Some(reverse) = self.reverse[self.committed].take() {
                let _ = self.memory.commit_shared_batch(reverse);
            }
        }
    }

    fn finish(&mut self) {
        if self.committed != 0 {
            for address in self.addresses.drain(..) {
                let _ = self.wake.wake(address);
            }
        }
        self.reverse.clear();
        self.committed = 0;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hl_memory::{Backing, MapRequest, Placement, SharedLimits, SharedObjectStore};
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig};

    use super::*;
    use crate::ffi::linux::VirtualMemory;

    struct Wake(AtomicUsize);

    impl RobustWake for Wake {
        fn wake(&self, _: GuestAddress) -> Result<(), ()> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
    }

    struct Fixture {
        arena: Arc<VirtualMemory>,
        memory: Arc<MappingCoordinator<MappingHostAdapter>>,
        tasks: Arc<TaskRegistry>,
        process: ProcessId,
        caller: ThreadId,
        sibling: ThreadId,
        wake: Arc<Wake>,
    }

    impl Fixture {
        fn new() -> Self {
            let arena = Arc::new(VirtualMemory::reserve(4096).unwrap());
            let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
            let memory = Arc::new(MappingCoordinator::with_shared_space(
                MappingHostAdapter::new(Arc::clone(&arena)),
                shared,
                hl_memory::AddressSpaceId { slot: 1, generation: 1 },
            ));
            memory
                .map(MapRequest {
                    placement: Placement::Fixed(GuestAddress::ZERO),
                    length: 4096,
                    alignment: 4096,
                    protection: Protection::READ.union(Protection::WRITE),
                    backing: Backing::Anonymous {
                        identity: 1,
                        shared: false,
                    },
                    backing_offset: 0,
                })
                .unwrap();
            let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
            let (process, caller) = tasks
                .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::empty())
                .unwrap();
            let sibling = tasks
                .commit_clone_thread(tasks.begin_clone_thread(caller).unwrap())
                .unwrap();
            let wake = Arc::new(Wake(AtomicUsize::new(0)));
            Self {
                arena,
                memory,
                tasks,
                process,
                caller,
                sibling,
                wake,
            }
        }

        fn write_u64(&self, address: u64, value: u64) {
            self.arena.write(address, &value.to_le_bytes()).unwrap();
        }

        fn write_u32(&self, address: u64, value: u32) {
            self.arena.write(address, &value.to_le_bytes()).unwrap();
        }

        fn read_u32(&self, address: u64) -> u32 {
            let mut bytes = [0; 4];
            self.arena.read(address, &mut bytes).unwrap();
            u32::from_le_bytes(bytes)
        }

        fn obligations(&self) -> (u64, u64, u32) {
            let head = 0x100;
            let node = 0x180;
            let robust = node + 8;
            let clear = 0x200;
            let owner = self.sibling.number() | FUTEX_WAITERS;
            self.write_u64(head, node);
            self.write_u64(head + 8, 8);
            self.write_u64(head + 16, 0);
            self.write_u64(node, head);
            self.write_u32(robust, owner);
            self.write_u32(clear, self.sibling.number());
            self.tasks
                .set_robust_list(self.sibling, RobustListRegistration::new(head))
                .unwrap();
            self.tasks.set_clear_tid(self.sibling, clear).unwrap();
            (robust, clear, owner)
        }

        fn prepare(&self) -> RetireImage {
            RetireImage::prepare(
                &self.tasks,
                self.process,
                self.caller,
                Arc::clone(&self.memory),
                self.wake.clone(),
            )
            .unwrap()
        }
    }

    #[test]
    fn rollback_restores() {
        let fixture = Fixture::new();
        let (robust, clear, owner) = fixture.obligations();
        let mut prepared = fixture.prepare();
        prepared.publish().unwrap();
        assert_eq!(fixture.read_u32(robust), FUTEX_WAITERS | FUTEX_OWNER_DIED);
        assert_eq!(fixture.read_u32(clear), 0);
        assert_eq!(fixture.wake.0.load(Ordering::Relaxed), 0);
        prepared.rollback();
        assert_eq!(fixture.read_u32(robust), owner);
        assert_eq!(fixture.read_u32(clear), fixture.sibling.number());
        assert_eq!(fixture.wake.0.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn finish_wakes_once() {
        let fixture = Fixture::new();
        fixture.obligations();
        let mut prepared = fixture.prepare();
        prepared.publish().unwrap();
        prepared.finish();
        prepared.finish();
        assert_eq!(fixture.wake.0.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn malformed_list_continues() {
        let fixture = Fixture::new();
        fixture
            .tasks
            .set_robust_list(fixture.sibling, RobustListRegistration::new(0x8_0000))
            .unwrap();
        fixture.tasks.set_clear_tid(fixture.sibling, 0x200).unwrap();
        fixture.write_u32(0x200, fixture.sibling.number());
        let mut prepared = fixture.prepare();
        prepared.publish().unwrap();
        prepared.finish();
        assert_eq!(fixture.read_u32(0x200), 0);
        assert_eq!(fixture.wake.0.load(Ordering::Relaxed), 1);
    }
}
