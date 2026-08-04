use std::collections::BTreeSet;
use std::sync::Arc;

use hl_isa::GuestAddress;
use hl_memory::{AtomicBatchHost, AtomicU32Write, MappingCoordinator, Protection, SharedAtomicBatch};
use hl_sync::{FUTEX_OWNER_DIED, FUTEX_TID_MASK, FUTEX_WAITERS};
use hl_task::{ProcessId, RobustListRegistration, TaskRegistry, ThreadId};

use crate::{ExitParticipant, ExitRuntimeError, PreparedExitParticipant};

const LIST_LIMIT: usize = 2_048;

pub trait Wake: Send + Sync {
    fn wake(&self, address: GuestAddress) -> Result<(), ()>;
}

pub struct ExitHandler<H: AtomicBatchHost> {
    tasks: Arc<TaskRegistry>,
    memory: Arc<MappingCoordinator<H>>,
    wake: Arc<dyn Wake>,
}

struct PreparedExit<H: AtomicBatchHost> {
    memory: Arc<MappingCoordinator<H>>,
    forward: Option<SharedAtomicBatch<H>>,
    reverse: Option<SharedAtomicBatch<H>>,
    addresses: Vec<GuestAddress>,
    wake: Arc<dyn Wake>,
    published: bool,
}

impl<H: AtomicBatchHost> ExitHandler<H> {
    pub fn new(tasks: Arc<TaskRegistry>, memory: Arc<MappingCoordinator<H>>, wake: Arc<dyn Wake>) -> Self {
        Self { tasks, memory, wake }
    }

    fn read_u64(&self, address: u64) -> Result<u64, ()> {
        let mut bytes = [0_u8; 8];
        self.memory
            .read(GuestAddress::new(address), &mut bytes, Protection::READ)
            .map_err(|_| ())?;
        Ok(u64::from_le_bytes(bytes))
    }

    fn read_u32(&self, address: GuestAddress) -> Result<u32, ()> {
        let mut bytes = [0_u8; 4];
        self.memory
            .read(address, &mut bytes, Protection::READ)
            .map_err(|_| ())?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn futex_address(node: u64, offset: i64) -> Result<GuestAddress, ()> {
        let value = if offset < 0 {
            node.checked_sub(offset.unsigned_abs())
        } else {
            node.checked_add(offset as u64)
        }
        .ok_or(())?;
        (value % 4 == 0).then_some(GuestAddress::new(value)).ok_or(())
    }

    fn append_owned(
        &self,
        node: u64,
        offset: i64,
        thread: ThreadId,
        seen: &mut BTreeSet<GuestAddress>,
        writes: &mut Vec<AtomicU32Write>,
    ) -> Result<(), ()> {
        let address = Self::futex_address(node & !1, offset)?;
        if !seen.insert(address) {
            return Ok(());
        }
        let observed = self.read_u32(address)?;
        if observed & FUTEX_TID_MASK == thread.number() {
            writes.push(AtomicU32Write {
                address,
                expected: observed,
                replacement: (observed & FUTEX_WAITERS) | FUTEX_OWNER_DIED,
            });
        }
        Ok(())
    }

    fn collect(
        &self,
        thread: ThreadId,
        registration: RobustListRegistration,
        seen: &mut BTreeSet<GuestAddress>,
        writes: &mut Vec<AtomicU32Write>,
    ) {
        let head = registration.head;
        let Ok(offset) = self.read_u64(head.checked_add(8).unwrap_or(u64::MAX)) else {
            return;
        };
        let offset = offset as i64;
        let pending = self.read_u64(head.checked_add(16).unwrap_or(u64::MAX)).unwrap_or(0);
        let mut node = self.read_u64(head).unwrap_or(head);
        for _ in 0..LIST_LIMIT {
            if node == head {
                break;
            }
            let current = node;
            if self.append_owned(current, offset, thread, seen, writes).is_err() {
                break;
            }
            let Ok(next) = self.read_u64(current & !1) else {
                break;
            };
            node = next;
        }
        if pending != 0 {
            match self.append_owned(pending, offset, thread, seen, writes) {
                Ok(()) | Err(()) => {}
            }
        }
    }
}

impl<H: AtomicBatchHost + 'static> ExitParticipant for ExitHandler<H> {
    fn prepare(
        &self,
        process: ProcessId,
        threads: &[ThreadId],
    ) -> Result<Box<dyn PreparedExitParticipant>, ExitRuntimeError> {
        let snapshot = self.tasks.snapshot();
        let mut seen = BTreeSet::new();
        let mut writes = Vec::new();
        for thread in threads {
            let registration = snapshot
                .threads
                .iter()
                .find(|candidate| candidate.process == process && candidate.id == *thread)
                .and_then(|candidate| candidate.robust_list);
            if let Some(registration) = registration {
                self.collect(*thread, registration, &mut seen, &mut writes);
            }
        }
        if writes.is_empty() {
            return Ok(Box::new(PreparedExit {
                memory: Arc::clone(&self.memory),
                forward: None,
                reverse: None,
                addresses: Vec::new(),
                wake: Arc::clone(&self.wake),
                published: false,
            }));
        }
        let reverse = writes
            .iter()
            .map(|write| AtomicU32Write {
                address: write.address,
                expected: write.replacement,
                replacement: write.expected,
            })
            .collect::<Vec<_>>();
        let forward = self
            .memory
            .prepare_shared_batch(&writes)
            .map_err(|_| ExitRuntimeError::Failed)?;
        let reverse = self
            .memory
            .prepare_shared_batch(&reverse)
            .map_err(|_| ExitRuntimeError::Failed)?;
        Ok(Box::new(PreparedExit {
            memory: Arc::clone(&self.memory),
            forward: Some(forward),
            reverse: Some(reverse),
            addresses: writes.iter().map(|write| write.address).collect(),
            wake: Arc::clone(&self.wake),
            published: false,
        }))
    }
}

impl<H: AtomicBatchHost> hl_task::RobustExitCleanup for ExitHandler<H> {
    type Error = ();

    fn cleanup(&self, _: ProcessId, thread: ThreadId, registration: RobustListRegistration) -> Result<(), Self::Error> {
        let mut seen = BTreeSet::new();
        let mut writes = Vec::new();
        self.collect(thread, registration, &mut seen, &mut writes);
        if writes.is_empty() {
            return Ok(());
        }
        let addresses = writes.iter().map(|write| write.address).collect::<Vec<_>>();
        let prepared = self.memory.prepare_shared_batch(&writes).map_err(|_| ())?;
        self.memory.commit_shared_batch(prepared).map_err(|_| ())?;
        for address in addresses {
            self.wake.wake(address)?;
        }
        Ok(())
    }
}

impl<H: AtomicBatchHost> PreparedExitParticipant for PreparedExit<H> {
    fn publish(&mut self) -> Result<(), ExitRuntimeError> {
        let Some(forward) = self.forward.take() else {
            self.published = true;
            return Ok(());
        };
        self.memory
            .commit_shared_batch(forward)
            .map_err(|_| ExitRuntimeError::Failed)?;
        self.published = true;
        for address in &self.addresses {
            self.wake.wake(*address).map_err(|_| ExitRuntimeError::Failed)?;
        }
        Ok(())
    }

    fn rollback(&mut self) {
        if !self.published {
            return;
        }
        if let Some(reverse) = self.reverse.take() {
            let _ = self.memory.commit_shared_batch(reverse);
        }
        self.published = false;
    }

    fn finish(&mut self) {
        self.reverse.take();
        self.published = false;
    }
}

#[cfg(test)]
#[path = "exit_test.rs"]
mod tests;
