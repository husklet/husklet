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
        // SAFETY: the child is single-threaded and this is its first action.
        unsafe { bound_this_fork_child() };
        // SAFETY: child owns its inherited ends and terminates with _exit.
        unsafe {
            libc::close(release[1]);
        }
        // SAFETY: this is a fork child of a multi-threaded binary, so the channel must be
        // announced without taking any lock the fork may have copied in a held state.
        let channel = unsafe { transport.connect_in_forked_child_for_test() };
        // SAFETY: no Rust destructors are run after fork.
        if channel < 0 {
            unsafe { libc::_exit(90) }
        }
        send_descriptor(&relay_child, channel);
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
    server.await_accepts(1);
    let mut request = [0_u8; protocol::REQUEST_BYTES];
    request[0..4].copy_from_slice(&protocol::MAGIC_REQUEST.to_ne_bytes());
    request[4..8].copy_from_slice(&protocol::ABI.to_ne_bytes());
    request[8..12].copy_from_slice(&protocol::GROUP_PRESENT.to_ne_bytes());
    // The revoked worker may close the channel before this write lands, so EPIPE is a
    // legitimate outcome of the very revocation under test.
    let _ = survivor.write_all(&request);
    completed
        .recv_timeout(Duration::from_secs(1))
        .expect("revoked connection must terminate");
    assert_eq!(server.connections.load(Ordering::Acquire), 0);
    assert_eq!(server.dispatch_count(), 0, "revoked peer request reached dispatch");
}

/// Seconds a fork child of this module may live before the kernel ends it.
const FORK_CHILD_SECONDS: u32 = 30;

/// Makes a fork child of this module incapable of outliving the test.
///
/// Wedged checkpoint peers were orphaned to init and sat for tens of minutes
/// holding a whole lane's box time, indistinguishable from a slow build.
/// `PR_SET_PDEATHSIG` ends the child the moment the test process dies, and the
/// alarm ends it unconditionally: the default `SIGALRM` disposition terminates,
/// so the parent's `waitpid` observes a signalled child and the assertion names
/// the test instead of hanging.
///
/// # Safety
///
/// Call only as the first action of a `fork()` child, which is single-threaded.
unsafe fn bound_this_fork_child() {
    #[cfg(target_os = "linux")]
    // SAFETY: prctl with PR_SET_PDEATHSIG touches no Rust storage and cannot unwind.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL)
    };
    // SAFETY: alarm takes no pointer, touches no Rust storage, and cannot unwind.
    unsafe { libc::alarm(FORK_CHILD_SECONDS) };
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

/// Longest a relayed descriptor may take to arrive before the test fails by name.
const RELAY_DEADLINE_MS: i16 = 10_000;

