#![allow(unsafe_code)]

use super::{CaptureFailure, Server, protocol};
use crate::composition::{CheckpointSink, CheckpointSource, CompositionError};
use std::{
    io::Write,
    mem,
    os::fd::{AsRawFd, FromRawFd},
};
use std::{
    num::NonZeroU64,
    os::unix::net::UnixStream,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

#[test]
fn production_broker_revokes_relayed_channel_when_authenticated_child_exits() {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (broker, transport) = hl_native::CheckpointTransport::create().expect("checkpoint transport");
    let (relay_child, relay_survivor) = UnixStream::pair().expect("descriptor relay");
    let mut release = [-1; 2];
    // SAFETY: release names writable storage for two new descriptors.
    assert_eq!(unsafe { libc::pipe(release.as_mut_ptr()) }, 0);
    // SAFETY: no Rust synchronization state is touched in the child; it only uses inherited descriptors then exits.
    let child = unsafe { libc::fork() };
    assert!(child >= 0, "fork checkpoint peer");
    if child == 0 {
        // SAFETY: child owns its inherited ends and terminates with _exit.
        unsafe {
            libc::close(release[1]);
        }
        let channel = transport.connect_for_test().expect("child checkpoint channel");
        send_descriptor(&relay_child, channel.as_raw_fd());
        let mut byte = 0_u8;
        // SAFETY: release[0] is live and byte is writable.
        let read = unsafe { libc::read(release[0], (&raw mut byte).cast(), 1) };
        // SAFETY: no Rust destructors are run after fork.
        unsafe { libc::_exit(if read == 1 { 0 } else { 91 }) }
    }
    drop(relay_child);
    drop(transport);
    // SAFETY: parent no longer uses the child's release end.
    unsafe { libc::close(release[0]) };
    let (channel, authority) = broker
        .accept(Duration::from_secs(2))
        .expect("authenticated production accept");
    assert_eq!(authority.host_pid, u64::try_from(child).unwrap());
    assert_ne!(authority.host_birth, 0);
    let mut survivor = receive_descriptor(&relay_survivor);
    // SAFETY: one byte releases the child and the descriptor is uniquely owned here.
    assert_eq!(unsafe { libc::write(release[1], b"x".as_ptr().cast(), 1) }, 1);
    // SAFETY: parent owns this release descriptor.
    unsafe { libc::close(release[1]) };
    let mut status = 0;
    // SAFETY: child is a direct unreaped child and status is writable.
    assert_eq!(unsafe { libc::waitpid(child, &raw mut status, 0) }, child);
    assert!(libc::WIFEXITED(status));
    assert_eq!(libc::WEXITSTATUS(status), 0);

    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let worker = Arc::clone(&server);
    let (done, completed) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        worker.serve_authenticated_for_test(channel, authority);
        let _ = done.send(());
    });
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    let mut request = [0_u8; protocol::REQUEST_BYTES];
    request[0..4].copy_from_slice(&protocol::MAGIC_REQUEST.to_ne_bytes());
    request[4..8].copy_from_slice(&protocol::ABI.to_ne_bytes());
    request[8..12].copy_from_slice(&protocol::GROUP_PRESENT.to_ne_bytes());
    survivor
        .write_all(&request)
        .expect("survivor writes a valid checkpoint request");
    completed
        .recv_timeout(Duration::from_secs(1))
        .expect("revoked connection must terminate");
    assert_eq!(server.connections.load(Ordering::Acquire), 0);
    assert_eq!(server.dispatch_count(), 0, "revoked peer request reached dispatch");
}

fn send_descriptor(channel: &UnixStream, descriptor: i32) {
    let mut byte = 0_u8;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0_u8; 64];
    // SAFETY: message points to live stack storage and contains one correctly sized SCM_RIGHTS record.
    unsafe {
        let mut message: libc::msghdr = mem::zeroed();
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(mem::size_of::<i32>() as _) as _;
        *libc::CMSG_DATA(header).cast::<i32>() = descriptor;
        message.msg_controllen = libc::CMSG_SPACE(mem::size_of::<i32>() as _) as _;
        assert_eq!(libc::sendmsg(channel.as_raw_fd(), &message, 0), 1);
    }
}

fn receive_descriptor(channel: &UnixStream) -> UnixStream {
    let mut byte = 0_u8;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0_u8; 64];
    // SAFETY: message points to writable stack storage; a successful receive transfers one descriptor.
    unsafe {
        let mut message: libc::msghdr = mem::zeroed();
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len() as _;
        assert_eq!(libc::recvmsg(channel.as_raw_fd(), &raw mut message, 0), 1);
        let header = libc::CMSG_FIRSTHDR(&message);
        assert!(!header.is_null());
        assert_eq!((*header).cmsg_level, libc::SOL_SOCKET);
        assert_eq!((*header).cmsg_type, libc::SCM_RIGHTS);
        UnixStream::from_raw_fd(*libc::CMSG_DATA(header).cast::<i32>())
    }
}

fn test_transaction() -> NonZeroU64 {
    NonZeroU64::MIN
}

struct Store;

