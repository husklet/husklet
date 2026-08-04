use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use hl_descriptor::{DescriptorFlags, DescriptorTable, ObjectError, OpenFileDescription, Readiness, StatusFlags};

use crate::{
    SIGNALFD_RECORD_SIZE, SignalFd, SignalFdError, SignalFdFlags, SignalFdSnapshot, SignalInfo, SignalMask,
    SignalObserver, SignalQueue, SignalQueueError, SignalSubscription,
};

struct TestRegistration {
    active: Arc<AtomicBool>,
}

impl SignalSubscription for TestRegistration {
    fn quiesce(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

struct ObserverEntry {
    active: Arc<AtomicBool>,
    observer: Arc<dyn SignalObserver>,
}

#[derive(Default)]
struct TestQueue {
    pending: Mutex<VecDeque<SignalInfo>>,
    observers: Mutex<Vec<ObserverEntry>>,
}

impl std::fmt::Debug for TestQueue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TestQueue")
            .field(
                "pending",
                &self.pending.lock().unwrap_or_else(|error| error.into_inner()).len(),
            )
            .finish_non_exhaustive()
    }
}

impl TestQueue {
    fn push(&self, info: SignalInfo) {
        {
            let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
            let insertion = pending
                .iter()
                .position(|queued| queued.signal > info.signal)
                .unwrap_or(pending.len());
            pending.insert(insertion, info);
        }
        let observers = self.observers.lock().unwrap_or_else(|error| error.into_inner());
        for entry in observers.iter() {
            if entry.active.load(Ordering::SeqCst) {
                entry.observer.signal_available();
            }
        }
    }

    fn push_standard(&self, info: SignalInfo) {
        let duplicate = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .any(|queued| queued.signal == info.signal);
        if !duplicate {
            self.push(info);
        }
    }

    fn len(&self) -> usize {
        self.pending.lock().unwrap_or_else(|error| error.into_inner()).len()
    }

    fn active_subscriptions(&self) -> usize {
        self.observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|entry| entry.active.load(Ordering::SeqCst))
            .count()
    }
}

impl SignalQueue for TestQueue {
    fn dequeue(&self, mask: SignalMask) -> Result<Option<SignalInfo>, SignalQueueError> {
        let mut pending = self.pending.lock().unwrap_or_else(|error| error.into_inner());
        let Some(index) = pending.iter().position(|info| mask.contains(info.signal)) else {
            return Ok(None);
        };
        Ok(pending.remove(index))
    }

    fn has_pending(&self, mask: SignalMask) -> bool {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .any(|info| mask.contains(info.signal))
    }

    fn subscribe(&self, observer: Arc<dyn SignalObserver>) -> Result<Box<dyn SignalSubscription>, SignalQueueError> {
        let active = Arc::new(AtomicBool::new(true));
        self.observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(ObserverEntry {
                active: Arc::clone(&active),
                observer,
            });
        Ok(Box::new(TestRegistration { active }))
    }
}

struct SignalFixture;

impl SignalFixture {
    fn mask(signals: &[u32]) -> SignalMask {
        let bits = signals
            .iter()
            .fold(0_u64, |bits, signal| bits | (1_u64 << (signal - 1)));
        SignalMask::from_bits(bits)
    }

    fn info(signal: u32, value: i32) -> SignalInfo {
        SignalInfo {
            signal,
            integer: value,
            pointer: u64::from(value.unsigned_abs()),
            process_id: 123,
            user_id: 456,
            ..SignalInfo::default()
        }
    }

    fn nonblocking() -> SignalFdFlags {
        SignalFdFlags::from_bits(SignalFdFlags::NONBLOCKING)
    }

    fn read_i32(record: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(record[offset..offset + 4].try_into().unwrap())
    }

    fn read_u32(record: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(record[offset..offset + 4].try_into().unwrap())
    }
}

#[test]
fn flags_validated_local() {
    let queue = Arc::new(TestQueue::default());
    assert_eq!(
        SignalFd::new(SignalFixture::mask(&[10]), SignalFdFlags::from_bits(4), queue.clone(),).unwrap_err(),
        SignalFdError::InvalidArgument
    );
    let flags = SignalFdFlags::from_bits(SignalFdFlags::NONBLOCKING | SignalFdFlags::CLOSE_ON_EXEC);
    assert!(flags.closes_on_exec());
    assert!(SignalFd::new(SignalFixture::mask(&[10]), flags, queue).is_ok());
}