fn receive_descriptor(channel: &UnixStream) -> UnixStream {
    let mut byte = 0_u8;
    let mut vector = libc::iovec {
        iov_base: (&raw mut byte).cast(),
        iov_len: 1,
    };
    let mut control = [0_u8; 64];
    let mut ready = libc::pollfd {
        fd: channel.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // A peer that never sends must fail this test by name. An unbounded recvmsg here is what
    // turned a wedged fork child into an hour-long silent stall for every lane on the box.
    // SAFETY: ready addresses one writable pollfd naming a live descriptor.
    let waited = unsafe { libc::poll(&raw mut ready, 1, i32::from(RELAY_DEADLINE_MS)) };
    assert_eq!(waited, 1, "relayed checkpoint descriptor never arrived");
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

/// Byte store that offers whichever object names it is told to, so recovery can
/// be asked to admit a generation that the storage transaction never committed.
struct OfferedGeneration(Vec<String>);

impl CheckpointSink for OfferedGeneration {
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
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
}

impl CheckpointSource for OfferedGeneration {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Err(CompositionError::RuntimeConstruction)
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Ok(self.0.clone())
    }
}

/// The byte store is adversarial and its committed-generation pointer is data,
/// not authority. A staged generation carries its objects but no manifest, so
/// recovery must refuse it before native restore can read a single one of them.
#[test]
fn recovery_refuses_a_prepared_generation_and_admits_a_finalized_one() {
    let staged = Arc::new(OfferedGeneration(vec![
        String::from("proc.1/pages"),
        String::from("proc.1/state"),
    ]));
    let server = Server::new(staged.clone(), staged);
    assert_eq!(
        server.begin_recovery(7, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Unfinalized)
    );

    let finalized = Arc::new(OfferedGeneration(vec![
        String::from("MANIFEST"),
        String::from("proc.1/pages"),
        String::from("proc.1/state"),
    ]));
    let server = Server::new(finalized.clone(), finalized);
    assert_eq!(
        server.begin_recovery(7, std::time::Instant::now() + Duration::from_secs(1)),
        Ok(7)
    );
}

/// A generation with nothing in it proves nothing about having been committed,
/// and a store that cannot answer proves less still. Both fail closed.
#[test]
fn recovery_refuses_an_empty_generation() {
    let empty = Arc::new(OfferedGeneration(Vec::new()));
    let server = Server::new(empty.clone(), empty);
    assert_eq!(
        server.begin_recovery(7, std::time::Instant::now() + Duration::from_secs(1)),
        Err(CaptureFailure::Unfinalized)
    );
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
        // A store that offers a generation for recovery must present it as
        // finalized: recovery refuses a generation carrying no manifest.
        Ok(vec![String::from("MANIFEST")])
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
        Ok(vec![String::from("MANIFEST")])
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
        Ok(vec![String::from("MANIFEST")])
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
        Ok(vec![String::from("MANIFEST")])
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
        Ok(vec![String::from("MANIFEST")])
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
    server.await_accepts(1);
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
        Ok(vec![String::from("MANIFEST")])
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
    let registration_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while server.channels.lock().unwrap().is_empty() {
        assert!(
            std::time::Instant::now() < registration_deadline,
            "silent checkpoint peer was never registered"
        );
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
    server.await_accepts(1);
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
    server.await_accepts(1);

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
        Ok(vec![String::from("MANIFEST")])
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

/// Serializes every test that mints a real checkpoint transport.
///
/// These tests authenticate this process against its own channel, and their broker worker threads own
/// descriptors. Two such tests overlapping -- or one outliving its test body -- lets a worker close a
/// descriptor number a later test has already reused, which aborts the whole binary on an IO-safety
/// violation rather than failing a test. So the lock is shared by all of them, and each joins its
/// worker before returning.
static TRANSPORT_SERIAL: Mutex<()> = Mutex::new(());

fn checkpoint_request(op: u32, generation: u32, payload: &[u8]) -> Vec<u8> {
    let mut request = vec![0_u8; protocol::REQUEST_BYTES];
    request[0..4].copy_from_slice(&protocol::MAGIC_REQUEST.to_ne_bytes());
    request[4..8].copy_from_slice(&protocol::ABI.to_ne_bytes());
    request[8..12].copy_from_slice(&op.to_ne_bytes());
    request[32..40].copy_from_slice(&(payload.len() as u64).to_ne_bytes());
    request[44..48].copy_from_slice(&generation.to_ne_bytes());
    request.extend_from_slice(payload);
    request
}

fn register_ready_payload(executors: &[u32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8 + executors.len() * 4);
    payload.extend_from_slice(&(executors.len() as u32).to_ne_bytes());
    payload.extend_from_slice(&0_u32.to_ne_bytes());
    for executor in executors {
        payload.extend_from_slice(&executor.to_ne_bytes());
    }
    payload
}

/// Reads one reply and returns its `(status, value)`.
fn read_reply(channel: &mut UnixStream) -> (i32, u64) {
    let mut reply = [0_u8; 32];
    std::io::Read::read_exact(channel, &mut reply).expect("checkpoint reply");
    (
        i32::from_ne_bytes(reply[8..12].try_into().unwrap()),
        u64::from_ne_bytes(reply[16..24].try_into().unwrap()),
    )
}

/// An authenticated engine process becomes a member of the running capture
/// exactly once; the repeat is a duplicate, not a second member.
///
/// The peer is this test process itself: the broker authenticates whoever
/// connects, so a real capability is available without forking a peer out of a
/// multi-threaded test binary, which deadlocks the child against a lock another
/// thread held at fork time.
#[test]
fn register_ready_admits_one_authenticated_process_exactly_once() {
    let _serial = TRANSPORT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (broker, transport) = hl_native::CheckpointTransport::create().expect("checkpoint transport");
    let mut member = transport.connect_for_test().expect("checkpoint channel");
    let (channel, authority) = broker
        .accept(Duration::from_secs(10))
        .expect("authenticated production accept");
    assert_eq!(authority.host_pid, u64::from(std::process::id()));
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let worker = Arc::clone(&server);
    let served = std::thread::spawn(move || worker.serve_authenticated_for_test(channel, authority));
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    server
        .begin_capture(7, std::time::Instant::now() + Duration::from_secs(30))
        .expect("capture admission");

    let registration = checkpoint_request(protocol::REGISTER_READY, 7, &register_ready_payload(&[101, 102]));
    member.write_all(&registration).expect("member registration");
    let (status, id) = read_reply(&mut member);
    assert_eq!(status, 0, "authenticated member was refused");
    assert_ne!(id, 0, "member registration returned no member ID");

    member.write_all(&registration).expect("duplicate registration");
    assert_eq!(read_reply(&mut member).0, -1, "one process registered twice");
    server.stop();
    drop(member);
    served.join().expect("broker worker");
}

fn member_restored_payload(guest_pid: i32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8);
    payload.extend_from_slice(&guest_pid.to_ne_bytes());
    payload.extend_from_slice(&0_u32.to_ne_bytes());
    payload
}

fn member_exited_payload(status: i32, kind: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(8);
    payload.extend_from_slice(&status.to_ne_bytes());
    payload.extend_from_slice(&kind.to_ne_bytes());
    payload
}

/// A restored member becomes individually reachable by the guest pid its image names it by, and the
/// capability it is reached through is the authenticated peer of its own channel.
///
/// This is what a whole-image restore otherwise cannot offer: one launch produces a tree, and without
/// this the host can only address the tree. Reaching one member is the difference between attaching a
/// pane to the process the user left running and starting their command a second time.
#[test]
fn a_restored_member_is_reachable_by_the_guest_pid_its_image_names_it_by() {
    let _serial = TRANSPORT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (broker, transport) = hl_native::CheckpointTransport::create().expect("checkpoint transport");
    let mut member = transport.connect_for_test().expect("checkpoint channel");
    let (channel, authority) = broker
        .accept(Duration::from_secs(10))
        .expect("authenticated production accept");
    let host_pid = authority.host_pid;
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let guest_pid = std::num::NonZeroI32::new(4242).expect("guest pid");
    assert!(
        server.restored_member(guest_pid).is_none(),
        "a member was reachable before any restore announced it"
    );
    // Recovery is entered before the member's channel is served, exactly as a restore does it: the
    // channel binds to the running recovery as it is accepted, which is what lets a restored member
    // address the broker with the restore's own generation.
    server
        .begin_recovery(1, std::time::Instant::now() + Duration::from_secs(30))
        .expect("recovery admission");
    let worker = Arc::clone(&server);
    let served = std::thread::spawn(move || worker.serve_authenticated_for_test(channel, authority));
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }

    member
        .write_all(&checkpoint_request(
            protocol::MEMBER_RESTORED,
            0,
            &member_restored_payload(guest_pid.get()),
        ))
        .expect("member announcement");
    assert_eq!(read_reply(&mut member).0, 0, "an authenticated member was refused");

    let restored = server
        .restored_member(guest_pid)
        .expect("announced member is reachable");
    assert_eq!(restored.guest_pid(), guest_pid);
    assert!(restored.is_live(), "the announcing process is live");
    assert_eq!(restored.exit(), None, "a live member has not exited");
    // Signal 0 proves the capability reaches this exact incarnation without disturbing it.
    assert!(
        restored.signal(0).is_ok(),
        "the member capability cannot reach its process"
    );

    member
        .write_all(&checkpoint_request(
            protocol::MEMBER_EXITED,
            0,
            &member_exited_payload(7, 1),
        ))
        .expect("member exit report");
    assert_eq!(read_reply(&mut member).0, 0, "a member could not report its own exit");
    assert_eq!(
        server.restored_member(guest_pid).expect("member").exit(),
        Some(crate::runtime::MemberExit::Code(7)),
        "the reported exit status was not the one the member produced"
    );
    assert_eq!(host_pid, u64::from(std::process::id()));
    server.stop();
    drop(member);
    served.join().expect("broker worker");
}

/// Only a running restore may name a restored member.
///
/// The capability an announcement installs is a reach into a live process, so it must come from the
/// one event that genuinely produces members. An authenticated channel announcing outside a recovery
/// is a process claiming to be something no restore re-forked.
#[test]
fn an_announcement_outside_a_running_restore_installs_no_member() {
    let _serial = TRANSPORT_SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (broker, transport) = hl_native::CheckpointTransport::create().expect("checkpoint transport");
    let mut member = transport.connect_for_test().expect("checkpoint channel");
    let (channel, authority) = broker
        .accept(Duration::from_secs(10))
        .expect("authenticated production accept");
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let worker = Arc::clone(&server);
    let served = std::thread::spawn(move || worker.serve_authenticated_for_test(channel, authority));
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    let guest_pid = std::num::NonZeroI32::new(31).expect("guest pid");

    member
        .write_all(&checkpoint_request(
            protocol::MEMBER_RESTORED,
            0,
            &member_restored_payload(guest_pid.get()),
        ))
        .expect("member announcement");

    assert_eq!(read_reply(&mut member).0, -1, "an announcement outside a restore was admitted");
    assert!(
        server.restored_member(guest_pid).is_none(),
        "an announcement outside a restore installed a member capability"
    );
    server.stop();
    drop(member);
    served.join().expect("broker worker");
}

/// A member stays reachable after the connection that announced it is gone.
///
/// The announcement is a one-shot event on a channel the restore tears down; reachability has to
/// outlive it, because a host reattaches long after the restore has finished. The registry therefore
/// holds its own duplicate of the authenticated capability rather than borrowing the connection's.
#[test]
fn a_restored_member_outlives_the_connection_that_announced_it() {
    let _serial = TRANSPORT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (broker, transport) = hl_native::CheckpointTransport::create().expect("checkpoint transport");
    let mut member = transport.connect_for_test().expect("checkpoint channel");
    let (channel, authority) = broker
        .accept(Duration::from_secs(10))
        .expect("authenticated production accept");
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let guest_pid = std::num::NonZeroI32::new(909).expect("guest pid");
    server
        .begin_recovery(1, std::time::Instant::now() + Duration::from_secs(30))
        .expect("recovery admission");
    let worker = Arc::clone(&server);
    let served = std::thread::spawn(move || worker.serve_authenticated_for_test(channel, authority));
    while server.connections.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }
    member
        .write_all(&checkpoint_request(
            protocol::MEMBER_RESTORED,
            0,
            &member_restored_payload(guest_pid.get()),
        ))
        .expect("member announcement");
    assert_eq!(read_reply(&mut member).0, 0);
    assert!(server.restored_member(guest_pid).is_some());
    // Settling the restore drops every channel it was serving. The member must stay reachable across
    // that, because the registry holds its own duplicate of the authenticated capability: reachability
    // is a property of the process, not of the connection that announced it.
    server
        .fail_recovery(1, CaptureFailure::Failed)
        .expect("recovery settlement");
    assert!(
        server.restored_member(guest_pid).is_some(),
        "the member stopped being reachable when the connection that announced it closed"
    );

    assert!(
        server.restored_member(guest_pid).expect("member").is_live(),
        "the retained capability no longer names the live announcing process"
    );
    server.stop();
    drop(member);
    served.join().expect("broker worker");
}

