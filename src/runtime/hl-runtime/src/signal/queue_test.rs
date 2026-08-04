use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, mpsc};
use std::thread;

use hl_descriptor::OperationActor;
use hl_event::{
    SIGNALFD_RECORD_SIZE, SignalFd, SignalFdFlags, SignalMask as EventSignalMask, SignalObserver, SignalQueue,
};
use hl_task::{
    PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig, SignalInfo, SignalNumber, TaskRegistry,
};

use crate::TaskSignalQueue;

struct Fixture;

impl Fixture {
    fn registry() -> (Arc<TaskRegistry>, hl_task::ProcessId, hl_task::ThreadId) {
        let registry = Arc::new(
            TaskRegistry::new(RegistryConfig {
                max_processes: 4,
                max_threads: 8,
                max_groups: 8,
                max_pending_signals: 8,
                online_cpus: 1,
            })
            .unwrap(),
        );
        let identity = registry
            .create_init(
                ProcessCredentials::new(1000, 1000, &[], 8).unwrap(),
                ProcessLimits::empty(),
            )
            .unwrap();
        (registry, identity.0, identity.1)
    }

    fn info(number: u8, value: u64) -> SignalInfo {
        let mut info = SignalInfo::bare(SignalNumber::new(number).unwrap());
        info.value = value;
        info
    }

    fn mask(numbers: &[u8]) -> EventSignalMask {
        let bits = numbers
            .iter()
            .fold(0_u64, |bits, number| bits | (1_u64 << (number - 1)));
        EventSignalMask::from_bits(bits)
    }
}

struct CountObserver(AtomicUsize);