impl CheckpointSink for Store {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[derive(Default)]
struct RecordingStore(Mutex<usize>);

impl CheckpointSink for RecordingStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], deadline: std::time::Instant) -> Result<(), CompositionError> {
        if std::time::Instant::now() >= deadline {
            return Err(CompositionError::DeadlineExceeded);
        }
        *self.0.lock().unwrap() += 1;
        Ok(())
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

#[derive(Default)]
struct RecoveryStore(Mutex<Vec<(String, Vec<u8>)>>);

impl CheckpointSink for RecoveryStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(
        &self,
        _: NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CompositionError> {
        if std::time::Instant::now() >= deadline {
            return Err(CompositionError::DeadlineExceeded);
        }
        self.0.lock().unwrap().push((name.to_owned(), bytes.to_vec()));
        Ok(())
    }

    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        self.0.lock().unwrap().clear();
        Ok(())
    }

    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

impl CheckpointSource for RecoveryStore {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn list_until(&self, deadline: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        if std::time::Instant::now() >= deadline {
            return Err(CompositionError::DeadlineExceeded);
        }
        Ok(Vec::new())
    }

    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

#[derive(Default)]
struct FailingRecoveryState {
    begins: usize,
    aborts: usize,
    staged: Vec<(String, Vec<u8>)>,
    fail_put: bool,
}

#[derive(Default)]
struct FailingRecoveryStore(Mutex<FailingRecoveryState>);

impl CheckpointSink for FailingRecoveryStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        self.0.lock().unwrap().begins += 1;
        Ok(test_transaction())
    }
    fn put_until(
        &self,
        _: NonZeroU64,
        name: &str,
        bytes: &[u8],
        _: std::time::Instant,
    ) -> Result<(), CompositionError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_put {
            state.fail_put = false;
            return Err(CompositionError::RuntimeConstruction);
        }
        state.staged.push((name.to_owned(), bytes.to_vec()));
        Ok(())
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        let mut state = self.0.lock().unwrap();
        state.aborts += 1;
        state.staged.clear();
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

impl CheckpointSource for FailingRecoveryStore {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Ok(Vec::new())
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

fn object_request(op: u32, stream: u64, generation: u32) -> protocol::Request {
    protocol::Request {
        op,
        stream,
        offset: 0,
        length: 0,
        name_size: 0,
        generation,
    }
}

#[derive(Default)]
struct TransactionState {
    committed: Vec<(String, Vec<u8>)>,
    staging: Vec<(String, Vec<u8>)>,
    aborts: usize,
    owner: Option<(NonZeroU64, std::time::Instant)>,
    next: u64,
}

#[derive(Default)]
struct TransactionStore {
    state: Mutex<TransactionState>,
}

impl TransactionStore {
    fn seed_committed(&self, name: &str, bytes: &[u8]) {
        self.state
            .lock()
            .unwrap()
            .committed
            .push((name.to_owned(), bytes.to_vec()));
    }

    fn snapshot(&self) -> (Vec<(String, Vec<u8>)>, Vec<(String, Vec<u8>)>, usize) {
        let state = self.state.lock().unwrap();
        (state.committed.clone(), state.staging.clone(), state.aborts)
    }

    fn expire_owner(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some((owner, _)) = state.owner {
            state.owner = Some((owner, std::time::Instant::now()));
        }
    }

    fn validate(
        state: &TransactionState,
        transaction: NonZeroU64,
        deadline: std::time::Instant,
    ) -> Result<(), CompositionError> {
        let now = std::time::Instant::now();
        match state.owner {
            Some((owner, lease)) if owner == transaction && now < lease && now < deadline => Ok(()),
            _ => Err(CompositionError::RuntimeConstruction),
        }
    }
}

impl CheckpointSink for TransactionStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, deadline: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        let mut state = self.state.lock().unwrap();
        if let Some((_, lease)) = state.owner {
            if std::time::Instant::now() < lease {
                return Err(CompositionError::TransactionBusy);
            }
            state.staging.clear();
            state.aborts += 1;
        }
        state.next = state.next.wrapping_add(1).max(1);
        let transaction = NonZeroU64::new(state.next).unwrap();
        state.owner = Some((transaction, deadline));
        Ok(transaction)
    }

    fn put_until(
        &self,
        transaction: NonZeroU64,
        name: &str,
        bytes: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CompositionError> {
        let mut state = self.state.lock().unwrap();
        Self::validate(&state, transaction, deadline)?;
        state.staging.push((name.to_owned(), bytes.to_vec()));
        Ok(())
    }

    fn abort_until(&self, transaction: NonZeroU64, deadline: std::time::Instant) -> Result<(), CompositionError> {
        let mut state = self.state.lock().unwrap();
        Self::validate(&state, transaction, deadline)?;
        state.staging.clear();
        state.aborts += 1;
        state.owner = None;
        Ok(())
    }

    fn commit_until(
        &self,
        transaction: NonZeroU64,
        _: &[u8],
        deadline: std::time::Instant,
    ) -> Result<(), CompositionError> {
        let mut state = self.state.lock().unwrap();
        Self::validate(&state, transaction, deadline)?;
        state.committed = std::mem::take(&mut state.staging);
        state.owner = None;
        Ok(())
    }
}

impl CheckpointSource for TransactionStore {
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

#[derive(Default)]
struct PanickingAbortStore(AtomicUsize);

impl CheckpointSink for PanickingAbortStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }

    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
            panic!("injected abort panic")
        } else {
            Ok(())
        }
    }

    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

impl CheckpointSource for PanickingAbortStore {
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

#[derive(Default)]
struct BlockingAbortStore {
    state: Mutex<(usize, bool)>,
    changed: Condvar,
}

impl BlockingAbortStore {
    fn wait_started(&self) {
        let mut state = self.state.lock().unwrap();
        while state.0 < 1 {
            state = self.changed.wait(state).unwrap();
        }
    }