#[test]
fn mask_uses_stop() {
    let selected = SignalFixture::mask(&[1, 9, 10, 19, 64]);
    assert!(selected.contains(1));
    assert!(!selected.contains(9));
    assert!(selected.contains(10));
    assert!(!selected.contains(19));
    assert!(selected.contains(64));
    assert!(!selected.contains(0));
}

#[test]
fn short_read_signal() {
    let queue = Arc::new(TestQueue::default());
    queue.push(SignalFixture::info(10, 7));
    let fd = SignalFd::new(SignalFixture::mask(&[10]), SignalFixture::nonblocking(), queue.clone()).unwrap();
    let mut short = [0_u8; SIGNALFD_RECORD_SIZE - 1];
    assert_eq!(fd.read(&mut short), Err(SignalFdError::InvalidArgument));
    assert_eq!(queue.len(), 1);
    let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
    assert_eq!(fd.read(&mut record), Ok(SIGNALFD_RECORD_SIZE));
    assert_eq!(SignalFixture::read_u32(&record, 0), 10);
}

#[test]
fn empty_nonblocking_block() {
    let queue = Arc::new(TestQueue::default());
    let fd = SignalFd::new(SignalFixture::mask(&[10]), SignalFixture::nonblocking(), queue).unwrap();
    let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
    assert_eq!(fd.read(&mut record), Err(SignalFdError::WouldBlock));
}

#[test]
fn record_encoding_layout() {
    let record = SignalInfo {
        signal: 35,
        error: -2,
        code: -1,
        process_id: 10,
        user_id: 20,
        file_descriptor: 30,
        thread_id: 40,
        band: 50,
        overrun: 60,
        trap_number: 70,
        status: -80,
        integer: -90,
        pointer: 0x1122_3344_5566_7788,
        user_time: 100,
        system_time: 110,
        address: 0xaabb_ccdd_eeff_0011,
        address_lsb: 12,
        syscall: 13,
        call_address: 0x8877_6655_4433_2211,
        architecture: 0xc000_003e,
    }
    .encode();
    assert_eq!(SignalFixture::read_u32(&record, 0), 35);
    assert_eq!(SignalFixture::read_i32(&record, 8), -1);
    assert_eq!(SignalFixture::read_u32(&record, 12), 10);
    assert_eq!(SignalFixture::read_i32(&record, 44), -90);
    assert_eq!(
        u64::from_le_bytes(record[48..56].try_into().unwrap()),
        0x1122_3344_5566_7788
    );
    assert_eq!(
        u64::from_le_bytes(record[88..96].try_into().unwrap()),
        0x8877_6655_4433_2211
    );
    assert_eq!(SignalFixture::read_u32(&record, 96), 0xc000_003e);
    assert!(record[100..].iter().all(|byte| *byte == 0));
}

#[test]
fn one_read_order() {
    let queue = Arc::new(TestQueue::default());
    queue.push(SignalFixture::info(36, 1));
    queue.push(SignalFixture::info(35, 2));
    queue.push(SignalFixture::info(35, 3));
    let fd = SignalFd::new(SignalFixture::mask(&[35, 36]), SignalFixture::nonblocking(), queue).unwrap();
    let mut records = [0_u8; SIGNALFD_RECORD_SIZE * 4];
    assert_eq!(fd.read(&mut records), Ok(SIGNALFD_RECORD_SIZE * 3));
    assert_eq!(SignalFixture::read_u32(&records, 0), 35);
    assert_eq!(SignalFixture::read_i32(&records, 44), 2);
    assert_eq!(SignalFixture::read_u32(&records, SIGNALFD_RECORD_SIZE), 35);
    assert_eq!(SignalFixture::read_i32(&records, SIGNALFD_RECORD_SIZE + 44), 3);
    assert_eq!(SignalFixture::read_u32(&records, SIGNALFD_RECORD_SIZE * 2), 36);
}

#[test]
fn standard_signal_queue() {
    let queue = Arc::new(TestQueue::default());
    queue.push_standard(SignalFixture::info(10, 1));
    queue.push_standard(SignalFixture::info(10, 2));
    let fd = SignalFd::new(SignalFixture::mask(&[10]), SignalFixture::nonblocking(), queue).unwrap();
    let mut records = [0_u8; SIGNALFD_RECORD_SIZE * 2];
    assert_eq!(fd.read(&mut records), Ok(SIGNALFD_RECORD_SIZE));
    assert_eq!(SignalFixture::read_i32(&records, 44), 1);
}