/// Membership is proven, never assumed: a channel carrying no authenticated
/// process capability is not a member and cannot become one, and a channel that
/// publishes without registering fails the capture by name instead of leaving it
/// to expire at its deadline.
#[test]
fn an_unregistered_channel_cannot_publish_and_fails_the_capture_by_name() {
    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let (mut claimant, claimant_channel) = UnixStream::pair().expect("claimant channel");
    let (mut publisher, publisher_channel) = UnixStream::pair().expect("publisher channel");
    for (channel, id) in [(claimant_channel, 11_u64), (publisher_channel, 12)] {
        let worker = Arc::clone(&server);
        std::thread::spawn(move || worker.serve(channel, id));
    }
    while server.connections.load(Ordering::Acquire) != 2 {
        std::thread::yield_now();
    }
    let capture = server
        .begin_capture(7, std::time::Instant::now() + Duration::from_secs(30))
        .expect("capture admission");

    claimant
        .write_all(&checkpoint_request(
            protocol::REGISTER_READY,
            7,
            &register_ready_payload(&[101]),
        ))
        .expect("unauthenticated registration");
    assert_eq!(
        read_reply(&mut claimant).0,
        -1,
        "a channel with no process capability registered as a member"
    );

    publisher
        .write_all(&checkpoint_request(protocol::GROUP_BEGIN, 7, &[]))
        .expect("unregistered publication");
    assert_eq!(
        server.wait_capture(capture, std::time::Instant::now() + Duration::from_secs(10)),
        Ok(Some(Err(CaptureFailure::Failed))),
        "an unregistered publisher must fail the capture, not exhaust its deadline"
    );
    server.stop();
}