    fn release(&self) {
        self.state.lock().unwrap().1 = true;
        self.changed.notify_all();
    }
}

impl CheckpointSink for BlockingAbortStore {
    fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }

    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }

    fn abort_until(&self, _: NonZeroU64, deadline: std::time::Instant) -> Result<(), CompositionError> {
        let mut state = self.state.lock().unwrap();
        state.0 += 1;
        self.changed.notify_all();
        while !state.1 {
            let now = std::time::Instant::now();
            if now >= deadline {
                return Err(CompositionError::DeadlineExceeded);
            }
            let (next, timeout) = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .unwrap();
            state = next;
            if timeout.timed_out() && !state.1 {
                return Err(CompositionError::DeadlineExceeded);
            }
        }
        Ok(())
    }

    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
}

impl CheckpointSource for BlockingAbortStore {
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
fn shared_sink_refuses_second_server_without_erasing_first_staging() {
    let store = Arc::new(TransactionStore::default());
    let first = Server::new(store.clone(), store.clone());
    let second = Server::new(store.clone(), store.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    first.begin_capture(1, deadline).unwrap();
    let transaction = first.transaction_token().unwrap();
    store.put_until(transaction, "first", b"owned", deadline).unwrap();

    let activated = std::cell::Cell::new(false);
    assert_eq!(
        second.begin_capture_after_admission(deadline, || {
            activated.set(true);
            2
        }),
        Err(CaptureFailure::Busy)
    );
    assert!(
        !activated.get(),
        "a rejected capture must not publish its trigger generation"
    );
    assert_eq!(store.snapshot().1, [("first".into(), b"owned".to_vec())]);

    first.discard_transaction(deadline).unwrap();
    second.begin_capture(2, deadline).unwrap();
}

#[test]
fn reclaimed_transaction_fences_stale_put_commit_and_abort() {
    let store = TransactionStore::default();
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    let stale = store.begin_until(deadline).unwrap();
    store.put_until(stale, "stale", b"one", deadline).unwrap();
    store.expire_owner();
    let current = store.begin_until(deadline).unwrap();
    store.put_until(current, "current", b"two", deadline).unwrap();

    assert!(store.put_until(stale, "late", b"bad", deadline).is_err());
    assert!(store.commit_until(stale, b"bad", deadline).is_err());
    assert!(store.abort_until(stale, deadline).is_err());
    assert_eq!(store.snapshot().1, [("current".into(), b"two".to_vec())]);
    store.commit_until(current, b"ok", deadline).unwrap();
    assert_eq!(store.snapshot().0, [("current".into(), b"two".to_vec())]);
}

#[test]
fn failed_capture_settles_server_and_storage_transaction_without_replacing_committed_image() {
    let store = Arc::new(TransactionStore::default());
    store.seed_committed("MANIFEST", b"prior");
    let server = Server::new(store.clone(), store.clone());
    let capture = server
        .begin_capture(17, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();

    let group_begin = object_request(protocol::GROUP_BEGIN, 0, 17);
    let group_commit = object_request(protocol::GROUP_COMMIT, 0, 17);
    let begin = object_request(protocol::OBJECT_BEGIN, 1, 17);
    let write = object_request(protocol::OBJECT_WRITE, 1, 17);
    let finish = object_request(protocol::OBJECT_FINISH, 1, 17);
    let claim = object_request(protocol::CLAIM, 0, 17);
    assert_eq!(
        server.dispatch(7, &group_begin, "proc.1", &[]).status,
        protocol::STATUS_OK
    );
    assert_eq!(
        server.dispatch(7, &begin, "proc.1/pages", &[]).status,
        protocol::STATUS_OK
    );
    assert_eq!(server.dispatch(7, &write, "", b"pages").status, protocol::STATUS_OK);
    assert_eq!(server.dispatch(7, &finish, "", &[]).status, protocol::STATUS_OK);
    assert_eq!(
        server.dispatch(7, &group_commit, "proc.1", &[]).status,
        protocol::STATUS_OK
    );

    assert_eq!(
        server.dispatch(7, &group_begin, "proc.2", &[]).status,
        protocol::STATUS_OK
    );
    assert_eq!(
        server.dispatch(7, &begin, "proc.2/open", &[]).status,
        protocol::STATUS_OK
    );
    assert_eq!(server.dispatch(7, &claim, "pipe.1", &[]).status, protocol::STATUS_OK);
    assert_eq!(server.transaction_state(), (1, 1, 1, 1, 1));
    assert_eq!(store.snapshot().1, [("proc.1/pages".into(), b"pages".to_vec())]);

    server.finish_failed(capture, CaptureFailure::Failed).unwrap();
    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Err(CaptureFailure::Failed))
    );

    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
    let (committed, staging, aborts) = store.snapshot();
    assert_eq!(committed, [("MANIFEST".into(), b"prior".to_vec())]);
    assert!(staging.is_empty());
    assert_eq!(aborts, 1);

    let retry = Server::new(store.clone(), store.clone());
    let retry_capture = retry
        .begin_capture(18, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let (committed, staging, aborts) = store.snapshot();
    assert_eq!(committed, [("MANIFEST".into(), b"prior".to_vec())]);
    assert!(staging.is_empty());
    assert_eq!(aborts, 1);
    retry.abort_capture(retry_capture).unwrap();
}

#[test]
fn one_rejected_process_aborts_the_whole_capture_before_manifest_publication() {
    let store = Arc::new(TransactionStore::default());
    store.seed_committed("MANIFEST", b"prior");
    store.seed_committed("proc.1/pages", b"prior-pages");
    let server = Server::new(store.clone(), store.clone());
    let capture = server
        .begin_capture(19, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();

    let group_begin = object_request(protocol::GROUP_BEGIN, 0, 19);
    let group_commit = object_request(protocol::GROUP_COMMIT, 0, 19);
    let group_abort = object_request(protocol::GROUP_ABORT, 0, 19);
    let begin = object_request(protocol::OBJECT_BEGIN, 1, 19);
    let write = object_request(protocol::OBJECT_WRITE, 1, 19);
    let finish = object_request(protocol::OBJECT_FINISH, 1, 19);
    let manifest = object_request(protocol::COMMIT, 0, 19);

    assert_eq!(
        server.dispatch(7, &group_begin, "proc.1", &[]).status,
        protocol::STATUS_OK
    );
    assert_eq!(
        server.dispatch(7, &begin, "proc.1/pages", &[]).status,
        protocol::STATUS_OK
    );
    assert_eq!(server.dispatch(7, &write, "", b"new-pages").status, protocol::STATUS_OK);
    assert_eq!(server.dispatch(7, &finish, "", &[]).status, protocol::STATUS_OK);
    assert_eq!(
        server.dispatch(7, &group_commit, "proc.1", &[]).status,
        protocol::STATUS_OK
    );

    assert_eq!(
        server.dispatch(8, &group_begin, "proc.2", &[]).status,
        protocol::STATUS_OK
    );
    for (stream, name, finish_object) in [
        (2, "proc.2/staged", true),
        (3, "proc.2/open", false),
        (4, "proc.20/staged", true),
        (5, "proc.20/open", false),
    ] {
        if name.starts_with("proc.20/") && stream == 4 {
            assert_eq!(
                server.dispatch(9, &group_begin, "proc.20", &[]).status,
                protocol::STATUS_OK
            );
        }
        let connection = if name.starts_with("proc.20/") { 9 } else { 8 };
        assert_eq!(
            server
                .dispatch(
                    connection,
                    &object_request(protocol::OBJECT_BEGIN, stream, 19),
                    name,
                    &[]
                )
                .status,
            protocol::STATUS_OK
        );
        assert_eq!(
            server
                .dispatch(
                    connection,
                    &object_request(protocol::OBJECT_WRITE, stream, 19),
                    "",
                    b"partial",
                )
                .status,
            protocol::STATUS_OK
        );
        if finish_object {
            assert_eq!(
                server
                    .dispatch(
                        connection,
                        &object_request(protocol::OBJECT_FINISH, stream, 19),
                        "",
                        &[]
                    )
                    .status,
                protocol::STATUS_OK
            );
        }
    }
    assert_eq!(
        server.dispatch(8, &group_abort, "proc.2", &[]).status,
        protocol::STATUS_ERROR,
        "a participant refusal must reject the whole capture"
    );
    {
        let state = server.state.lock().unwrap();
        assert!(!state.staged.contains_key("proc.2"));
        assert!(state.staged.contains_key("proc.20"));
        assert!(!state.open.values().any(|object| object.name.starts_with("proc.2/")));
        assert!(state.open.values().any(|object| object.name == "proc.20/open"));
    }
    assert_eq!(
        server.dispatch(7, &manifest, "", b"incomplete-manifest").status,
        protocol::STATUS_ERROR,
        "no manifest may cross a failed participant barrier"
    );
    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Err(CaptureFailure::Failed))
    );

    let (committed, staging, aborts) = store.snapshot();
    assert_eq!(
        committed,
        [
            ("MANIFEST".into(), b"prior".to_vec()),
            ("proc.1/pages".into(), b"prior-pages".to_vec()),
        ],
        "a rejected process must leave the prior generation authoritative"
    );
    assert!(staging.is_empty());
    assert_eq!(aborts, 1);
    assert_eq!(
        server.dispatch(8, &group_abort, "proc.2", &[]).status,
        protocol::STATUS_ERROR,
        "a late duplicate abort must remain rejected"
    );
    assert_eq!(store.snapshot().2, 1, "a late abort must not repeat storage cleanup");
}

#[test]
fn admitted_group_abort_blocks_concurrent_manifest_then_forces_rollback() {
    let store = Arc::new(TransactionStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let capture = server
        .begin_capture(20, std::time::Instant::now() + Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        server
            .dispatch(1, &object_request(protocol::GROUP_BEGIN, 0, 20), "proc.2", &[])
            .status,
        protocol::STATUS_OK
    );

    let held_state = server.state.lock().unwrap();
    let aborting = Arc::clone(&server);
    let abort =
        std::thread::spawn(move || aborting.dispatch(2, &object_request(protocol::GROUP_ABORT, 0, 20), "proc.2", &[]));
    let admission_deadline = std::time::Instant::now() + Duration::from_secs(1);
    while server.capture.lock().unwrap().mutations != 1 {
        assert!(std::time::Instant::now() < admission_deadline, "abort was not admitted");
        std::thread::yield_now();
    }
    let committing = Arc::clone(&server);
    let (sent, received) = mpsc::sync_channel(1);
    let commit = std::thread::spawn(move || {
        sent.send(committing.dispatch(1, &object_request(protocol::COMMIT, 0, 20), "", b"incomplete"))
            .unwrap();
    });
    assert!(
        received.recv_timeout(Duration::from_millis(20)).is_err(),
        "manifest crossed an admitted participant-abort barrier"
    );
    drop(held_state);
    assert_eq!(abort.join().unwrap().status, protocol::STATUS_ERROR);
    assert_eq!(
        received.recv_timeout(Duration::from_secs(1)).unwrap().status,
        protocol::STATUS_ERROR
    );
    commit.join().unwrap();
    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Err(CaptureFailure::Failed))
    );
    assert_eq!(store.snapshot().2, 1);
}