#[test]
fn independent_descriptions_masks() {
    let queue = Arc::new(TestQueue::default());
    let first = SignalFd::new(SignalFixture::mask(&[10]), SignalFixture::nonblocking(), queue.clone()).unwrap();
    let second = SignalFd::new(SignalFixture::mask(&[12]), SignalFixture::nonblocking(), queue.clone()).unwrap();
    queue.push(SignalFixture::info(10, 1));
    let interests = Readiness::from_bits(Readiness::READ);
    assert!(first.readiness(interests).contains(Readiness::READ));
    assert!(!second.readiness(interests).contains(Readiness::READ));
    let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
    assert_eq!(second.read(&mut record), Err(SignalFdError::WouldBlock));
    assert_eq!(first.read(&mut record), Ok(SIGNALFD_RECORD_SIZE));
}

#[test]
fn retargeting_changes_mask() {
    let queue = Arc::new(TestQueue::default());
    let fd = SignalFd::new(SignalFixture::mask(&[10]), SignalFixture::nonblocking(), queue.clone()).unwrap();
    fd.set_mask(SignalFixture::mask(&[12])).unwrap();
    queue.push(SignalFixture::info(10, 1));
    queue.push(SignalFixture::info(12, 2));
    let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
    assert_eq!(fd.read(&mut record), Ok(SIGNALFD_RECORD_SIZE));
    assert_eq!(SignalFixture::read_u32(&record, 0), 12);
    assert_eq!(queue.len(), 1);
}

#[test]
fn blocking_read_subscription() {
    let queue = Arc::new(TestQueue::default());
    let fd = SignalFd::new(SignalFixture::mask(&[10]), SignalFdFlags::default(), queue.clone()).unwrap();
    let reader = fd.clone();
    let barrier = Arc::new(Barrier::new(2));
    let entered = barrier.clone();
    let thread = thread::spawn(move || {
        entered.wait();
        let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
        reader.read(&mut record).map(|_| SignalFixture::read_u32(&record, 0))
    });
    barrier.wait();
    thread::sleep(Duration::from_millis(10));
    queue.push(SignalFixture::info(10, 1));
    assert_eq!(thread.join().unwrap(), Ok(10));
}

#[test]
fn retirement_wakes_subscription() {
    let queue = Arc::new(TestQueue::default());
    let fd = SignalFd::new(SignalFixture::mask(&[10]), SignalFdFlags::default(), queue.clone()).unwrap();
    let reader = fd.clone();
    let thread = thread::spawn(move || {
        let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
        reader.read(&mut record)
    });
    thread::sleep(Duration::from_millis(10));
    OpenFileDescription::retire(&fd);
    assert_eq!(thread.join().unwrap(), Err(SignalFdError::Retired));
    assert_eq!(queue.active_subscriptions(), 0);
    queue.push(SignalFixture::info(10, 1));
    assert!(
        fd.readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::ERROR)
    );
}

#[test]
fn snapshot_restores_status() {
    let queue = Arc::new(TestQueue::default());
    let snapshot = SignalFdSnapshot {
        mask: SignalFixture::mask(&[10, 12]),
        nonblocking: true,
    };
    let fd = SignalFd::from_snapshot(snapshot, queue).unwrap();
    assert_eq!(fd.snapshot(), snapshot);
    assert_eq!(fd.status().mode, 0o100_600);
    assert_eq!(fd.status().size, 0);
    assert_eq!(fd.status().link_count, 1);
}

#[test]
fn descriptor_port_flag() {
    let queue = Arc::new(TestQueue::default());
    let fd = Arc::new(SignalFd::new(SignalFixture::mask(&[10]), SignalFdFlags::default(), queue.clone()).unwrap());
    let table = DescriptorTable::new(8).unwrap();
    let number = table
        .commit(
            table.reserve(0).unwrap(),
            fd,
            StatusFlags::from_bits(0),
            DescriptorFlags::default(),
        )
        .unwrap();
    let lease = table.pin(number).unwrap();
    lease
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
    assert_eq!(lease.read(&mut record), Err(ObjectError::WouldBlock));
    queue.push(SignalFixture::info(10, 9));
    assert!(
        lease
            .readiness(Readiness::from_bits(Readiness::READ))
            .contains(Readiness::READ)
    );
    assert_eq!(lease.read(&mut record), Ok(SIGNALFD_RECORD_SIZE));
    assert_eq!(SignalFixture::read_i32(&record, 44), 9);
}
