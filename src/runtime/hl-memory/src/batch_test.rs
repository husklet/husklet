use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::WriteReservation;
use hl_isa::{AddressRange, GuestAddress};

use crate::{
    AddressSpaceId, AtomicBatchHost, AtomicU32Write, Backing, MapRequest, MappingCoordinator, MappingHost,
    MemoryAccessHost, MemoryError, Placement, Protection, SharedBackingRef, SharedLimits, SharedObjectStore,
};
use std::sync::Arc;

#[derive(Debug, Default)]
struct BatchHost {
    state: Mutex<BatchState>,
}

#[derive(Debug, Default)]
struct BatchState {
    next: u64,
    bytes: BTreeMap<u64, u8>,
    writes: BTreeMap<u64, AddressRange>,
    batches: BTreeMap<u64, Vec<AtomicU32Write>>,
    failure_index: Option<usize>,
}

impl BatchHost {
    fn reserve(state: &mut BatchState) -> u64 {
        state.next += 1;
        state.next
    }
}

impl MappingHost for BatchHost {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        Ok(Self::reserve(&mut self.state.lock().unwrap()))
    }

    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        Ok(Self::reserve(&mut self.state.lock().unwrap()))
    }

    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        Ok(Self::reserve(&mut self.state.lock().unwrap()))
    }

    fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
        Ok(())
    }

    fn rollback(&self, _: u64) {}
}

impl MemoryAccessHost for BatchHost {
    type Projection = u64;

    fn read(&self, range: AddressRange, output: &mut [u8], _: Protection) -> Result<(), MemoryError> {
        let state = self.state.lock().unwrap();
        for (offset, byte) in output.iter_mut().enumerate() {
            *byte = state
                .bytes
                .get(&(range.start().get() + offset as u64))
                .copied()
                .unwrap_or(0);
        }
        Ok(())
    }

    fn prepare_write(&self, range: AddressRange) -> Result<WriteReservation, MemoryError> {
        let mut state = self.state.lock().unwrap();
        let reservation = Self::reserve(&mut state);
        state.writes.insert(reservation, range);
        Ok(WriteReservation::new(reservation, range))
    }

    fn commit_write(&self, reservation: WriteReservation, input: &[u8]) -> Result<(), MemoryError> {
        let reservation = reservation.token;
        let mut state = self.state.lock().unwrap();
        let range = state
            .writes
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        if range.length() != input.len() as u64 {
            return Err(MemoryError::InvariantViolation);
        }
        for (offset, byte) in input.iter().enumerate() {
            state.bytes.insert(range.start().get() + offset as u64, *byte);
        }
        Ok(())
    }

    fn rollback_write(&self, reservation: WriteReservation) {
        let reservation = reservation.token;
        self.state.lock().unwrap().writes.remove(&reservation);
    }
}

impl AtomicBatchHost for BatchHost {
    fn prepare_u32_batch(&self, writes: &[AtomicU32Write]) -> Result<u64, MemoryError> {
        let mut state = self.state.lock().unwrap();
        let reservation = Self::reserve(&mut state);
        state.batches.insert(reservation, writes.to_vec());
        Ok(reservation)
    }

    fn commit_u32_batch(&self, reservation: u64) -> Result<(), MemoryError> {
        let mut state = self.state.lock().unwrap();
        let writes = state.batches.get(&reservation).ok_or(MemoryError::InvariantViolation)?;
        for (index, write) in writes.iter().enumerate() {
            let current = u32::from_le_bytes(std::array::from_fn(|offset| {
                state
                    .bytes
                    .get(&(write.address.get() + offset as u64))
                    .copied()
                    .unwrap_or(0)
            }));
            if state.failure_index == Some(index) || current != write.expected {
                return Err(MemoryError::InvariantViolation);
            }
        }
        let writes = state.batches.remove(&reservation).unwrap();
        for write in writes {
            for (offset, byte) in write.replacement.to_le_bytes().into_iter().enumerate() {
                state.bytes.insert(write.address.get() + offset as u64, byte);
            }
        }
        Ok(())
    }

