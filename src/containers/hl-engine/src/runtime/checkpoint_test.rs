use super::{CaptureFailure, Server, protocol};
use crate::composition::{CheckpointSink, CheckpointSource, CompositionError};
use std::{
    os::unix::net::UnixStream,
    sync::{Arc, Condvar, Mutex, mpsc},
    time::Duration,
};

struct Store;

impl CheckpointSink for Store {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn put_until(&self, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn commit_until(&self, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[derive(Default)]
struct RecordingStore(Mutex<usize>);

impl CheckpointSink for RecordingStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn commit(&self, _: &[u8]) -> Result<(), CompositionError> {
        *self.0.lock().unwrap() += 1;
        Ok(())
    }
    fn put_until(&self, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn commit_until(&self, manifest: &[u8], deadline: std::time::Instant) -> Result<(), CompositionError> {
        if std::time::Instant::now() >= deadline {
            return Err(CompositionError::DeadlineExceeded);
        }
        self.commit(manifest)
    }
}

impl CheckpointSource for RecordingStore {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list(&self) -> Result<Vec<String>, CompositionError> {
        Ok(Vec::new())
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[test]
fn restore_digest_remains_available_without_a_capture_scope() {
    let store = Arc::new(RecordingStore::default());
    let server = Server::new(store.clone(), store);
    let request = protocol::Request {
        op: protocol::DIGEST,
        stream: 0,
        offset: 0,
        length: 0,
        name_size: 0,
        generation: 0,
    };
    assert_eq!(server.dispatch(1, &request, "", &[]).status, protocol::STATUS_OK);
}

impl CheckpointSource for Store {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[test]
fn stop_interrupts_silent_checkpoint_peer() {
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let (channel, _silent) = UnixStream::pair().unwrap();
    let worker = Arc::clone(&server);
    let (done, completed) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        worker.serve(channel, 1);
        let _ = done.send(());
    });
    while server.channels.lock().unwrap().is_empty() {
        std::thread::yield_now();
    }
    server.stop();
    completed.recv_timeout(Duration::from_millis(250)).unwrap();
}

#[test]
fn peer_accepted_after_stop_cannot_escape_shutdown() {
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let (channel, _silent) = UnixStream::pair().unwrap();
    server.stop();
    let worker = Arc::clone(&server);
    let (done, completed) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        worker.serve(channel, 1);
        let _ = done.send(());
    });
    completed.recv_timeout(Duration::from_millis(250)).unwrap();
}

#[test]
fn expired_capture_never_reaches_manifest_publication() {
    let store = Arc::new(RecordingStore::default());
    let sink: Arc<dyn CheckpointSink> = store.clone();
    let source: Arc<dyn CheckpointSource> = store.clone();
    let server = Server::new(sink, source);
    assert_eq!(
        server.begin_capture(1, std::time::Instant::now()),
        Err(CaptureFailure::Deadline)
    );
    let request = protocol::Request {
        op: protocol::COMMIT,
        stream: 0,
        offset: 0,
        length: 0,
        name_size: 0,
        generation: 0,
    };

    let reply = server.dispatch(1, &request, "", b"manifest");

    assert_ne!(reply.status, protocol::STATUS_OK);
    assert_eq!(*store.0.lock().unwrap(), 0);
    assert!(!server.committed());
}

fn commit_request() -> protocol::Request {
    protocol::Request {
        op: protocol::COMMIT,
        stream: 0,
        offset: 0,
        length: 0,
        name_size: 0,
        generation: 1,
    }
}

#[test]
fn timed_out_capture_rejects_every_late_manifest() {
    let store = Arc::new(RecordingStore::default());
    let server = Server::new(store.clone(), store.clone());
    let capture = server
        .begin_capture(1, std::time::Instant::now() + Duration::from_millis(10))
        .unwrap();
    let result = server
        .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(result, Some(Err(CaptureFailure::Deadline)));

    let reply = server.dispatch(1, &commit_request(), "", b"late");
    assert_ne!(reply.status, protocol::STATUS_OK);
    assert_eq!(*store.0.lock().unwrap(), 0);
    assert_eq!(
        server.begin_capture(1, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Poisoned),
        "a timed-out engine must fail closed instead of admitting stale work into a later capture"
    );
}

#[test]
fn request_from_another_trigger_generation_cannot_enter_capture() {
    let store = Arc::new(RecordingStore::default());
    let server = Server::new(store.clone(), store.clone());
    server
        .begin_capture(1, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let mut stale = commit_request();
    stale.generation = 0;
    assert_ne!(server.dispatch(1, &stale, "", b"stale").status, protocol::STATUS_OK);
    stale.generation = 2;
    assert_ne!(server.dispatch(1, &stale, "", b"future").status, protocol::STATUS_OK);
    assert_eq!(*store.0.lock().unwrap(), 0);
}

#[derive(Default)]
struct PublicationGate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}

#[derive(Default)]
struct PublishedUncertain(Mutex<usize>);

impl CheckpointSink for PublishedUncertain {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn put_until(&self, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn commit_until(&self, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        *self.0.lock().unwrap() += 1;
        Err(CompositionError::PublishedNotDurable)
    }
}

impl CheckpointSource for PublishedUncertain {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[test]
fn published_but_not_durable_is_authoritative_success_not_retryable_failure() {
    let store = Arc::new(PublishedUncertain::default());
    let server = Server::new(store.clone(), store.clone());
    let capture = server
        .begin_capture(1, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        server.dispatch(1, &commit_request(), "", b"manifest").status,
        protocol::STATUS_OK
    );
    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Ok(()))
    );
    assert_eq!(*store.0.lock().unwrap(), 1);
}

#[derive(Default)]
struct MutationPublicationRace {
    state: Mutex<(usize, bool, bool, usize)>,
    changed: Condvar,
}

impl MutationPublicationRace {
    fn wait_mutations(&self, count: usize) {
        let mut state = self.state.lock().unwrap();
        while state.0 < count {
            state = self.changed.wait(state).unwrap();
        }
    }
    fn release_mutations(&self, fail: bool) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        state.2 = fail;
        self.changed.notify_all();
    }
    fn commits(&self) -> usize {
        self.state.lock().unwrap().3
    }
}

impl CheckpointSink for MutationPublicationRace {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn put_until(&self, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
        if state.2 {
            Err(CompositionError::RuntimeConstruction)
        } else {
            Ok(())
        }
    }
    fn commit_until(&self, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        self.state.lock().unwrap().3 += 1;
        Ok(())
    }
}

impl CheckpointSource for MutationPublicationRace {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

fn finish_request(stream: u64, generation: u32) -> protocol::Request {
    protocol::Request {
        op: protocol::OBJECT_FINISH,
        stream,
        offset: 0,
        length: 0,
        name_size: 0,
        generation,
    }
}

#[test]
fn commit_waits_for_all_admitted_mutations_and_never_publishes_after_one_fails() {
    let store = Arc::new(MutationPublicationRace::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    server
        .begin_capture(1, std::time::Instant::now() + Duration::from_secs(2))
        .unwrap();
    {
        let mut state = server.state.lock().unwrap();
        state.open.insert(
            (1, 1),
            super::Object {
                name: "one".into(),
                bytes: vec![1],
            },
        );
        state.open.insert(
            (2, 2),
            super::Object {
                name: "two".into(),
                bytes: vec![2],
            },
        );
    }
    let first = Arc::clone(&server);
    let one = std::thread::spawn(move || first.dispatch(1, &finish_request(1, 1), "", &[]));
    let second = Arc::clone(&server);
    let two = std::thread::spawn(move || second.dispatch(2, &finish_request(2, 1), "", &[]));
    store.wait_mutations(2);
    let publisher = Arc::clone(&server);
    let commit = std::thread::spawn(move || publisher.dispatch(1, &commit_request(), "", b"manifest"));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(store.commits(), 0, "COMMIT crossed the mutation barrier");
    store.release_mutations(true);

    assert_ne!(one.join().unwrap().status, protocol::STATUS_OK);
    assert_ne!(two.join().unwrap().status, protocol::STATUS_OK);
    assert_ne!(commit.join().unwrap().status, protocol::STATUS_OK);
    assert_eq!(store.commits(), 0, "failed mutation must make publication unreachable");
}

#[test]
fn dropped_mutation_admission_latches_failure_and_wakes_commit() {
    let store = Arc::new(MutationPublicationRace::default());
    let server = Server::new(store.clone(), store.clone());
    server
        .begin_capture(1, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    drop(server.admit_mutation().unwrap().unwrap());
    assert_ne!(
        server.dispatch(1, &commit_request(), "", b"manifest").status,
        protocol::STATUS_OK
    );
    assert_eq!(store.commits(), 0);
}

#[test]
fn commit_expires_behind_slow_admitted_mutation_before_publication() {
    let store = Arc::new(MutationPublicationRace::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    server
        .begin_capture(1, std::time::Instant::now() + Duration::from_millis(40))
        .unwrap();
    server.state.lock().unwrap().open.insert(
        (1, 1),
        super::Object {
            name: "slow".into(),
            bytes: vec![1],
        },
    );
    let worker = Arc::clone(&server);
    let mutation = std::thread::spawn(move || worker.dispatch(1, &finish_request(1, 1), "", &[]));
    store.wait_mutations(1);
    let publisher = Arc::clone(&server);
    let commit = std::thread::spawn(move || publisher.dispatch(1, &commit_request(), "", b"manifest"));
    std::thread::sleep(Duration::from_millis(60));
    store.release_mutations(false);

    assert_ne!(mutation.join().unwrap().status, protocol::STATUS_OK);
    assert_ne!(commit.join().unwrap().status, protocol::STATUS_OK);
    assert_eq!(
        store.commits(),
        0,
        "deadline behind admitted work must precede Publishing"
    );
}

impl PublicationGate {
    fn wait_started(&self) {
        let mut state = self.state.lock().unwrap();
        while !state.0 {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.changed.notify_all();
    }
}

impl CheckpointSink for PublicationGate {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn put_until(&self, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn commit_until(&self, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
        Ok(())
    }
}

impl CheckpointSource for PublicationGate {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[test]
fn publication_claimed_before_deadline_reports_its_real_late_result() {
    let store = Arc::new(PublicationGate::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let capture = server
        .begin_capture(1, std::time::Instant::now() + Duration::from_millis(250))
        .unwrap();
    let publisher = Arc::clone(&server);
    let publish = std::thread::spawn(move || publisher.dispatch(1, &commit_request(), "", b"manifest"));
    store.wait_started();
    std::thread::sleep(Duration::from_millis(300));

    let waiter = Arc::clone(&server);
    let waited =
        std::thread::spawn(move || waiter.wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1)));
    store.release();

    assert_eq!(publish.join().unwrap().status, protocol::STATUS_OK);
    assert_eq!(waited.join().unwrap().unwrap(), Some(Ok(())));
    assert!(server.committed());
    assert_eq!(
        server.begin_capture(1, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Busy),
        "a completed one-shot capture must not admit stale channel work into another scope"
    );
}

#[test]
fn poisoned_capture_coordination_fails_closed() {
    let store = Arc::new(RecordingStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let poison = Arc::clone(&server);
    let _ = std::thread::spawn(move || {
        let _held = poison.capture.lock().unwrap();
        panic!("intentional capture-lock poison");
    })
    .join();

    assert_eq!(
        server.begin_capture(1, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Poisoned)
    );
    let reply = server.dispatch(1, &commit_request(), "", b"must-not-publish");
    assert_ne!(reply.status, protocol::STATUS_OK);
    assert_eq!(*store.0.lock().unwrap(), 0);
}

#[test]
fn capture_lock_poison_wakes_a_waiter_with_deterministic_error() {
    let store = Arc::new(RecordingStore::default());
    let server = Arc::new(Server::new(store.clone(), store));
    let capture = server
        .begin_capture(1, std::time::Instant::now() + Duration::from_secs(2))
        .unwrap();
    let waiter = Arc::clone(&server);
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    let waiting = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        waiter.wait_capture(capture, std::time::Instant::now() + Duration::from_secs(2))
    });
    ready_rx.recv().unwrap();
    std::thread::sleep(Duration::from_millis(10));
    let poison = Arc::clone(&server);
    let _ = std::thread::spawn(move || {
        let _held = poison.capture.lock().unwrap();
        panic!("intentional waiter poison");
    })
    .join();
    server.capture_changed.notify_all();

    assert_eq!(waiting.join().unwrap(), Err(CaptureFailure::Poisoned));
}