impl SignalObserver for CountObserver {
    fn signal_available(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct BlockingObserver {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl SignalObserver for BlockingObserver {
    fn signal_available(&self) {
        self.entered.wait();
        self.release.wait();
    }
}

#[test]
fn adapter_priority_fifo() {
    let (tasks, process, thread) = Fixture::registry();
    let queue = TaskSignalQueue::new(tasks, thread);
    assert!(
        queue
            .enqueue(PendingTarget::Process(process), Fixture::info(10, 1),)
            .unwrap()
    );
    assert!(
        !queue
            .enqueue(PendingTarget::Process(process), Fixture::info(10, 2),)
            .unwrap()
    );
    for info in [Fixture::info(33, 3), Fixture::info(33, 4), Fixture::info(32, 5)] {
        queue.enqueue(PendingTarget::Process(process), info).unwrap();
    }
    let mask = Fixture::mask(&[10, 32, 33]);
    let mut values = Vec::new();
    while let Some(info) = queue.dequeue(mask).unwrap() {
        values.push((info.signal, info.pointer));
    }
    assert_eq!(values, [(10, 1), (32, 5), (33, 3), (33, 4)]);
}

#[test]
fn thread_pending_sets() {
    let (tasks, process, first) = Fixture::registry();
    let clone = tasks.begin_clone_thread(first).unwrap();
    let second = tasks.commit_clone_thread(clone).unwrap();
    let first_queue = TaskSignalQueue::new(Arc::clone(&tasks), first);
    let second_queue = TaskSignalQueue::new(tasks, second);
    first_queue
        .enqueue(PendingTarget::Thread(second), Fixture::info(12, 7))
        .unwrap();
    first_queue
        .enqueue(PendingTarget::Process(process), Fixture::info(13, 8))
        .unwrap();

    assert!(!first_queue.has_pending(Fixture::mask(&[12])));
    assert_eq!(first_queue.dequeue(Fixture::mask(&[13])).unwrap().unwrap().pointer, 8);
    assert_eq!(second_queue.dequeue(Fixture::mask(&[12])).unwrap().unwrap().pointer, 7);
}

#[test]
fn observer_changes_quiesce() {
    let (tasks, process, thread) = Fixture::registry();
    let queue = TaskSignalQueue::new(tasks, thread);
    let observer = Arc::new(CountObserver(AtomicUsize::new(0)));
    let subscription = queue.subscribe(observer.clone()).unwrap();
    queue
        .enqueue(PendingTarget::Process(process), Fixture::info(10, 1))
        .unwrap();
    queue
        .enqueue(PendingTarget::Process(process), Fixture::info(10, 2))
        .unwrap();
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    subscription.quiesce();
    queue
        .enqueue(PendingTarget::Process(process), Fixture::info(32, 3))
        .unwrap();
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
    queue.quiesce();
    assert!(queue.subscribe(observer).is_err());
}

#[test]
fn registry_enqueue_notifies() {
    let (tasks, process, thread) = Fixture::registry();
    let queue = TaskSignalQueue::new(Arc::clone(&tasks), thread);
    let observer = Arc::new(CountObserver(AtomicUsize::new(0)));
    let _subscription = queue.subscribe(observer.clone()).unwrap();
    tasks
        .enqueue_signal(PendingTarget::Process(process), Fixture::info(10, 1))
        .unwrap();
    assert_eq!(observer.0.load(Ordering::SeqCst), 1);
}

#[test]
fn quiesce_future_callbacks() {
    let (tasks, process, thread_id) = Fixture::registry();
    let queue = Arc::new(TaskSignalQueue::new(tasks, thread_id));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let subscription = queue
        .subscribe(Arc::new(BlockingObserver {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }))
        .unwrap();
    let producer = {
        let queue = Arc::clone(&queue);
        thread::spawn(move || {
            queue
                .enqueue(PendingTarget::Process(process), Fixture::info(32, 1))
                .unwrap();
        })
    };
    entered.wait();
    let (completed, observed) = mpsc::channel();
    let quiescer = thread::spawn(move || {
        subscription.quiesce();
        completed.send(()).unwrap();
    });
    assert!(observed.try_recv().is_err());
    release.wait();
    producer.join().unwrap();
    quiescer.join().unwrap();
    observed.recv().unwrap();
    queue
        .enqueue(PendingTarget::Process(process), Fixture::info(33, 2))
        .unwrap();
}

#[test]
fn adapter_record_path() {
    let (tasks, process, thread_id) = Fixture::registry();
    let queue = Arc::new(TaskSignalQueue::new(tasks, thread_id));
    let fd = SignalFd::new(
        Fixture::mask(&[10]),
        SignalFdFlags::from_bits(SignalFdFlags::NONBLOCKING),
        queue.clone(),
    )
    .unwrap();
    queue
        .enqueue(PendingTarget::Process(process), Fixture::info(10, 0x1234))
        .unwrap();
    let mut record = [0_u8; SIGNALFD_RECORD_SIZE];
    assert_eq!(fd.read(&mut record).unwrap(), SIGNALFD_RECORD_SIZE);
    assert_eq!(u32::from_ne_bytes(record[0..4].try_into().unwrap()), 10);
}

#[test]
fn fork_actor_isolation() {
    let (tasks, parent_process, parent_thread) = Fixture::registry();
    let plan = tasks.begin_fork_process(parent_thread).unwrap();
    let (child_process, child_thread) = tasks.commit_fork_process(plan).unwrap();
    let queue = TaskSignalQueue::new(Arc::clone(&tasks), parent_thread);
    tasks
        .enqueue_signal(PendingTarget::Thread(parent_thread), Fixture::info(10, 11))
        .unwrap();
    tasks
        .enqueue_signal(PendingTarget::Thread(child_thread), Fixture::info(10, 22))
        .unwrap();
    let mask = Fixture::mask(&[10]);
    let child = queue
        .prepare_context(
            mask,
            OperationActor {
                process: child_process.wire_parts().0,
                process_generation: child_process.wire_parts().1,
                thread: child_thread.wire_parts().0,
                thread_generation: child_thread.wire_parts().1,
            },
        )
        .unwrap()
        .unwrap();
    let parent = queue
        .prepare_context(
            mask,
            OperationActor {
                process: parent_process.wire_parts().0,
                process_generation: parent_process.wire_parts().1,
                thread: parent_thread.wire_parts().0,
                thread_generation: parent_thread.wire_parts().1,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(child.info().pointer, 22);
    assert_eq!(parent.info().pointer, 11);
    assert!(child.commit().unwrap());
    assert!(parent.commit().unwrap());
    assert!(
        !tasks
            .has_signal_wait(child_thread, hl_task::SignalMask::from_bits(mask.bits()))
            .unwrap()
    );
    assert!(
        !tasks
            .has_signal_wait(parent_thread, hl_task::SignalMask::from_bits(mask.bits()))
            .unwrap()
    );
}