#[test]
fn group_abort_is_out_of_scope_during_idle_and_recovery() {
    let store = Arc::new(TransactionStore::default());
    let server = Server::new(store.clone(), store);
    assert_eq!(
        server
            .dispatch(1, &object_request(protocol::GROUP_ABORT, 0, 0), "proc.2", &[])
            .status,
        protocol::STATUS_ERROR
    );
    server
        .begin_recovery(21, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        server
            .dispatch(1, &object_request(protocol::GROUP_ABORT, 0, 21), "proc.2", &[])
            .status,
        protocol::STATUS_ERROR
    );
    assert_eq!(
        server.abort_recovery(21),
        Ok(()),
        "invalid abort must not poison recovery"
    );
}

#[test]
fn rejected_process_interrupts_checkpoint_channels_and_propagates_abort_panic() {
    let store = Arc::new(PanickingAbortStore::default());
    let server = Arc::new(Server::new(store.clone(), store));
    let (channel, peer) = UnixStream::pair().unwrap();
    let serving = Arc::clone(&server);
    let worker = std::thread::spawn(move || serving.serve(channel, 1));
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    let capture = server
        .begin_capture(22, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        server
            .dispatch(2, &object_request(protocol::GROUP_BEGIN, 0, 22), "proc.2", &[])
            .status,
        protocol::STATUS_OK
    );
    assert_eq!(
        server
            .dispatch(2, &object_request(protocol::GROUP_ABORT, 0, 22), "proc.2", &[])
            .status,
        protocol::STATUS_ERROR
    );
    assert_eq!(
        server.wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Poisoned)
    );
    drop(peer);
    worker.join().unwrap();
}