    fn rollback_u32_batch(&self, reservation: u64) {
        self.state.lock().unwrap().batches.remove(&reservation);
    }
}

fn coordinator() -> MappingCoordinator<BatchHost> {
    let coordinator = MappingCoordinator::new(BatchHost::default());
    coordinator
        .map(MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0x1000)),
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
    coordinator
}

#[test]
fn later_failure_publishes() {
    let coordinator = coordinator();
    let initial = coordinator.prepare_write(GuestAddress::new(0x1000), 8).unwrap();
    coordinator.commit_write(initial, &[1, 0, 0, 0, 2, 0, 0, 0]).unwrap();
    coordinator.host.state.lock().unwrap().failure_index = Some(1);
    let prepared = coordinator
        .prepare_u32_batch(&[
            AtomicU32Write {
                address: GuestAddress::new(0x1000),
                expected: 1,
                replacement: 10,
            },
            AtomicU32Write {
                address: GuestAddress::new(0x1004),
                expected: 2,
                replacement: 20,
            },
        ])
        .unwrap();
    assert_eq!(
        coordinator.commit_u32_batch(prepared),
        Err(MemoryError::InvariantViolation),
    );
    let mut observed = [0; 8];
    coordinator
        .read(GuestAddress::new(0x1000), &mut observed, Protection::READ)
        .unwrap();
    assert_eq!(observed, [1, 0, 0, 0, 2, 0, 0, 0]);
    assert!(coordinator.host.state.lock().unwrap().batches.is_empty());
}

#[test]
fn success_publishes_all() {
    let coordinator = coordinator();
    let prepared = coordinator
        .prepare_u32_batch(&[
            AtomicU32Write {
                address: GuestAddress::new(0x1000),
                expected: 0,
                replacement: 10,
            },
            AtomicU32Write {
                address: GuestAddress::new(0x1004),
                expected: 0,
                replacement: 20,
            },
        ])
        .unwrap();
    assert_eq!(coordinator.commit_u32_batch(prepared).unwrap(), 1);
    let mut observed = [0; 8];
    coordinator
        .read(GuestAddress::new(0x1000), &mut observed, Protection::READ)
        .unwrap();
    assert_eq!(observed, [10, 0, 0, 0, 20, 0, 0, 0]);
}

#[test]
fn peer_batch() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = store.create(1, 4096).unwrap();
    let mapping = |address| MapRequest {
        placement: Placement::Fixed(GuestAddress::new(address)),
        length: 4096,
        alignment: 4096,
        protection: Protection::READ.union(Protection::WRITE),
        backing: Backing::Shared(SharedBackingRef {
            object,
            offset: 0,
            length: 4096,
            write_shared: true,
        }),
        backing_offset: 0,
    };
    let first = Arc::new(MappingCoordinator::with_shared_space(
        BatchHost::default(),
        Arc::clone(&store),
        AddressSpaceId { slot: 1, generation: 1 },
    ));
    let second =
        MappingCoordinator::with_shared_space(BatchHost::default(), store, AddressSpaceId { slot: 2, generation: 1 });
    first.map(mapping(0x1000)).unwrap();
    second.map(mapping(0x2000)).unwrap();
    let prepared = first
        .prepare_shared_batch(&[
            AtomicU32Write {
                address: GuestAddress::new(0x1000),
                expected: 0,
                replacement: 10,
            },
            AtomicU32Write {
                address: GuestAddress::new(0x1004),
                expected: 0,
                replacement: 20,
            },
        ])
        .unwrap();
    first.commit_shared_batch(prepared).unwrap();
    let mut observed = [0; 8];
    second
        .read(GuestAddress::new(0x2000), &mut observed, Protection::READ)
        .unwrap();
    assert_eq!(observed, [10, 0, 0, 0, 20, 0, 0, 0]);
}