// =============================== park and release (plan step 4) ===============================
//
// The property under test is the one the whole freeze rests on and the one today's `_exit` peer arm
// cannot provide: TWO members simultaneously STOPPED AND ALIVE across ONE shared-object capture, and
// then both RESUMED. "Stopped" is proved by the member being inside its park loop and nowhere else;
// "alive" is proved by its park heartbeat still arriving AFTER the other member has finished draining
// the shared object; "resumed" is proved by each member leaving the park and running again.
//
// The members are real forked processes on real authenticated channels against the real Server, because
// the property is about two host processes existing at the same instant. A mock cannot state it.

const PARK_EVENT_PROGRESSING: u8 = b'P';
const PARK_EVENT_REGISTERED: u8 = b'R';
const PARK_EVENT_HEARTBEAT: u8 = b'H';
const PARK_EVENT_DRAINED: u8 = b'D';
const PARK_EVENT_RESUMED: u8 = b'S';
const PARK_EVENT_EXIT_DISPOSITION: u8 = b'X';
const PARK_SHARED_OBJECT_BYTES: usize = 16;

fn park_pipe() -> [i32; 2] {
    let mut ends = [-1; 2];
    // SAFETY: ends names writable storage for two new descriptors.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    ends
}

/// One request/response round trip written by hand, exactly as the engine's C client frames it.
/// Fork-safe: no allocation, no Rust synchronization, only the inherited descriptor.
fn park_call(channel: i32, op: u32, name: &[u8], payload: &[u8], generation: u32) -> Option<(i32, u64)> {
    let mut request = [0_u8; protocol::REQUEST_BYTES];
    request[0..4].copy_from_slice(&protocol::MAGIC_REQUEST.to_ne_bytes());
    request[4..8].copy_from_slice(&protocol::ABI.to_ne_bytes());
    request[8..12].copy_from_slice(&op.to_ne_bytes());
    request[32..40].copy_from_slice(&(payload.len() as u64).to_ne_bytes());
    request[40..44].copy_from_slice(&(name.len() as u32).to_ne_bytes());
    request[44..48].copy_from_slice(&generation.to_ne_bytes());
    park_write(channel, &request)?;
    if !name.is_empty() {
        park_write(channel, name)?;
    }
    if !payload.is_empty() {
        park_write(channel, payload)?;
    }
    let mut reply = [0_u8; 32];
    park_read(channel, &mut reply)?;
    let status = i32::from_ne_bytes(reply[8..12].try_into().ok()?);
    let value = u64::from_ne_bytes(reply[16..24].try_into().ok()?);
    Some((status, value))
}