#[test]
fn failed_capture_waits_for_admitted_local_mutation_before_clearing_state() {
    let store = Arc::new(TransactionStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let capture = server
        .begin_capture(23, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let held_state = server.state.lock().unwrap();
    let request = object_request(protocol::CLAIM, 0, 23);
    let worker = Arc::clone(&server);
    let mutation = std::thread::spawn(move || worker.dispatch(7, &request, "pipe.race", &[]));
    let admission_deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        if server.capture.lock().unwrap().mutations == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < admission_deadline,
            "local state mutation never entered the transaction barrier"
        );
        std::thread::yield_now();
    }

    server.finish_failed(capture, CaptureFailure::Failed).unwrap();
    let waiter = Arc::clone(&server);
    let (settled, settlement) = mpsc::sync_channel(1);
    let wait = std::thread::spawn(move || {
        let result = waiter.wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1));
        settled.send(result).unwrap();
    });
    assert!(
        settlement.recv_timeout(Duration::from_millis(20)).is_err(),
        "failure became observable before the admitted mutation settled"
    );

    drop(held_state);
    assert_eq!(mutation.join().unwrap().status, protocol::STATUS_OK);
    assert_eq!(
        settlement.recv_timeout(Duration::from_secs(1)).unwrap().unwrap(),
        Some(Err(CaptureFailure::Failed))
    );
    wait.join().unwrap();
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

#[test]
fn dropped_mutation_admission_still_settles_and_clears_the_transaction() {
    let store = Arc::new(TransactionStore::default());
    let server = Server::new(store.clone(), store);
    let capture = server
        .begin_capture(29, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    server.state.lock().unwrap().claims.insert("pipe.cancelled".into());
    drop(server.admit_mutation().unwrap().unwrap());
    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Err(CaptureFailure::Failed))
    );
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

