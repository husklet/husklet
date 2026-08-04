use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use hl_isa::{AddressRange, GuestAddress};
use hl_linux::{FutexOperation, FutexPlan, FutexWaitVector, LinuxResult};
use hl_memory::{
    AddressSpaceId, Backing, FileIdentity, MapRequest, MappingCoordinator, MappingHost, MemoryAccessHost, MemoryError,
    Placement, Protection,
};
use hl_sync::{FutexLimits, Interruption};
use hl_task::{ExitStatus, ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
use hl_time::{ClockError, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};

use crate::{FutexInterruptionSource, RuntimeFutexPort, SafeRuntimeFutex};

#[derive(Debug, Default)]
struct Host {
    next: AtomicU64,
    bytes: Mutex<BTreeMap<u64, u8>>,
    writes: Mutex<BTreeMap<u64, AddressRange>>,
}

impl MappingHost for Host {
    fn stage_map(&self, _: GuestAddress, _: MapRequest) -> Result<u64, MemoryError> {
        Ok(self.next.fetch_add(1, Ordering::Relaxed))
    }
    fn stage_unmap(&self, _: AddressRange) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn stage_protect(&self, _: AddressRange, _: Protection) -> Result<u64, MemoryError> {
        Ok(1)
    }
    fn commit(&self, _: &[u64]) -> Result<(), MemoryError> {
        Ok(())
    }
    fn rollback(&self, _: u64) {}
}

impl Host {
    fn mapping(address: u64, backing: Backing) -> MapRequest {
        MapRequest {
            placement: Placement::Fixed(GuestAddress::new(address)),
            length: 4096,
            alignment: 4096,
            protection: Protection::READ.union(Protection::WRITE),
            backing,
            backing_offset: 0,
        }
    }
}

impl MemoryAccessHost for Host {
    type Projection = u64;

    fn read(&self, range: AddressRange, output: &mut [u8]) -> Result<(), MemoryError> {
        let bytes = self.bytes.lock().unwrap();
        for (offset, output) in output.iter_mut().enumerate() {
            *output = bytes.get(&(range.start().get() + offset as u64)).copied().unwrap_or(0);
        }
        Ok(())
    }

    fn prepare_write(&self, range: AddressRange) -> Result<u64, MemoryError> {
        let reservation = self.next.fetch_add(1, Ordering::Relaxed);
        self.writes.lock().unwrap().insert(reservation, range);
        Ok(reservation)
    }

    fn commit_write(&self, reservation: u64, input: &[u8]) -> Result<(), MemoryError> {
        let range = self
            .writes
            .lock()
            .unwrap()
            .remove(&reservation)
            .ok_or(MemoryError::InvariantViolation)?;
        let mut bytes = self.bytes.lock().unwrap();
        for (offset, value) in input.iter().enumerate() {
            bytes.insert(range.start().get() + offset as u64, *value);
        }
        Ok(())
    }

    fn rollback_write(&self, reservation: u64) {
        self.writes.lock().unwrap().remove(&reservation);
    }
}

struct Clock;

struct Interruptions(Mutex<BTreeMap<hl_task::ThreadId, Arc<Interruption>>>);

impl FutexInterruptionSource for Interruptions {
    fn interruption(&self, thread: hl_task::ThreadId) -> Option<Arc<Interruption>> {
        self.0.lock().unwrap().get(&thread).cloned()
    }

    fn identity(&self, number: u32) -> Option<hl_task::ThreadId> {
        self.0
            .lock()
            .unwrap()
            .keys()
            .find(|thread| thread.number() == number)
            .copied()
    }
}

impl MonotonicClock for Clock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(0))
    }
}

impl RealtimeClock for Clock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        Ok(Timespec::ZERO)
    }
}

fn task() -> (hl_task::ProcessId, hl_task::ThreadId, hl_task::ThreadId) {
    let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
    let (process, thread) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 65_536).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let plan = tasks.begin_clone_thread(thread).unwrap();
    let second = tasks.commit_clone_thread(plan).unwrap();
    (process, thread, second)
}

fn plan(operation: FutexOperation, address: u64, value: u32) -> FutexPlan {
    FutexPlan {
        operation,
        address,
        private: true,
        value,
        secondary_address: 0,
        secondary_count: 0,
        secondary_value: 0,
        bitset: u32::MAX,
        deadline: None,
        timeout_absolute: false,
    }
}