fn park_write(descriptor: i32, bytes: &[u8]) -> Option<()> {
    let mut written = 0;
    while written < bytes.len() {
        // SAFETY: the slice is live for the call and the descriptor is owned by this process.
        let count = unsafe { libc::write(descriptor, bytes[written..].as_ptr().cast(), bytes.len() - written) };
        if count <= 0 {
            return None;
        }
        written += count as usize;
    }
    Some(())
}

fn park_read(descriptor: i32, bytes: &mut [u8]) -> Option<()> {
    let mut read = 0;
    while read < bytes.len() {
        // SAFETY: the slice is live and writable for the call.
        let count = unsafe { libc::read(descriptor, bytes[read..].as_mut_ptr().cast(), bytes.len() - read) };
        if count <= 0 {
            return None;
        }
        read += count as usize;
    }
    Some(())
}

/// Closes the parent's copy of a descriptor the members own from here on.
fn park_close(descriptor: i32) {
    // SAFETY: the parent has no further use for this inherited end.
    unsafe { libc::close(descriptor) };
}

fn park_emit(events: i32, tag: u8) {
    let byte = [tag];
    // SAFETY: a one-byte write to an inherited pipe end this process owns.
    unsafe { libc::write(events, byte.as_ptr().cast(), 1) };
}

/// Waits for a byte with `tag`, and fails the test rather than hanging forever: the event pipe is
/// non-blocking, so a member that dies -- which is exactly what the non-vacuity mutation makes every
/// member do -- is reported as a dead member instead of stalling the suite.
fn park_await(events: i32, tag: u8, what: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let mut byte = [0_u8; 1];
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        // SAFETY: byte is writable and the descriptor is owned here.
        let count = unsafe { libc::read(events, byte.as_mut_ptr().cast(), 1) };
        if count < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        }
        assert!(count == 1, "member exited without reaching {what}");
        if byte[0] == tag {
            return;
        }
        assert_ne!(
            byte[0], PARK_EVENT_EXIT_DISPOSITION,
            "member was released to EXIT, not {what}"
        );
    }
}