#[test]
fn panicking_storage_abort_cannot_strand_the_server_in_aborting() {
    let store = Arc::new(PanickingAbortStore::default());
    let server = Server::new(store.clone(), store);
    let capture = server
        .begin_capture(31, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    server.state.lock().unwrap().claims.insert("pipe.panic".into());
    server.finish_failed(capture, CaptureFailure::Failed).unwrap();
    assert_eq!(
        server.wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Poisoned)
    );
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

#[test]
fn failed_activation_cleanup_poisons_authority_and_propagates_abort_failure() {
    let store = Arc::new(PanickingAbortStore::default());
    let server = Server::new(store.clone(), store);
    let deadline = std::time::Instant::now() + Duration::from_secs(1);

    assert_eq!(
        server.begin_capture_after_admission(deadline, || 0),
        Err(CaptureFailure::Poisoned)
    );
    assert_eq!(
        server.begin_capture_after_admission(deadline, || 1),
        Err(CaptureFailure::Poisoned),
        "failed cleanup must not expose an apparently idle capture authority"
    );
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

#[test]
fn poisoned_server_state_is_recovered_and_cleared_during_abort() {
    let store = Arc::new(TransactionStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let capture = server
        .begin_capture(37, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let poisoned = Arc::clone(&server);
    assert!(
        std::thread::spawn(move || {
            let _state = poisoned.state.lock().unwrap();
            panic!("injected state poison")
        })
        .join()
        .is_err()
    );
    server.finish_failed(capture, CaptureFailure::Failed).unwrap();
    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Err(CaptureFailure::Failed))
    );
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

#[test]
fn concurrent_abort_callers_observe_cleanup_before_either_returns() {
    let store = Arc::new(BlockingAbortStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let capture = server
        .begin_capture(41, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    server.state.lock().unwrap().claims.insert("pipe.concurrent".into());
    let first = Arc::clone(&server);
    let one = std::thread::spawn(move || first.abort_capture(capture));
    store.wait_started();
    let second = Arc::clone(&server);
    let (returned, result) = mpsc::sync_channel(1);
    let two = std::thread::spawn(move || returned.send(second.abort_capture(capture)).unwrap());
    assert!(
        result.recv_timeout(Duration::from_millis(20)).is_err(),
        "concurrent abort returned while storage cleanup was blocked"
    );
    store.release();
    assert_eq!(one.join().unwrap(), Ok(()));
    assert_eq!(
        result.recv_timeout(Duration::from_secs(1)).unwrap(),
        Err(CaptureFailure::Poisoned)
    );
    two.join().unwrap();
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

fn publish_recovery(server: &Server, generation: u32, stream: u64) -> (i32, i32) {
    let begin = object_request(protocol::OBJECT_BEGIN, stream, generation);
    let finish = object_request(protocol::OBJECT_FINISH, stream, generation);
    (
        server.dispatch(7, &begin, "RECOVERY.jsonl", &[]).status,
        server.dispatch(7, &finish, "", &[]).status,
    )
}

#[test]
fn recovery_report_requires_and_closes_its_typed_scope() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store.clone());
    let recovery = server
        .begin_recovery(9, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let complete = object_request(protocol::RECOVERY_COMPLETE, 0, 9);
    let stale_complete = object_request(protocol::RECOVERY_COMPLETE, 0, 8);
    assert_ne!(server.dispatch(7, &complete, "", &[]).status, protocol::STATUS_OK);
    assert_ne!(server.dispatch(7, &stale_complete, "", &[]).status, protocol::STATUS_OK);
    assert_eq!(
        publish_recovery(&server, 9, 1),
        (protocol::STATUS_OK, protocol::STATUS_OK)
    );
    assert_eq!(
        store.0.lock().unwrap().as_slice(),
        &[("RECOVERY.jsonl".into(), Vec::new())]
    );
    assert_eq!(
        server.begin_capture(10, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Busy),
        "publishing the report is not proof that restore finished reading the image"
    );
    assert_eq!(server.dispatch(7, &complete, "", &[]).status, protocol::STATUS_OK);
    assert_eq!(server.wait_recovery(recovery), Ok(()));
    assert_ne!(server.dispatch(7, &complete, "", &[]).status, protocol::STATUS_OK);
    let stale_source = object_request(protocol::SOURCE_SIZE, 0, 9);
    assert_ne!(
        server.dispatch(7, &stale_source, "MANIFEST", &[]).status,
        protocol::STATUS_OK
    );
    assert_eq!(server.abort_recovery(recovery), Ok(()));
    assert!(
        server
            .begin_capture(10, std::time::Instant::now() + Duration::from_secs(1))
            .is_ok()
    );
}

#[test]
fn recovery_readiness_times_out_and_settles_the_transaction() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store);
    let recovery = server
        .begin_recovery(11, std::time::Instant::now() + Duration::from_millis(5))
        .unwrap();

    assert_eq!(server.wait_recovery(recovery), Err(CaptureFailure::Deadline));
    assert!(
        server
            .begin_recovery(12, std::time::Instant::now() + Duration::from_secs(1))
            .is_ok(),
        "a readiness timeout must release the abandoned recovery transaction"
    );
}

#[test]
fn failed_recovery_publication_aborts_staging_and_allows_immediate_retry() {
    let store = Arc::new(FailingRecoveryStore::default());
    store.0.lock().unwrap().fail_put = true;
    let server = Server::new(store.clone(), store.clone());
    let recovery = server
        .begin_recovery(31, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();

    assert_ne!(publish_recovery(&server, 31, 1).1, protocol::STATUS_OK);
    assert_eq!(server.wait_recovery(recovery), Err(CaptureFailure::Poisoned));
    {
        let state = store.0.lock().unwrap();
        assert_eq!((state.begins, state.aborts), (1, 1));
        assert!(state.staged.is_empty());
    }

    let retry = server
        .begin_recovery(32, std::time::Instant::now() + Duration::from_secs(1))
        .expect("a settled storage failure must not poison the next restore");
    server.abort_recovery(retry).unwrap();
    let state = store.0.lock().unwrap();
    assert_eq!((state.begins, state.aborts), (2, 2));
    assert!(state.staged.is_empty());
}

#[test]
fn idle_and_stale_scopes_cannot_publish_recovery_reports() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store.clone());
    assert_ne!(publish_recovery(&server, 0, 1).0, protocol::STATUS_OK);
    server
        .begin_recovery(4, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_ne!(publish_recovery(&server, 3, 2).0, protocol::STATUS_OK);
    assert_ne!(publish_recovery(&server, 5, 3).0, protocol::STATUS_OK);
    assert!(store.0.lock().unwrap().is_empty());
}

#[test]
fn recovery_and_capture_scopes_are_mutually_exclusive() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store);
    let recovery = server
        .begin_recovery(3, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let capture_activated = std::cell::Cell::new(false);
    assert_eq!(
        server.begin_capture_after_admission(std::time::Instant::now() + Duration::from_secs(1), || {
            capture_activated.set(true);
            4
        }),
        Err(CaptureFailure::Busy)
    );
    assert!(!capture_activated.get());
    server.abort_recovery(recovery).unwrap();
    server
        .begin_capture(4, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let recovery_activated = std::cell::Cell::new(false);
    assert_eq!(
        server.begin_recovery_after_admission(std::time::Instant::now() + Duration::from_secs(1), || {
            recovery_activated.set(true);
            5
        }),
        Err(CaptureFailure::Busy)
    );
    assert!(!recovery_activated.get());
}

#[test]
fn capture_readiness_waits_for_recovery_completion() {
    let store = Arc::new(RecoveryStore::default());
    let server = Arc::new(Server::new(store.clone(), store));
    let recovery = server
        .begin_recovery(21, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let recovering = Arc::clone(&server);
    let recovery_waiter = std::thread::spawn(move || recovering.wait_recovery(recovery));
    let waiting = Arc::clone(&server);
    let (sent, received) = mpsc::sync_channel(1);
    let waiter = std::thread::spawn(move || {
        sent.send(waiting.wait_capture_ready(std::time::Instant::now() + Duration::from_secs(1)))
            .unwrap();
    });
    assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
    assert_eq!(
        publish_recovery(&server, 21, 1),
        (protocol::STATUS_OK, protocol::STATUS_OK)
    );
    let complete = object_request(protocol::RECOVERY_COMPLETE, 0, 21);
    assert_eq!(server.dispatch(7, &complete, "", &[]).status, protocol::STATUS_OK);
    assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
    waiter.join().unwrap();
    assert_eq!(recovery_waiter.join().unwrap(), Ok(()));
}

#[test]
fn every_recovery_waiter_observes_the_same_terminal_result() {
    let store = Arc::new(RecoveryStore::default());
    let server = Arc::new(Server::new(store.clone(), store));
    let recovery = server
        .begin_recovery(23, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let one = Arc::clone(&server);
    let two = Arc::clone(&server);
    let first = std::thread::spawn(move || one.wait_recovery(recovery));
    let second = std::thread::spawn(move || two.wait_recovery(recovery));
    assert_eq!(
        publish_recovery(&server, 23, 1),
        (protocol::STATUS_OK, protocol::STATUS_OK)
    );
    let complete = object_request(protocol::RECOVERY_COMPLETE, 0, 23);
    assert_eq!(server.dispatch(7, &complete, "", &[]).status, protocol::STATUS_OK);
    assert_eq!(first.join().unwrap(), Ok(()));
    assert_eq!(second.join().unwrap(), Ok(()));
}

#[test]
fn delayed_recovery_waiter_keeps_its_result_across_new_capture_admission() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store);
    let recovery = server
        .begin_recovery(29, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    server.fail_recovery(recovery, CaptureFailure::Failed).unwrap();

    assert_eq!(server.wait_recovery(recovery), Err(CaptureFailure::Failed));
    let capture = server
        .begin_capture(30, std::time::Instant::now() + Duration::from_secs(1))
        .expect("completed recovery ownership must not block the next capture");

    assert_eq!(
        server.wait_recovery(recovery),
        Err(CaptureFailure::Failed),
        "new capture admission replaced the completed recovery result with Busy"
    );
    server.abort_capture(capture).unwrap();
}

#[test]
fn reused_recovery_generation_cannot_observe_its_previous_result() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store);
    let first = server
        .begin_recovery(29, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    server.fail_recovery(first, CaptureFailure::Failed).unwrap();
    assert_eq!(server.wait_recovery(first), Err(CaptureFailure::Failed));

    let reused = server
        .begin_recovery(29, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    server.fail_recovery(reused, CaptureFailure::Deadline).unwrap();
    assert_eq!(server.wait_recovery(reused), Err(CaptureFailure::Deadline));
}

#[test]
fn every_capture_waiter_observes_the_same_terminal_error() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store);
    let capture = server
        .begin_capture(24, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    server.finish_failed(capture, CaptureFailure::Failed).unwrap();
    let wake = std::time::Instant::now() + Duration::from_secs(1);
    assert_eq!(
        server.wait_capture(capture, wake),
        Ok(Some(Err(CaptureFailure::Failed)))
    );
    assert_eq!(
        server.wait_capture(capture, wake),
        Ok(Some(Err(CaptureFailure::Failed)))
    );
}

#[test]
fn poisoned_recovery_coordination_discards_and_reopens_cleanly() {
    let store = Arc::new(FailingRecoveryStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let recovery = server
        .begin_recovery(25, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let poison = Arc::clone(&server);
    let _ = std::thread::spawn(move || {
        let _held = poison.capture.lock().unwrap();
        panic!("intentional recovery coordination poison");
    })
    .join();

    assert_eq!(server.wait_recovery(recovery), Err(CaptureFailure::Poisoned));
    server.abort_recovery(recovery).unwrap();
    let retry = server
        .begin_recovery(26, std::time::Instant::now() + Duration::from_secs(1))
        .expect("settled mutex poison must permit a defined retry");
    server.abort_recovery(retry).unwrap();
    let state = store.0.lock().unwrap();
    assert_eq!((state.begins, state.aborts), (2, 2));
    assert!(state.staged.is_empty());
}

#[test]
fn capture_readiness_bounds_an_incomplete_recovery() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store);
    server
        .begin_recovery(22, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        server.wait_capture_ready(std::time::Instant::now() + Duration::from_millis(5)),
        Err(CaptureFailure::Deadline)
    );
}

#[test]
fn recovery_complete_closes_mutation_admission_before_storage_cleanup() {
    let store = Arc::new(BlockingAbortStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    server
        .begin_recovery(12, std::time::Instant::now() + Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        publish_recovery(&server, 12, 1),
        (protocol::STATUS_OK, protocol::STATUS_OK)
    );
    let complete = object_request(protocol::RECOVERY_COMPLETE, 0, 12);
    let completing = Arc::clone(&server);
    let worker = std::thread::spawn(move || completing.dispatch(1, &complete, "", &[]));
    store.wait_started();

    let late = object_request(protocol::OBJECT_BEGIN, 2, 12);
    assert_ne!(server.dispatch(1, &late, "late", &[]).status, protocol::STATUS_OK);
    store.release();
    assert_eq!(worker.join().unwrap().status, protocol::STATUS_OK);
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

#[test]
fn abort_recovery_waits_for_admitted_mutation_then_releases_transaction() {
    let store = Arc::new(MutationPublicationRace::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    server
        .begin_recovery(13, std::time::Instant::now() + Duration::from_secs(2))
        .unwrap();
    server.state.lock().unwrap().open.insert(
        (1, 1),
        super::Object {
            name: "RECOVERY.jsonl".into(),
            bytes: vec![1],
        },
    );
    let mutating = Arc::clone(&server);
    let mutation = std::thread::spawn(move || mutating.dispatch(1, &finish_request(1, 13), "", &[]));
    store.wait_mutations(1);
    let aborting = Arc::clone(&server);
    let (sent, received) = mpsc::sync_channel(1);
    let abort = std::thread::spawn(move || sent.send(aborting.abort_recovery(13)).unwrap());
    assert!(received.recv_timeout(Duration::from_millis(20)).is_err());
    store.release_mutations(false);
    let _ = mutation.join().unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), Ok(()));
    abort.join().unwrap();
    assert_eq!(server.transaction_state(), (0, 0, 0, 0, 0));
}

#[test]
fn expired_recovery_scope_rejects_publication() {
    let store = Arc::new(RecoveryStore::default());
    let server = Server::new(store.clone(), store.clone());
    server
        .begin_recovery(6, std::time::Instant::now() + Duration::from_millis(5))
        .unwrap();
    std::thread::sleep(Duration::from_millis(10));
    assert_ne!(publish_recovery(&server, 6, 1).0, protocol::STATUS_OK);
    assert!(store.0.lock().unwrap().is_empty());
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
fn last_checkpoint_channel_exit_fails_an_active_capture_immediately() {
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let (channel, peer) = UnixStream::pair().unwrap();
    let worker = Arc::clone(&server);
    let serving = std::thread::spawn(move || worker.serve(channel, 1));
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    let capture = server
        .begin_capture(1, std::time::Instant::now() + Duration::from_secs(5))
        .unwrap();

    drop(peer);

    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Err(CaptureFailure::Failed))
    );
    serving.join().unwrap();
    assert_eq!(server.connections.load(Ordering::Acquire), 0);
}

#[test]
fn last_checkpoint_channel_exit_aborts_recovery_immediately() {
    let store = Arc::new(FailingRecoveryStore::default());
    let server = Arc::new(Server::new(store.clone(), store.clone()));
    let recovery = server
        .begin_recovery(41, std::time::Instant::now() + Duration::from_secs(5))
        .unwrap();
    let (channel, peer) = UnixStream::pair().unwrap();
    let worker = Arc::clone(&server);
    let serving = std::thread::spawn(move || worker.serve(channel, 1));
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }

    drop(peer);

    assert_eq!(server.wait_recovery(recovery), Err(CaptureFailure::Failed));
    serving.join().unwrap();
    let state = store.0.lock().unwrap();
    assert_eq!((state.begins, state.aborts), (1, 1));
    assert!(state.staged.is_empty());
}

#[test]
fn an_accepted_unscheduled_channel_prevents_a_false_last_channel_failure() {
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let active = super::broker::AcceptedChannel::new(Arc::clone(&server));
    let accepted_not_scheduled = super::broker::AcceptedChannel::new(Arc::clone(&server));
    let capture = server
        .begin_capture(1, std::time::Instant::now() + Duration::from_secs(5))
        .unwrap();

    drop(active);

    assert_eq!(server.connections.load(Ordering::Acquire), 1);
    assert_eq!(
        server.wait_capture(capture, std::time::Instant::now()).unwrap(),
        None,
        "an accepted channel must count before its worker gets scheduled"
    );

    drop(accepted_not_scheduled);
    assert_eq!(
        server
            .wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1))
            .unwrap(),
        Some(Err(CaptureFailure::Failed))
    );
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
    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
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
    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
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
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        self.state.lock().unwrap().3 += 1;
        Ok(())
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
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

    fn begin_until(&self, _: std::time::Instant) -> Result<NonZeroU64, CompositionError> {
        Ok(test_transaction())
    }
    fn put_until(&self, _: NonZeroU64, _: &str, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn abort_until(&self, _: NonZeroU64, _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }

    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
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

#[test]
fn capture_completion_wait_expires_at_the_deadline_instead_of_re_interrupting_forever() {
    let store = Arc::new(RecoveryStore::default());
    let server = Arc::new(Server::new(store.clone(), store));
    // The server's own capture deadline is far away; only the completion loop's deadline can end
    // this wait, which is exactly the guarantee a stalled guest dump depends on.
    let capture = server
        .begin_capture(41, std::time::Instant::now() + Duration::from_secs(3600))
        .unwrap();
    let (sent, received) = mpsc::channel();
    let waiter = server.clone();
    std::thread::spawn(move || {
        let interrupts = AtomicUsize::new(0);
        let outcome = super::super::execution::await_capture_completion(
            &waiter,
            capture,
            std::time::Instant::now() + Duration::from_millis(250),
            || {
                interrupts.fetch_add(1, Ordering::Relaxed);
            },
        );
        let _ = sent.send(outcome);
    });
    let outcome = received
        .recv_timeout(Duration::from_secs(10))
        .expect("completion wait must return at its deadline");
    assert_eq!(outcome, Err(CaptureFailure::Deadline));
}
