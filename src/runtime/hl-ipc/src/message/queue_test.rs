use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use hl_sync::Interruption;
use hl_time::{ClockError, Deadline, MonotonicClock, MonotonicInstant};

use crate::{
    Credentials, IPC_PRIVATE, IpcKey, MSG_COPY, MSG_EXCEPT, MSG_NOERROR, MSG_NOWAIT, MessageError, MessageLimits,
    MessageQueueNamespace, MsgGetRequest,
};

const OWNER: Credentials = Credentials { uid: 10, gid: 20 };
const OTHER: Credentials = Credentials { uid: 11, gid: 21 };

struct Fixture;

impl Fixture {
    fn namespace(limits: MessageLimits) -> MessageQueueNamespace {
        MessageQueueNamespace::new(limits).unwrap()
    }

    fn request(key: IpcKey) -> MsgGetRequest {
        MsgGetRequest {
            key,
            create: true,
            exclusive: false,
            mode: 0o600,
            actor: OWNER,
            pid: 100,
            now: 1,
        }
    }
}

#[derive(Debug)]
struct Clock(AtomicU64);

impl MonotonicClock for Clock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(self.0.load(Ordering::Acquire)))
    }
}

#[test]
fn keys_private_exclusive() {
    let namespace = Fixture::namespace(MessageLimits::default());
    let id = namespace.msgget(Fixture::request(IpcKey(1))).unwrap();
    assert_eq!(namespace.msgget(Fixture::request(IpcKey(1))), Ok(id));
    let mut exclusive = Fixture::request(IpcKey(1));
    exclusive.exclusive = true;
    assert_eq!(namespace.msgget(exclusive), Err(MessageError::Exists));
    assert_eq!(
        namespace.send(id, OTHER, 1, 1, b"x", MSG_NOWAIT, 2),
        Err(MessageError::Permission)
    );
    assert_eq!(
        namespace.set_permissions(id, OTHER, OTHER, 0o600, 2),
        Err(MessageError::Permission)
    );
    namespace.set_permissions(id, OWNER, OTHER, 0o600, 2).unwrap();
    namespace.send(id, OTHER, 1, 1, b"x", MSG_NOWAIT, 2).unwrap();
    namespace.receive(id, OTHER, 1, 0, 1, MSG_NOWAIT, 2).unwrap();
    namespace.send(id, OWNER, 1, 1, b"c", MSG_NOWAIT, 2).unwrap();
    namespace.receive(id, OWNER, 1, 0, 1, MSG_NOWAIT, 2).unwrap();
    assert_ne!(
        namespace.msgget(Fixture::request(IPC_PRIVATE)).unwrap(),
        namespace.msgget(Fixture::request(IPC_PRIVATE)).unwrap()
    );
    namespace.remove(id, OWNER, 100, 3).unwrap();
    assert_eq!(
        namespace.receive(id, OWNER, 100, 0, 1, MSG_NOWAIT, 4),
        Err(MessageError::Removed)
    );
    let next = namespace.msgget(Fixture::request(IpcKey(1))).unwrap();
    assert_eq!(next.slot, id.slot);
    assert_ne!(next.generation, id.generation);
}

#[test]
fn zero_positive_negative() {
    let namespace = Fixture::namespace(MessageLimits::default());
    let id = namespace.msgget(Fixture::request(IpcKey(2))).unwrap();
    for (message_type, bytes) in [(5, b"a".as_slice()), (2, b"b"), (5, b"c"), (3, b"d")] {
        namespace
            .send(id, OWNER, 1, message_type, bytes, MSG_NOWAIT, 2)
            .unwrap();
    }
    assert_eq!(
        namespace
            .receive(id, OWNER, 1, 1, 8, MSG_COPY | MSG_NOWAIT, 3)
            .unwrap()
            .bytes,
        b"b"
    );
    assert_eq!(namespace.metadata(id).unwrap().messages, 4);
    assert_eq!(
        namespace.receive(id, OWNER, 1, -4, 8, MSG_NOWAIT, 3).unwrap().bytes,
        b"b"
    );
    assert_eq!(
        namespace
            .receive(id, OWNER, 1, 5, 8, MSG_EXCEPT | MSG_NOWAIT, 3)
            .unwrap()
            .bytes,
        b"d"
    );
    assert_eq!(
        namespace.receive(id, OWNER, 1, 5, 8, MSG_NOWAIT, 3).unwrap().bytes,
        b"a"
    );
    assert_eq!(
        namespace.receive(id, OWNER, 1, 0, 8, MSG_NOWAIT, 3).unwrap().bytes,
        b"c"
    );
}