/// The park loop, written exactly as the engine's C park is: poll `RELEASE_WAIT`, do nothing else, and
/// treat a transport failure as RESUME so a dead broker can never leave this process frozen.
fn park_member(channel: i32, events: i32, generation: u32) -> u64 {
    loop {
        let state = match park_call(channel, protocol::RELEASE_WAIT, &[], &[], generation) {
            Some((protocol::STATUS_OK, value)) => value,
            _ => protocol::RELEASE_RESUME,
        };
        if state != protocol::RELEASE_HOLD {
            return state;
        }
        park_emit(events, PARK_EVENT_HEARTBEAT);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn park_register_ready(channel: i32, generation: u32, executor: u32) -> bool {
    let mut payload = [0_u8; 12];
    payload[0..4].copy_from_slice(&1_u32.to_ne_bytes());
    payload[8..12].copy_from_slice(&executor.to_ne_bytes());
    matches!(
        park_call(channel, protocol::REGISTER_READY, &[], &payload, generation),
        Some((protocol::STATUS_OK, member)) if member != 0
    )
}

#[test]
fn two_members_stay_stopped_and_alive_across_one_shared_object_capture_and_then_resume() {
    static SERIAL: Mutex<()> = Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    const GENERATION: u32 = 7;

    let (broker, transport) = hl_native::CheckpointTransport::create().expect("checkpoint transport");
    // The shared object both members hold: one end each, bytes in flight, captured destructively by
    // whichever member wins the image-wide claim.
    let shared = park_pipe();
    let events = [park_pipe(), park_pipe()];
    let start = park_pipe();
    let gate = park_pipe();
    assert_eq!(
        park_write(shared[1], &[0xab; PARK_SHARED_OBJECT_BYTES]),
        Some(()),
        "seed the shared object"
    );

    let mut members = [0; 2];
    for (index, member) in members.iter_mut().enumerate() {
        // SAFETY: the child touches only inherited descriptors and terminates with _exit, so no Rust
        // destructor, lock, or allocator state crosses the fork.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork checkpoint member");
        if child == 0 {
            // SAFETY: the child is single-threaded and this is its first action.
            unsafe { bound_this_fork_child() };
            let events = events[index][1];
            // SAFETY: this is a fork child of a multi-threaded binary, so the channel must be
            // announced without taking any lock the fork may have copied in a held state.
            let channel = unsafe { transport.connect_in_forked_child_for_test() };
            // SAFETY: no Rust destructors are run after fork.
            if channel < 0 {
                unsafe { libc::_exit(90) }
            }
            let mut byte = [0_u8; 1];
            assert_eq!(park_read(start[0], &mut byte), Some(()), "capture start gate");
            // Progressing BEFORE the freeze: without this the fixture could pass on two members that
            // were never running in the first place.
            park_emit(events, PARK_EVENT_PROGRESSING);
            let registered = park_register_ready(channel, GENERATION, 100 + index as u32);
            // SAFETY: no Rust destructors are run after fork.
            if !registered {
                unsafe { libc::_exit(81) }
            }
            park_emit(events, PARK_EVENT_REGISTERED);
            if index == 1 {
                // The second member owns the shared-object capture. It waits to be let in so the first
                // member is provably already parked when the drain happens.
                let mut byte = [0_u8; 1];
                assert_eq!(park_read(gate[0], &mut byte), Some(()), "shared-object gate");
                let claimed = park_call(channel, protocol::CLAIM, b"pipe.shared\0", &[], GENERATION);
                // SAFETY: no Rust destructors are run after fork.
                if claimed != Some((protocol::STATUS_OK, 0)) {
                    unsafe { libc::_exit(82) }
                }
                // FIRST-CLAIM: every other holder of this object is told it is already taken and returns
                // without touching the fd. That is what keeps the coordinator out of a parked member.
                let repeat = park_call(channel, protocol::CLAIM, b"pipe.shared\0", &[], GENERATION);
                // SAFETY: no Rust destructors are run after fork.
                if repeat != Some((protocol::STATUS_ALREADY, 0)) {
                    unsafe { libc::_exit(83) }
                }
                let mut drained = [0_u8; PARK_SHARED_OBJECT_BYTES];
                let complete =
                    park_read(shared[0], &mut drained) == Some(()) && drained.iter().all(|byte| *byte == 0xab);
                // SAFETY: no Rust destructors are run after fork.
                if !complete {
                    unsafe { libc::_exit(84) }
                }
                park_emit(events, PARK_EVENT_DRAINED);
            }
            let state = park_member(channel, events, GENERATION);
            park_emit(
                events,
                if state == protocol::RELEASE_EXIT {
                    PARK_EVENT_EXIT_DISPOSITION
                } else {
                    PARK_EVENT_RESUMED
                },
            );
            // SAFETY: no Rust destructors are run after fork.
            unsafe { libc::_exit(0) }
        }
        *member = child;
    }

    // The members own every write end from here on, so a member that dies closes its event pipe and the
    // parent observes a dead member rather than blocking on a descriptor it is holding open itself.
    for member in &events {
        park_close(member[1]);
        // SAFETY: reading a member's events must never block the coordinator.
        unsafe { libc::fcntl(member[0], libc::F_SETFL, libc::O_NONBLOCK) };
    }
    park_close(start[0]);
    park_close(gate[0]);
    park_close(shared[0]);
    park_close(shared[1]);

    let server = Arc::new(Server::new(Arc::new(Store), Arc::new(Store)));
    let id = server
        .begin_capture(GENERATION, std::time::Instant::now() + Duration::from_secs(60))
        .expect("capture generation");
    for _ in 0..members.len() {
        let (channel, authority) = broker
            .accept(Duration::from_secs(20))
            .expect("authenticated member accept");
        let worker = Arc::clone(&server);
        std::thread::spawn(move || worker.serve_authenticated_for_test(channel, authority));
    }
    server.await_accepts(members.len());
    assert_eq!(park_write(start[1], b"gg"), Some(()), "release both members");

    for member in &events {
        park_await(member[0], PARK_EVENT_PROGRESSING, "pre-freeze progress");
        park_await(member[0], PARK_EVENT_REGISTERED, "REGISTER_READY");
    }
    // Member 0 is now inside its park, and nowhere else: the only thing it emits from here on is the
    // park heartbeat.
    park_await(events[0][0], PARK_EVENT_HEARTBEAT, "first member to park");
    assert_eq!(park_write(gate[1], b"g"), Some(()), "admit the shared-object capture");
    park_await(events[1][0], PARK_EVENT_DRAINED, "shared object captured");

    // THE PROPERTY. The shared object has been captured destructively by member 1, and member 0 -- the
    // holder of the other end -- is still stopped inside its park AND still alive: it answers with a
    // heartbeat emitted after the drain, and the kernel still has it. Under today's `_exit` peer arm
    // member 0 would already be a corpse here and neither assertion could hold.
    park_await(
        events[0][0],
        PARK_EVENT_HEARTBEAT,
        "first member alive after the capture",
    );
    park_await(
        events[1][0],
        PARK_EVENT_HEARTBEAT,
        "second member parked after the capture",
    );
    for member in members {
        // SAFETY: signal 0 only probes for the process; it delivers nothing.
        assert_eq!(
            unsafe { libc::kill(member, 0) },
            0,
            "member {member} died inside the freeze"
        );
    }

    // ABORT BEFORE RELEASE. Nothing was published, so every member is told to resume and the container
    // survives. (Member 1 drained a shared object, so the engine's own abort contract makes ITS resume
    // terminal; that decision belongs to the member, and the protocol's answer here is RESUME.)
    server.abort_capture(id).expect("abort the capture");
    for member in &events {
        park_await(member[0], PARK_EVENT_RESUMED, "member resumed out of its park");
    }
    for member in members {
        let mut status = 0;
        // SAFETY: member is a direct unreaped child and status is writable.
        assert_eq!(unsafe { libc::waitpid(member, &raw mut status, 0) }, member);
        assert!(libc::WIFEXITED(status), "member {member} did not exit cleanly");
        assert_eq!(libc::WEXITSTATUS(status), 0, "member {member} park protocol failed");
    }
    drop(transport);
    server.stop();
}

#[test]
fn a_parked_member_is_released_to_exit_only_once_the_manifest_has_committed() {
    let store = Arc::new(TransactionStore::default());
    let server = Arc::new(Server::new(store.clone(), store));
    server
        .begin_capture(9, std::time::Instant::now() + Duration::from_secs(60))
        .expect("capture generation");
    // Running capture: HOLD, and only for this member's own generation.
    assert_eq!(server.release_disposition(9), protocol::RELEASE_HOLD);
    assert_eq!(server.release_disposition(8), protocol::RELEASE_RESUME);
    server.publish_manifest(&[0_u8; 8]).expect("publish the manifest");
    assert_eq!(server.release_disposition(9), protocol::RELEASE_EXIT);
    assert!(server.committed(), "EXIT was answered without a committed image");

    // A capture that is abandoned releases every member to RESUME instead, which is the whole
    // "an abort before release leaves the container running" half of the contract.
    let store = Arc::new(TransactionStore::default());
    let server = Arc::new(Server::new(store.clone(), store));
    let id = server
        .begin_capture(9, std::time::Instant::now() + Duration::from_secs(60))
        .expect("capture generation");
    assert_eq!(server.release_disposition(9), protocol::RELEASE_HOLD);
    server.abort_capture(id).expect("abort the capture");
    assert_eq!(server.release_disposition(9), protocol::RELEASE_RESUME);
    assert!(!server.committed());
}

/// Byte store whose source side always answers with the bytes of a previously
/// published image, so a read served from the restore source during a capture
/// is visible as a wrong-store answer rather than as an absence.
#[derive(Default)]
struct PublishedImage;

const PUBLISHED_BYTES: &[u8] = b"previously-published-image";

impl CheckpointSink for PublishedImage {
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
        Ok(())
    }
    fn commit_until(&self, _: NonZeroU64, _: &[u8], _: std::time::Instant) -> Result<(), CompositionError> {
        Ok(())
    }
}

impl CheckpointSource for PublishedImage {
    fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
        Ok(PUBLISHED_BYTES.to_vec())
    }
    fn get(&self, _: &str) -> Result<Vec<u8>, CompositionError> {
        Ok(PUBLISHED_BYTES.to_vec())
    }
    fn get_until(&self, _: &str, _: std::time::Instant) -> Result<Vec<u8>, CompositionError> {
        Ok(PUBLISHED_BYTES.to_vec())
    }
    fn list(&self) -> Result<Vec<String>, CompositionError> {
        Ok(vec![String::from("proc.1")])
    }
    fn list_until(&self, _: std::time::Instant) -> Result<Vec<String>, CompositionError> {
        Ok(vec![String::from("proc.1")])
    }
}

