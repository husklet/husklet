use std::collections::BTreeMap;
use std::sync::Mutex;

use hl_isa::AddressRange;
use hl_memory::{Backing, MapRequest, MappingHost, MemoryAccessHost, MemoryError, Placement, WriteReservation};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig};

use super::*;

#[derive(Debug, Default)]
struct Host {
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    next: u64,
    bytes: BTreeMap<u64, u8>,
    writes: BTreeMap<u64, AddressRange>,
    batches: BTreeMap<u64, Vec<AtomicU32Write>>,
}

impl Host {
    fn reserve(state: &mut State) -> u64 {
        state.next += 1;
        state.next
    }
}

impl MappingHost for Host {
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

impl MemoryAccessHost for Host {
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
        let token = Self::reserve(&mut state);
        state.writes.insert(token, range);
        Ok(WriteReservation::new(token, range))
    }

    fn commit_write(&self, token: WriteReservation, input: &[u8]) -> Result<(), MemoryError> {
        let token = token.token;
        let mut state = self.state.lock().unwrap();
        let range = state.writes.remove(&token).ok_or(MemoryError::InvariantViolation)?;
        for (offset, byte) in input.iter().enumerate() {
            state.bytes.insert(range.start().get() + offset as u64, *byte);
        }
        Ok(())
    }

    fn rollback_write(&self, token: WriteReservation) {
        let token = token.token;
        self.state.lock().unwrap().writes.remove(&token);
    }
}

impl AtomicBatchHost for Host {
    fn prepare_u32_batch(&self, writes: &[AtomicU32Write]) -> Result<u64, MemoryError> {
        let mut state = self.state.lock().unwrap();
        let token = Self::reserve(&mut state);
        state.batches.insert(token, writes.to_vec());
        Ok(token)
    }

    fn commit_u32_batch(&self, token: u64) -> Result<(), MemoryError> {
        let mut state = self.state.lock().unwrap();
        let writes = state.batches.get(&token).ok_or(MemoryError::InvariantViolation)?;
        for write in writes {
            let current = u32::from_le_bytes(std::array::from_fn(|offset| {
                state
                    .bytes
                    .get(&(write.address.get() + offset as u64))
                    .copied()
                    .unwrap_or(0)
            }));
            if current != write.expected {
                return Err(MemoryError::InvariantViolation);
            }
        }
        let writes = state.batches.remove(&token).unwrap();
        for write in writes {
            for (offset, byte) in write.replacement.to_le_bytes().into_iter().enumerate() {
                state.bytes.insert(write.address.get() + offset as u64, byte);
            }
        }
        Ok(())
    }

    fn rollback_u32_batch(&self, token: u64) {
        self.state.lock().unwrap().batches.remove(&token);
    }
}

#[derive(Default)]
struct Wake(Mutex<Vec<GuestAddress>>);

impl super::Wake for Wake {
    fn wake(&self, address: GuestAddress) -> Result<(), ()> {
        self.0.lock().unwrap().push(address);
        Ok(())
    }
}

fn put(memory: &MappingCoordinator<Host>, address: u64, bytes: &[u8]) {
    let write = memory
        .prepare_write(GuestAddress::new(address), bytes.len() as u64)
        .unwrap();
    memory.commit_write(write, bytes).unwrap();
}

fn word(memory: &MappingCoordinator<Host>, address: u64) -> u32 {
    let mut bytes = [0; 4];
    memory
        .read(GuestAddress::new(address), &mut bytes, Protection::READ)
        .unwrap();
    u32::from_le_bytes(bytes)
}

#[test]
fn publish_restores_them() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
    let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    tasks
        .set_robust_list(thread, RobustListRegistration::new(0x1000))
        .unwrap();

    let memory = Arc::new(MappingCoordinator::new(Host::default()));
    memory
        .map(MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0x1000)),
            length: 0x1000,
            alignment: 0x1000,
            protection: Protection::READ.union(Protection::WRITE),
            backing: Backing::Anonymous {
                identity: 1,
                shared: false,
            },
            backing_offset: 0,
        })
        .unwrap();
    put(&memory, 0x1000, &0x1100_u64.to_le_bytes());
    put(&memory, 0x1008, &8_i64.to_le_bytes());
    put(&memory, 0x1010, &0_u64.to_le_bytes());
    put(&memory, 0x1100, &0x1000_u64.to_le_bytes());
    let original = thread.number() | FUTEX_WAITERS;
    put(&memory, 0x1108, &original.to_le_bytes());

    let wake = Arc::new(Wake::default());
    let participant = ExitHandler::new(tasks, memory.clone(), wake.clone());
    let mut prepared = participant.prepare(process, &[thread]).unwrap();
    assert_eq!(word(&memory, 0x1108), original);
    prepared.publish().unwrap();
    assert_eq!(word(&memory, 0x1108), FUTEX_WAITERS | FUTEX_OWNER_DIED);
    assert_eq!(*wake.0.lock().unwrap(), [GuestAddress::new(0x1108)]);
    prepared.rollback();
    assert_eq!(word(&memory, 0x1108), original);
}

#[test]
fn pending_nonowner_unchanged() {
    let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
    let credentials = ProcessCredentials::new(1, 1, &[], 4).unwrap();
    let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
    tasks
        .set_robust_list(thread, RobustListRegistration::new(0x2000))
        .unwrap();
    let memory = Arc::new(MappingCoordinator::new(Host::default()));
    memory
        .map(MapRequest {
            placement: Placement::Fixed(GuestAddress::new(0x2000)),
            length: 0x1000,
            alignment: 0x1000,
            protection: Protection::READ.union(Protection::WRITE),
            backing: Backing::Anonymous {
                identity: 2,
                shared: false,
            },
            backing_offset: 0,
        })
        .unwrap();
    put(&memory, 0x2000, &0x2100_u64.to_le_bytes());
    put(&memory, 0x2008, &8_i64.to_le_bytes());
    put(&memory, 0x2010, &0x2100_u64.to_le_bytes());
    put(&memory, 0x2100, &0x2000_u64.to_le_bytes());
    put(&memory, 0x2108, &77_u32.to_le_bytes());
    let wake = Arc::new(Wake::default());
    let participant = ExitHandler::new(tasks, memory.clone(), wake.clone());
    let mut prepared = participant.prepare(process, &[thread]).unwrap();
    prepared.publish().unwrap();
    prepared.finish();
    assert_eq!(word(&memory, 0x2108), 77);
    assert!(wake.0.lock().unwrap().is_empty());
}