#[test]
fn safe_coordinator_memory() {
    let memory = Arc::new(MappingCoordinator::with_address_space(
        Host::default(),
        AddressSpaceId { slot: 4, generation: 9 },
    ));
    memory
        .map(Host::mapping(
            0x1000,
            Backing::Anonymous {
                identity: 1,
                shared: false,
            },
        ))
        .unwrap();
    let write = memory.prepare_write(GuestAddress::new(0x1000), 4).unwrap();
    memory.commit_write(write, &3_u32.to_le_bytes()).unwrap();
    let (process, thread_id, second_thread) = task();
    let first_interrupt = Arc::new(Interruption::new());
    let second_interrupt = Arc::new(Interruption::new());
    let interruptions = Arc::new(Interruptions(Mutex::new(BTreeMap::from([
        (thread_id, first_interrupt.clone()),
        (second_thread, second_interrupt),
    ]))));
    let port = Arc::new(
        SafeRuntimeFutex::new(memory.clone(), Arc::new(Clock), interruptions, FutexLimits::default()).unwrap(),
    );
    let mut empty_mask = plan(FutexOperation::WaitBitset, 0x1000, 3);
    empty_mask.bitset = 0;
    assert_eq!(
        port.execute(process, thread_id, empty_mask),
        LinuxResult::Error(hl_linux::Errno::EINVAL),
    );
    empty_mask.address = 0x2000;
    assert_eq!(
        port.execute(process, thread_id, empty_mask),
        LinuxResult::Error(hl_linux::Errno::EFAULT),
    );
    first_interrupt.interrupt();
    assert_eq!(
        port.execute(process, thread_id, plan(FutexOperation::Wait, 0x1000, 3)),
        LinuxResult::Error(hl_linux::Errno::EINTR),
    );
    let waiter_port = port.clone();
    let waiter =
        thread::spawn(move || waiter_port.execute(process, second_thread, plan(FutexOperation::Wait, 0x1000, 3)));
    let mut woke = false;
    for _ in 0..1_000 {
        let result = port.execute(process, second_thread, plan(FutexOperation::Wake, 0x1000, 1));
        if result == LinuxResult::Value(1) {
            woke = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(woke);
    assert_eq!(waiter.join().unwrap(), LinuxResult::Value(0));

    let shared = Backing::File {
        identity: FileIdentity { device: 7, object: 8 },
        shared: true,
    };
    memory.map(Host::mapping(0x3000, shared)).unwrap();
    memory.map(Host::mapping(0x5000, shared)).unwrap();
    let write = memory.prepare_write(GuestAddress::new(0x3000), 4).unwrap();
    memory.commit_write(write, &4_u32.to_le_bytes()).unwrap();
    let alias_write = memory.prepare_write(GuestAddress::new(0x5000), 4).unwrap();
    memory.commit_write(alias_write, &4_u32.to_le_bytes()).unwrap();
    let mut wait = plan(FutexOperation::Wait, 0x3000, 4);
    wait.private = false;
    let waiter_port = port.clone();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        sender.send(waiter_port.execute(process, second_thread, wait)).unwrap();
    });
    let mut wake = plan(FutexOperation::Wake, 0x5000, 1);
    wake.private = false;
    let mut woke = false;
    for _ in 0..1_000 {
        if let Ok(result) = receiver.try_recv() {
            panic!("shared waiter exited before wake: {result:?}");
        }
        if port.execute(process, second_thread, wake) == LinuxResult::Value(1) {
            woke = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(woke);
    assert_eq!(
        receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
        LinuxResult::Value(0),
    );
}

#[test]
fn safe_space_identity() {
    assert!(matches!(
        SafeRuntimeFutex::new(
            Arc::new(MappingCoordinator::new(Host::default())),
            Arc::new(Clock),
            Arc::new(Interruptions(Mutex::new(BTreeMap::new()))),
            FutexLimits::default(),
        ),
        Err(hl_sync::FutexError::InvalidArgument),
    ));
}

#[test]
fn fork_wake() {
    let shared = Backing::File {
        identity: FileIdentity { device: 7, object: 9 },
        shared: true,
    };
    let parent = Arc::new(MappingCoordinator::with_address_space(
        Host::default(),
        AddressSpaceId { slot: 1, generation: 1 },
    ));
    let child = Arc::new(MappingCoordinator::with_address_space(
        Host::default(),
        AddressSpaceId { slot: 2, generation: 1 },
    ));
    parent.map(Host::mapping(0x1000, shared)).unwrap();
    child.map(Host::mapping(0x2000, shared)).unwrap();
    for (memory, address) in [(&parent, 0x1000), (&child, 0x2000)] {
        let write = memory.prepare_write(GuestAddress::new(address), 4).unwrap();
        memory.commit_write(write, &4_u32.to_le_bytes()).unwrap();
    }
    let (process, waiter_thread, wake_thread) = task();
    let waiter_interrupt = Arc::new(Interruption::new());
    let interruptions = Arc::new(Interruptions(Mutex::new(BTreeMap::from([
        (waiter_thread, Arc::clone(&waiter_interrupt)),
        (wake_thread, Arc::new(Interruption::new())),
    ]))));
    let parent_port = Arc::new(
        SafeRuntimeFutex::new(parent, Arc::new(Clock), interruptions.clone(), FutexLimits::default()).unwrap(),
    );
    let child_port = parent_port.fork(child, interruptions).unwrap();
    let mut wait = plan(FutexOperation::Wait, 0x1000, 4);
    wait.private = false;
    let blocked = {
        let port = Arc::clone(&parent_port);
        thread::spawn(move || port.execute(process, waiter_thread, wait))
    };
    let mut wake = plan(FutexOperation::Wake, 0x2000, 1);
    wake.private = false;
    let mut count = LinuxResult::Value(0);
    for _ in 0..1_000 {
        count = child_port.execute(process, wake_thread, wake);
        if count == LinuxResult::Value(1) {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    if count != LinuxResult::Value(1) {
        waiter_interrupt.interrupt();
    }
    let blocked = blocked.join().unwrap();
    assert_eq!(count, LinuxResult::Value(1));
    assert_eq!(blocked, LinuxResult::Value(0));
}

#[test]
fn safe_exact_identity() {
    let memory = Arc::new(MappingCoordinator::with_address_space(
        Host::default(),
        AddressSpaceId { slot: 8, generation: 3 },
    ));
    memory
        .map(Host::mapping(
            0x1000,
            Backing::Anonymous {
                identity: 9,
                shared: false,
            },
        ))
        .unwrap();
    let (process, owner, contender) = task();
    let interruptions = Arc::new(Interruptions(Mutex::new(BTreeMap::from([
        (owner, Arc::new(Interruption::new())),
        (contender, Arc::new(Interruption::new())),
    ]))));
    let port = SafeRuntimeFutex::new(memory.clone(), Arc::new(Clock), interruptions, FutexLimits::default()).unwrap();
    assert_eq!(
        port.execute(process, owner, plan(FutexOperation::LockPriorityInheritance, 0x1000, 0)),
        LinuxResult::Value(0),
    );
    assert_eq!(
        port.execute(
            process,
            contender,
            plan(FutexOperation::TryLockPriorityInheritance, 0x1000, 0),
        ),
        LinuxResult::Error(hl_linux::Errno::EAGAIN),
    );
    assert_eq!(
        port.execute(
            process,
            contender,
            plan(FutexOperation::UnlockPriorityInheritance, 0x1000, 0),
        ),
        LinuxResult::Error(hl_linux::Errno::EPERM),
    );
    port.owner_exit(owner);
    assert_eq!(
        port.execute(
            process,
            contender,
            plan(FutexOperation::LockPriorityInheritance2, 0x1000, 0)
        ),
        LinuxResult::Value(0),
    );
    assert_eq!(
        port.execute(
            process,
            contender,
            plan(FutexOperation::UnlockPriorityInheritance, 0x1000, 0),
        ),
        LinuxResult::Value(0),
    );
}

#[test]
fn safe_other_keys() {
    let memory = Arc::new(MappingCoordinator::with_address_space(
        Host::default(),
        AddressSpaceId { slot: 5, generation: 2 },
    ));
    for (address, value) in [(0x1000, 3_u32), (0x2000, 5_u32)] {
        memory
            .map(Host::mapping(
                address,
                Backing::Anonymous {
                    identity: address,
                    shared: false,
                },
            ))
            .unwrap();
        let write = memory.prepare_write(GuestAddress::new(address), 4).unwrap();
        memory.commit_write(write, &value.to_le_bytes()).unwrap();
    }
    let (process, thread_id, _) = task();
    let interruptions = Arc::new(Interruptions(Mutex::new(BTreeMap::from([(
        thread_id,
        Arc::new(Interruption::new()),
    )]))));
    let port = Arc::new(SafeRuntimeFutex::new(memory, Arc::new(Clock), interruptions, FutexLimits::default()).unwrap());
    let waiter_port = port.clone();
    let waiter = thread::spawn(move || {
        waiter_port.wait_multiple(
            thread_id,
            &[
                FutexWaitVector {
                    value: 3,
                    address: 0x1000,
                    private: true,
                },
                FutexWaitVector {
                    value: 5,
                    address: 0x2000,
                    private: true,
                },
            ],
            None,
        )
    });
    let mut woke = false;
    for _ in 0..1_000 {
        if port.execute(process, thread_id, plan(FutexOperation::Wake, 0x2000, 1)) == LinuxResult::Value(1) {
            woke = true;
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert!(woke);
    assert_eq!(waiter.join().unwrap(), LinuxResult::Value(1));
    assert_eq!(
        port.execute(process, thread_id, plan(FutexOperation::Wake, 0x1000, 1)),
        LinuxResult::Value(0),
    );
}

#[test]
fn interruption_slot_generation() {
    let tasks = TaskRegistry::new(RegistryConfig {
        // Reserve slot zero for the process leader and force the only
        // nonleader slot to be reused after exit. The production allocator
        // deliberately prefers unused slots while any remain.
        max_threads: 2,
        ..RegistryConfig::default()
    })
    .unwrap();
    let (_, leader) = tasks
        .create_init(
            ProcessCredentials::new(0, 0, &[], 65_536).unwrap(),
            ProcessLimits::default(),
        )
        .unwrap();
    let old = tasks
        .commit_clone_thread(tasks.begin_clone_thread(leader).unwrap())
        .unwrap();
    let token = Arc::new(Interruption::new());
    let source = Interruptions(Mutex::new(BTreeMap::from([(old, token)])));
    tasks.exit_thread(old, ExitStatus::Code(0)).unwrap();
    let replacement = tasks
        .commit_clone_thread(tasks.begin_clone_thread(leader).unwrap())
        .unwrap();
    assert_eq!(old.number(), replacement.number());
    assert_ne!(old, replacement);
    assert!(source.interruption(old).is_some());
    assert!(source.interruption(replacement).is_none());
}