#[test]
fn prepared_receive_aborts() {
    let namespace = Fixture::namespace(MessageLimits::default());
    let id = namespace.msgget(Fixture::request(IPC_PRIVATE)).unwrap();
    namespace.send(id, OWNER, 1, 7, b"payload", MSG_NOWAIT, 2).unwrap();
    {
        let prepared = namespace.prepare_receive(id, OWNER, 1, 0, 32, MSG_NOWAIT, 3).unwrap();
        assert_eq!(prepared.message().bytes, b"payload");
        assert!(matches!(
            namespace.prepare_receive(id, OWNER, 2, 0, 32, MSG_NOWAIT, 3),
            Err(MessageError::NoMessage)
        ));
    }
    assert_eq!(namespace.metadata(id).unwrap().messages, 1);
    let prepared = namespace.prepare_receive(id, OWNER, 3, 0, 32, MSG_NOWAIT, 4).unwrap();
    assert_eq!(prepared.commit().unwrap().bytes, b"payload");
    assert_eq!(namespace.metadata(id).unwrap().messages, 0);
}

#[test]
fn again_no_message() {
    let limits = MessageLimits {
        queues: 1,
        queue_bytes: 3,
        queue_messages: 1,
        total_bytes: 3,
        total_messages: 1,
        message_bytes: 3,
    };
    let namespace = Fixture::namespace(limits);
    let id = namespace.msgget(Fixture::request(IpcKey(3))).unwrap();
    assert_eq!(
        namespace.receive(id, OWNER, 1, 0, 1, MSG_NOWAIT, 2),
        Err(MessageError::NoMessage)
    );
    namespace.send(id, OWNER, 1, 1, b"abc", MSG_NOWAIT, 2).unwrap();
    assert_eq!(
        namespace.send(id, OWNER, 1, 2, b"x", MSG_NOWAIT, 2),
        Err(MessageError::Again)
    );
    assert_eq!(
        namespace.receive(id, OWNER, 1, 0, 2, MSG_NOWAIT, 3),
        Err(MessageError::TooBig)
    );
    assert_eq!(namespace.metadata(id).unwrap().messages, 1);
    let received = namespace
        .receive(id, OWNER, 1, 0, 2, MSG_NOWAIT | MSG_NOERROR, 3)
        .unwrap();
    assert_eq!(received.bytes, b"ab");
    assert!(received.truncated);
}

#[test]
fn rmid_wakes_blocked() {
    let namespace = Arc::new(Fixture::namespace(MessageLimits::default()));
    let id = namespace.msgget(Fixture::request(IpcKey(4))).unwrap();
    let worker_namespace = namespace.clone();
    let worker = thread::spawn(move || {
        worker_namespace.receive_wait(
            id,
            OWNER,
            1,
            0,
            8,
            0,
            &Interruption::new(),
            None,
            &Clock(AtomicU64::new(0)),
            2,
        )
    });
    while namespace.metadata(id).unwrap().messages != 0 {
        thread::yield_now();
    }
    namespace.remove(id, OWNER, 1, 3).unwrap();
    assert_eq!(worker.join().unwrap(), Err(MessageError::Removed));

    let id = namespace.msgget(Fixture::request(IpcKey(4))).unwrap();
    let interruption = Interruption::new();
    interruption.interrupt();
    assert_eq!(
        namespace.receive_wait(id, OWNER, 1, 0, 8, 0, &interruption, None, &Clock(AtomicU64::new(0)), 4,),
        Err(MessageError::Interrupted)
    );
    assert_eq!(
        namespace.receive_wait(
            id,
            OWNER,
            1,
            0,
            8,
            0,
            &Interruption::new(),
            Some(Deadline::from_nanoseconds(0)),
            &Clock(AtomicU64::new(0)),
            4,
        ),
        Err(MessageError::TimedOut)
    );
}

#[test]
fn snapshot_restore_is() {
    let namespace = Fixture::namespace(MessageLimits::default());
    let id = namespace.msgget(Fixture::request(IpcKey(5))).unwrap();
    namespace.send(id, OWNER, 7, 3, b"data", MSG_NOWAIT, 2).unwrap();
    let snapshot = namespace.snapshot();
    let restored = MessageQueueNamespace::restore(MessageLimits::default(), snapshot.clone()).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
    let mut corrupt = snapshot;
    corrupt.queues[0].metadata.bytes += 1;
    assert_eq!(
        MessageQueueNamespace::restore(MessageLimits::default(), corrupt).unwrap_err(),
        MessageError::ResourceLimit
    );
}