/// `SOURCE_*` resolve the restore source, which during a capture is the previous
/// generation and not the group the capture is writing. The sink has no read
/// path at all, so there is nothing correct to serve: the scope must refuse.
#[test]
fn source_reads_are_refused_while_a_capture_is_active() {
    let store = Arc::new(PublishedImage);
    let server = Server::new(store.clone(), store);
    let idle = object_request(protocol::SOURCE_READ, 0, 0);
    let idle_reply = server.dispatch(1, &idle, "proc.1", &[]);
    assert_eq!(
        idle_reply.status,
        protocol::STATUS_OK,
        "restore reads stay available outside a capture"
    );

    server
        .begin_capture(4, std::time::Instant::now() + Duration::from_secs(5))
        .expect("capture scope");
    for op in [protocol::SOURCE_READ, protocol::SOURCE_SIZE, protocol::SOURCE_LIST] {
        let request = object_request(op, 0, 4);
        let reply = server.dispatch(1, &request, "proc.1", &[]);
        assert_ne!(
            reply.status,
            protocol::STATUS_OK,
            "op {op} was served from the restore source during an active capture"
        );
    }

    let digest = object_request(protocol::DIGEST, 0, 4);
    assert_ne!(
        server.dispatch(1, &digest, "", &[]).status,
        protocol::STATUS_OK,
        "an empty capture must not report the previous image's digest"
    );
}

/// Builds one `struct ckpt_fd` (`linux_abi/checkpoint/capture.c:172-179`) for a
/// `CKF_SOCKETPAIR` endpoint, exactly as `image.c:139-147` writes it.
fn socketpair_record(guest_descriptor: i32, object: u64, peer: u64) -> Vec<u8> {
    let mut bytes = vec![0_u8; 560];
    bytes[0..4].copy_from_slice(&guest_descriptor.to_ne_bytes());
    bytes[4..8].copy_from_slice(&10_i32.to_ne_bytes());
    bytes[16..24].copy_from_slice(&1_i64.to_ne_bytes());
    bytes[24..32].copy_from_slice(&object.to_ne_bytes());
    bytes[40..48].copy_from_slice(&peer.to_ne_bytes());
    bytes
}