#[test]
fn concurrent_senders_respect() {
    let limits = MessageLimits {
        queues: 1,
        queue_bytes: 64,
        queue_messages: 64,
        total_bytes: 64,
        total_messages: 64,
        message_bytes: 1,
    };
    let namespace = Arc::new(Fixture::namespace(limits));
    let id = namespace.msgget(Fixture::request(IpcKey(6))).unwrap();
    let workers: Vec<_> = (0..128)
        .map(|pid| {
            let namespace = namespace.clone();
            thread::spawn(move || namespace.send(id, OWNER, pid, 1, b"x", MSG_NOWAIT, 2))
        })
        .collect();
    let successes = workers
        .into_iter()
        .filter_map(|worker| worker.join().unwrap().ok())
        .count();
    assert_eq!(successes, 64);
    assert_eq!(namespace.metadata(id).unwrap().messages, 64);
}

#[test]
fn queue_byte_control() {
    let limits = MessageLimits {
        queues: 2,
        queue_bytes: 8,
        queue_messages: 16,
        total_bytes: 32,
        total_messages: 32,
        message_bytes: 8,
    };
    let namespace = Fixture::namespace(limits);
    let first = namespace.msgget(Fixture::request(IpcKey(8))).unwrap();
    let second = namespace.msgget(Fixture::request(IpcKey(9))).unwrap();
    assert_eq!(namespace.metadata(first).unwrap().maximum_bytes, 8);

    namespace.set_control(first, OWNER, OWNER, 0o600, 2, 2).unwrap();
    namespace.send(first, OWNER, 1, 1, b"ab", MSG_NOWAIT, 2).unwrap();
    assert_eq!(
        namespace.send(first, OWNER, 1, 1, b"c", MSG_NOWAIT, 2),
        Err(MessageError::Again)
    );
    namespace.send(second, OWNER, 1, 1, b"abcd", MSG_NOWAIT, 2).unwrap();

    assert_eq!(
        namespace.set_control(first, OWNER, OWNER, 0o600, 9, 3),
        Err(MessageError::Permission)
    );
    namespace
        .set_control(first, Credentials { uid: 0, gid: 0 }, OWNER, 0o640, 12, 4)
        .unwrap();
    namespace.send(first, OWNER, 1, 1, b"cdefgh", MSG_NOWAIT, 5).unwrap();
    let metadata = namespace.metadata(first).unwrap();
    assert_eq!(metadata.maximum_bytes, 12);
    assert_eq!(metadata.mode, 0o640);
    assert_eq!(metadata.changed_at, 4);
    assert_eq!((metadata.creator_uid, metadata.creator_gid), (OWNER.uid, OWNER.gid));
}

#[test]
fn snapshot_preserves_queue() {
    let limits = MessageLimits {
        queues: 1,
        queue_bytes: 8,
        queue_messages: 16,
        total_bytes: 16,
        total_messages: 16,
        message_bytes: 8,
    };
    let namespace = Fixture::namespace(limits);
    let id = namespace.msgget(Fixture::request(IpcKey(10))).unwrap();
    namespace
        .set_control(id, Credentials { uid: 0, gid: 0 }, OWNER, 0o600, 12, 2)
        .unwrap();
    namespace.send(id, OWNER, 1, 1, b"123456", MSG_NOWAIT, 3).unwrap();
    // Linux permits lowering qbytes below the bytes already resident; new
    // sends remain blocked until receives bring the queue below the limit.
    namespace.set_control(id, OWNER, OWNER, 0o600, 5, 4).unwrap();
    let snapshot = namespace.snapshot();
    let restored = MessageQueueNamespace::restore(limits, snapshot.clone()).unwrap();
    assert_eq!(restored.snapshot(), snapshot);

    let mut zero = snapshot.clone();
    zero.queues[0].metadata.maximum_bytes = 0;
    assert_eq!(
        MessageQueueNamespace::restore(limits, zero).unwrap_err(),
        MessageError::ResourceLimit
    );
}

#[test]
fn deterministic_selection_model() {
    let namespace = Fixture::namespace(MessageLimits::default());
    let id = namespace.msgget(Fixture::request(IpcKey(7))).unwrap();
    for value in [7, 3, 5, 3, 9] {
        namespace
            .send(id, OWNER, 1, value, &[value as u8], MSG_NOWAIT, 2)
            .unwrap();
    }
    let mut output = Vec::new();
    while let Ok(message) = namespace.receive(id, OWNER, 1, -9, 1, MSG_NOWAIT, 3) {
        output.push(message.message_type);
    }
    assert_eq!(output, [3, 3, 5, 7, 9]);
}