fn capture_with_socket_inventories(
    store: &Arc<TransactionStore>,
    generation: u32,
    inventories: &[(&str, Vec<u8>)],
) -> Result<(), CaptureFailure> {
    let server = Server::new(store.clone(), store.clone());
    let capture = server
        .begin_capture(generation, std::time::Instant::now() + Duration::from_secs(1))
        .unwrap();
    let group_begin = object_request(protocol::GROUP_BEGIN, 0, generation);
    let group_commit = object_request(protocol::GROUP_COMMIT, 0, generation);
    let begin = object_request(protocol::OBJECT_BEGIN, 1, generation);
    let write = object_request(protocol::OBJECT_WRITE, 1, generation);
    let finish = object_request(protocol::OBJECT_FINISH, 1, generation);
    for (index, (group, records)) in inventories.iter().enumerate() {
        let publisher = 7 + index as u64;
        assert_eq!(
            server.dispatch(publisher, &group_begin, group, &[]).status,
            protocol::STATUS_OK
        );
        assert_eq!(
            server.dispatch(publisher, &begin, &format!("{group}/fds"), &[]).status,
            protocol::STATUS_OK
        );
        assert_eq!(
            server.dispatch(publisher, &write, "", records).status,
            protocol::STATUS_OK
        );
        assert_eq!(server.dispatch(publisher, &finish, "", &[]).status, protocol::STATUS_OK);
        assert_eq!(
            server.dispatch(publisher, &group_commit, group, &[]).status,
            protocol::STATUS_OK
        );
    }
    let result = server.publish_manifest(b"manifest");
    let _ = server.wait_capture(capture, std::time::Instant::now() + Duration::from_secs(1));
    result
}

#[test]
fn a_reciprocal_socket_topology_publishes_its_generation() {
    let store = Arc::new(TransactionStore::default());
    store.seed_committed("MANIFEST", b"prior");
    assert_eq!(
        capture_with_socket_inventories(
            &store,
            41,
            &[
                (
                    "proc.1",
                    socketpair_record(10, 0x003c_a832_0000_0002, 0x003c_ac81_0000_0001)
                ),
                (
                    "proc.2",
                    socketpair_record(7, 0x003c_ac81_0000_0001, 0x003c_a832_0000_0002)
                ),
            ],
        ),
        Ok(())
    );
    assert!(
        store
            .snapshot()
            .0
            .iter()
            .any(|(name, _)| name == "proc.1/fds" && store.snapshot().0.iter().any(|(other, _)| other == "proc.2/fds"))
    );
}

#[test]
fn a_socket_endpoint_no_member_reciprocates_leaves_no_generation() {
    let store = Arc::new(TransactionStore::default());
    store.seed_committed("MANIFEST", b"prior");
    assert_eq!(
        capture_with_socket_inventories(
            &store,
            42,
            &[(
                "proc.1",
                socketpair_record(10, 0x003c_a832_0000_0002, 0x003c_ac81_0000_0001)
            )],
        ),
        Err(CaptureFailure::Failed)
    );
    assert_eq!(store.snapshot().0, [("MANIFEST".into(), b"prior".to_vec())]);
}

#[test]
fn one_socket_object_owned_by_two_members_leaves_no_generation() {
    let store = Arc::new(TransactionStore::default());
    store.seed_committed("MANIFEST", b"prior");
    let mut first = socketpair_record(10, 0x11, 0x22);
    first.extend(socketpair_record(11, 0x22, 0x11));
    assert_eq!(
        capture_with_socket_inventories(
            &store,
            43,
            &[("proc.1", first), ("proc.2", socketpair_record(4, 0x11, 0x22))],
        ),
        Err(CaptureFailure::Failed)
    );
    assert_eq!(store.snapshot().0, [("MANIFEST".into(), b"prior".to_vec())]);
}
/// Each minted test channel is a live channel of its own, however many are minted in one process.
///
/// The engine caches one channel per process on purpose -- a guest process has exactly one for its
/// whole life -- and the minting hook has to opt out of that cache, because it hands each descriptor
/// to a caller who closes it. Without that, the second channel minted in a test binary was the first
/// one's already-closed descriptor: the accept below never completed, and closing it a second time
/// aborted the whole run on an IO-safety violation, which reads as some unrelated test failing.
#[test]
fn every_minted_test_channel_is_a_live_channel_of_its_own() {
    let _serial = TRANSPORT_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for round in 0..2 {
        let (broker, transport) = hl_native::CheckpointTransport::create().expect("checkpoint transport");
        let member = transport.connect_for_test().expect("checkpoint channel");
        let (channel, authority) = broker
            .accept(Duration::from_secs(10))
            .unwrap_or_else(|| panic!("channel {round} was never accepted"));
        assert_eq!(authority.host_pid, u64::from(std::process::id()));
        drop(channel);
        drop(member);
        drop(transport);
        drop(broker);
    }
}
