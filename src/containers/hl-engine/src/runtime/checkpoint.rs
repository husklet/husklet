//! Safe server for the retained C checkpoint object-stream protocol.
//!
//! Descriptor creation and worker inheritance are deliberately outside this
//! module. Keeping the codec/store state independently testable prevents a
//! partially wired product path from advertising checkpoint support.

use crate::composition::{CheckpointSink, CheckpointSource};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    num::NonZeroU64,
    os::unix::net::UnixStream,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

mod authority;
mod broker;
#[path = "checkpoint_lifecycle.rs"]
mod lifecycle;
pub(super) mod member_stdio;
pub(super) mod members;
mod participants;
#[path = "checkpoint_protocol.rs"]
mod protocol;
mod publication;
mod reciprocity;
mod request;
#[cfg(test)]
#[path = "checkpoint_test.rs"]
mod test;
mod transaction;
use participants::ParticipantLedger;
use protocol::{
    CAPTURE_REFUSED, CLAIM, COMMIT, DIGEST, GROUP_ABORT, GROUP_BEGIN, GROUP_COMMIT, GROUP_COUNT, GROUP_PRESENT,
    MEMBER_EXITED, MEMBER_RESTORED, MEMBER_STDIO, OBJECT_ABORT, OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL, OBJECT_WRITE,
    OBJECT_WRITE_AT, PARTICIPANT_REGISTERED, PAYLOAD_MAX, RECOVERY_COMPLETE, REGISTER_READY, RELEASE_EXIT,
    RELEASE_HOLD, RELEASE_RESUME, RELEASE_WAIT, REQUEST_BYTES, Reply, Request, SEAL_MEMBERSHIP, SOURCE_LIST,
    SOURCE_READ, SOURCE_SIZE, STATUS_ALREADY, UNCLAIM,
};

const HASH_BASIS: u64 = 14_695_981_039_346_656_037;
const HASH_PRIME: u64 = 1_099_511_628_211;
const ABORT_SETTLEMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

struct Object {
    name: String,
    bytes: Vec<u8>,
}

#[derive(Default)]
struct State {
    open: HashMap<(u64, u64), Object>,
    staged: HashMap<String, Vec<Object>>,
    groups: HashSet<String>,
    claims: HashSet<String>,
    digest: BTreeMap<String, (u64, u64)>,
    /// The `proc.<gpid>/fds` inventories of the running capture, retained for
    /// the reciprocal socket-topology join `publish_manifest` performs.
    topology: reciprocity::SocketTopology,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureFailure {
    Deadline,
    Failed,
    /// The engine decided not to publish this capture and said why. Distinct from `Failed` because it
    /// is a decision with a recoverable reason (`Server::capture_refusal`) rather than a breakage, and
    /// because it is reported at the moment of the decision rather than at a deadline.
    Refused,
    Poisoned,
    Busy,
    /// The generation the byte store offered for recovery is not finalized, so
    /// native restore must not read it.
    Unfinalized,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturePhase {
    Idle,
    Recovery {
        id: u64,
        deadline: std::time::Instant,
    },
    RecoveryFinished {
        id: u64,
        result: Result<(), CaptureFailure>,
    },
    Active {
        id: u64,
        deadline: std::time::Instant,
    },
    Publishing {
        id: u64,
    },
    Aborting {
        id: u64,
    },
    Finished {
        id: u64,
        result: Result<(), CaptureFailure>,
    },
    Complete,
    Poisoned,
}

struct CaptureState {
    phase: CapturePhase,
    mutations: usize,
    recovery_report_published: bool,
    recovery_result: Option<(u64, Result<(), CaptureFailure>)>,
    capture_result: Option<(u64, Result<(), CaptureFailure>)>,
}

struct MutationAdmission<'a> {
    server: &'a Server,
    id: u64,
    deadline: std::time::Instant,
    finished: bool,
}

impl MutationAdmission<'_> {
    fn finish(mut self, result: Result<(), CaptureFailure>) -> Result<(), CaptureFailure> {
        self.finished = true;
        self.server.finish_mutation(self.id, result)
    }
}

impl Drop for MutationAdmission<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.server.finish_mutation(self.id, Err(CaptureFailure::Failed));
        }
    }
}

pub(crate) struct Server {
    sink: Arc<dyn CheckpointSink>,
    source: Arc<dyn CheckpointSource>,
    state: Mutex<State>,
    transaction: Mutex<Option<NonZeroU64>>,
    capture: Mutex<CaptureState>,
    capture_changed: Condvar,
    channels: Mutex<HashMap<i32, Arc<UnixStream>>>,
    recovery_connections: Mutex<HashMap<u64, u64>>,
    /// Members of the restored tree that have announced themselves, keyed on the guest pid the image
    /// names each of them by. Outlives the recovery scope: recovery ends exactly when the tree starts
    /// running, which is when a host begins needing to reach into it.
    members: Arc<members::RestoredMembers>,
    /// The terminal the host pre-created for each member it is about to revive, keyed on the same guest
    /// pid. Registered before the restore starts, because a member asks for it from inside its own
    /// descriptor restore.
    member_terminals: Arc<member_stdio::MemberTerminals>,
    participants: Mutex<Option<ParticipantLedger>>,
    /// Why the engine refused the last capture, in its own words.
    ///
    /// A refusal used to exist only as a `[ckpt] refuse:` line on the engine's stderr, so every
    /// host-side report of a failed capture named the failure and not the cause. The coordinator now
    /// sends the reason before it exits, and this is where it is held for the report.
    refusal: Mutex<Option<String>>,
    committed: AtomicBool,
    running: AtomicBool,
    connections: AtomicUsize,
    #[cfg(test)]
    dispatches: AtomicUsize,
    #[cfg(test)]
    accepts: AtomicUsize,
}

impl Server {
    pub(crate) fn new(sink: Arc<dyn CheckpointSink>, source: Arc<dyn CheckpointSource>) -> Self {
        Self {
            sink,
            source,
            state: Mutex::new(State::default()),
            transaction: Mutex::new(None),
            capture: Mutex::new(CaptureState {
                phase: CapturePhase::Idle,
                mutations: 0,
                recovery_report_published: false,
                recovery_result: None,
                capture_result: None,
            }),
            capture_changed: Condvar::new(),
            channels: Mutex::new(HashMap::new()),
            recovery_connections: Mutex::new(HashMap::new()),
            members: Arc::new(members::RestoredMembers::default()),
            member_terminals: Arc::new(member_stdio::MemberTerminals::default()),
            participants: Mutex::new(None),
            refusal: Mutex::new(None),
            committed: AtomicBool::new(false),
            running: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
            #[cfg(test)]
            dispatches: AtomicUsize::new(0),
            #[cfg(test)]
            accepts: AtomicUsize::new(0),
        }
    }

    /// Why the engine refused the last capture, in the coordinator's own words, or `None` when no
    /// refusal was reported for it.
    #[must_use]
    pub(crate) fn capture_refusal(&self) -> Option<String> {
        self.refusal.lock().ok().and_then(|reason| reason.clone())
    }

    /// Record a refusal reason the coordinator reported, replacing whatever the previous capture left.
    pub(super) fn record_refusal(&self, reason: String) {
        if let Ok(mut held) = self.refusal.lock() {
            *held = Some(reason);
        }
    }

    /// One member of the restored tree, by the guest pid its image names it by.
    ///
    /// `None` for a guest pid no restore announced, which includes every guest pid when this domain
    /// was started fresh rather than restored. The member is addressable for as long as its process
    /// incarnation lives, and reports its own exit afterwards.
    #[must_use]
    pub(crate) fn restored_member(&self, guest_pid: std::num::NonZeroI32) -> Option<Arc<members::RestoredMember>> {
        self.members.get(guest_pid)
    }

    /// Registers the terminal one sealed member will reattach to when the restore revives it.
    ///
    /// Must be called before the restore starts. The member asks for this from inside its descriptor
    /// restore, which is the first thing that runs after its memory is back, and an unanswered member
    /// keeps the container's single bridge for the rest of its life.
    pub(crate) fn register_member_terminal(
        &self,
        guest_pid: std::num::NonZeroI32,
        terminal: std::os::fd::OwnedFd,
    ) -> Result<(), &'static str> {
        self.member_terminals.register(guest_pid, terminal)
    }

    /// Blocks until at least `count` checkpoint channels have ever been accepted.
    ///
    /// Tests must not poll `connections`, which is a level and returns to zero as
    /// soon as a channel closes: a worker that opens and closes its channel inside
    /// one scheduling slice leaves a level poll spinning forever. `accepts` only
    /// ever increases, so the edge cannot be missed, and the wait is bounded.
    #[cfg(test)]
    pub(crate) fn await_accepts(&self, count: usize) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while self.accepts.load(Ordering::Acquire) < count {
            assert!(
                std::time::Instant::now() < deadline,
                "checkpoint channel was never accepted"
            );
            std::thread::yield_now();
        }
    }

    #[cfg(test)]
    pub(crate) fn dispatch_count(&self) -> usize {
        self.dispatches.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn poison_coordination(&self) -> ! {
        let _held = self.capture.lock().unwrap();
        panic!("intentional recovery coordination poison");
    }

    fn capture_lock(&self) -> Result<std::sync::MutexGuard<'_, CaptureState>, CaptureFailure> {
        match self.capture.lock() {
            Ok(capture) => Ok(capture),
            Err(poisoned) => {
                let mut capture = poisoned.into_inner();
                capture.phase = CapturePhase::Poisoned;
                self.capture_changed.notify_all();
                self.capture.clear_poison();
                Err(CaptureFailure::Poisoned)
            }
        }
    }

    fn admit_mutation(&self) -> Result<Option<MutationAdmission<'_>>, CaptureFailure> {
        let mut capture = self.capture_lock()?;
        match capture.phase {
            CapturePhase::Idle => Ok(None),
            CapturePhase::Active { id, deadline } if std::time::Instant::now() < deadline => {
                capture.mutations = capture.mutations.checked_add(1).ok_or(CaptureFailure::Poisoned)?;
                Ok(Some(MutationAdmission {
                    server: self,
                    id,
                    deadline,
                    finished: false,
                }))
            }
            CapturePhase::Recovery { id, deadline } if std::time::Instant::now() < deadline => {
                capture.mutations = capture.mutations.checked_add(1).ok_or(CaptureFailure::Poisoned)?;
                Ok(Some(MutationAdmission {
                    server: self,
                    id,
                    deadline,
                    finished: false,
                }))
            }
            CapturePhase::Recovery { .. } => Err(CaptureFailure::Deadline),
            CapturePhase::Active { .. } => Err(CaptureFailure::Deadline),
            CapturePhase::Poisoned => Err(CaptureFailure::Poisoned),
            _ => Err(CaptureFailure::Busy),
        }
    }

    fn finish_mutation(&self, id: u64, result: Result<(), CaptureFailure>) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        if capture.mutations == 0 {
            capture.phase = CapturePhase::Poisoned;
            self.capture_changed.notify_all();
            return Err(CaptureFailure::Poisoned);
        }
        capture.mutations -= 1;
        if let Err(failure) = result
            && matches!(capture.phase, CapturePhase::Active { id: active, .. } if active == id)
        {
            capture.phase = CapturePhase::Finished {
                id,
                result: Err(failure),
            };
        }
        if result.is_err() && matches!(capture.phase, CapturePhase::Recovery { id: active, .. } if active == id) {
            capture.phase = CapturePhase::Poisoned;
        }
        self.capture_changed.notify_all();
        let terminal = result.is_err();
        drop(capture);
        if terminal {
            self.interrupt_channels();
        }
        result
    }

    fn source_deadline(&self) -> Result<Option<std::time::Instant>, CaptureFailure> {
        let capture = self.capture_lock()?;
        match capture.phase {
            CapturePhase::Idle => Ok(None),
            CapturePhase::Recovery { deadline, .. } if std::time::Instant::now() < deadline => Ok(Some(deadline)),
            CapturePhase::Recovery { .. } => Err(CaptureFailure::Deadline),
            CapturePhase::RecoveryFinished { result: Ok(()), .. } => Ok(None),
            CapturePhase::RecoveryFinished { result: Err(error), .. } => Err(error),
            CapturePhase::Active { deadline, .. } if std::time::Instant::now() < deadline => Ok(Some(deadline)),
            CapturePhase::Active { .. } => Err(CaptureFailure::Deadline),
            CapturePhase::Publishing { .. } => Err(CaptureFailure::Busy),
            CapturePhase::Aborting { .. } => Err(CaptureFailure::Busy),
            CapturePhase::Finished { result: Ok(()), .. } => Ok(None),
            CapturePhase::Finished { result: Err(error), .. } => Err(error),
            CapturePhase::Complete => Ok(None),
            CapturePhase::Poisoned => Err(CaptureFailure::Poisoned),
        }
    }

    fn finish_failed(&self, id: u64, failure: CaptureFailure) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        if matches!(capture.phase, CapturePhase::Active { id: active, .. } if active == id) {
            capture.phase = CapturePhase::Finished {
                id,
                result: Err(failure),
            };
            self.capture_changed.notify_all();
        }
        drop(capture);
        self.interrupt_channels();
        Ok(())
    }

    fn settle_failed_capture(&self, id: u64, failure: CaptureFailure) -> Result<CaptureFailure, CaptureFailure> {
        let settlement_deadline = std::time::Instant::now() + ABORT_SETTLEMENT_TIMEOUT;
        let mut capture = self.capture_lock()?;
        loop {
            match capture.phase {
                CapturePhase::Finished {
                    id: active,
                    result: Err(_),
                } if active == id => {
                    if capture.mutations != 0 {
                        let now = std::time::Instant::now();
                        if now >= settlement_deadline {
                            capture.phase = CapturePhase::Poisoned;
                            self.capture_changed.notify_all();
                            drop(capture);
                            self.interrupt_channels();
                            return Err(CaptureFailure::Deadline);
                        }
                        let (next, timeout) = self
                            .capture_changed
                            .wait_timeout(capture, settlement_deadline.saturating_duration_since(now))
                            .map_err(|_| CaptureFailure::Poisoned)?;
                        capture = next;
                        if timeout.timed_out() && capture.mutations != 0 {
                            capture.phase = CapturePhase::Poisoned;
                            self.capture_changed.notify_all();
                            drop(capture);
                            self.interrupt_channels();
                            return Err(CaptureFailure::Deadline);
                        }
                        continue;
                    }
                    capture.phase = CapturePhase::Aborting { id };
                    self.capture_changed.notify_all();
                    drop(capture);
                    let mut transition = transaction::AbortTransition {
                        server: self,
                        id,
                        finished: false,
                    };
                    let discarded = self.discard_transaction(std::time::Instant::now() + ABORT_SETTLEMENT_TIMEOUT);
                    let mut capture = self.capture_lock()?;
                    if !matches!(capture.phase, CapturePhase::Aborting { id: active } if active == id) {
                        capture.phase = CapturePhase::Poisoned;
                        self.capture_changed.notify_all();
                        return Err(CaptureFailure::Poisoned);
                    }
                    capture.phase = CapturePhase::Poisoned;
                    capture.capture_result = Some((id, Err(failure)));
                    self.capture_changed.notify_all();
                    transition.finished = true;
                    return discarded.map(|()| failure);
                }
                CapturePhase::Aborting { id: active } if active == id => {
                    let now = std::time::Instant::now();
                    if now >= settlement_deadline {
                        return Err(CaptureFailure::Deadline);
                    }
                    let (next, timeout) = self
                        .capture_changed
                        .wait_timeout(capture, settlement_deadline.saturating_duration_since(now))
                        .map_err(|_| CaptureFailure::Poisoned)?;
                    capture = next;
                    if timeout.timed_out() && matches!(capture.phase, CapturePhase::Aborting { .. }) {
                        return Err(CaptureFailure::Deadline);
                    }
                }
                CapturePhase::Poisoned => return Err(CaptureFailure::Poisoned),
                _ => return Err(CaptureFailure::Busy),
            }
        }
    }

    fn interrupt_channels(&self) {
        if let Ok(channels) = self.channels.lock() {
            for channel in channels.values() {
                let _ = channel.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    pub(crate) fn abort_capture(&self, id: u64) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        if matches!(capture.phase, CapturePhase::Active { id: active, .. } if active == id) {
            capture.phase = CapturePhase::Finished {
                id,
                result: Err(CaptureFailure::Failed),
            };
            self.capture_changed.notify_all();
        }
        drop(capture);
        self.interrupt_channels();
        self.settle_failed_capture(id, CaptureFailure::Failed).map(|_| ())
    }

    pub(crate) fn wait_capture(
        &self,
        id: u64,
        wake: std::time::Instant,
    ) -> Result<Option<Result<(), CaptureFailure>>, CaptureFailure> {
        let mut capture = self.capture_lock()?;
        loop {
            if let Some((completed, result)) = capture.capture_result
                && completed == id
            {
                return Ok(Some(result));
            }
            match capture.phase {
                CapturePhase::Active { id: active, deadline } if active == id => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        capture.phase = CapturePhase::Finished {
                            id,
                            result: Err(CaptureFailure::Deadline),
                        };
                        self.capture_changed.notify_all();
                        drop(capture);
                        self.interrupt_channels();
                        return self
                            .settle_failed_capture(id, CaptureFailure::Deadline)
                            .map(|failure| Some(Err(failure)));
                    }
                    if now >= wake {
                        return Ok(None);
                    }
                    let wait = deadline.min(wake).saturating_duration_since(now);
                    let (next, timeout) = match self.capture_changed.wait_timeout(capture, wait) {
                        Ok(result) => result,
                        Err(poisoned) => {
                            let (mut capture, _) = poisoned.into_inner();
                            capture.phase = CapturePhase::Poisoned;
                            self.capture_changed.notify_all();
                            drop(capture);
                            self.interrupt_channels();
                            return Err(CaptureFailure::Poisoned);
                        }
                    };
                    capture = next;
                    if timeout.timed_out() && std::time::Instant::now() >= wake {
                        return Ok(None);
                    }
                }
                CapturePhase::Publishing { id: active } if active == id => {
                    // This capture exclusively owns the synchronous publication attempt.
                    // Storage checks the deadline immediately before replacement; after
                    // replacement starts, its actual result wins over wall-clock expiry.
                    capture = match self.capture_changed.wait(capture) {
                        Ok(capture) => capture,
                        Err(poisoned) => {
                            let mut capture = poisoned.into_inner();
                            capture.phase = CapturePhase::Poisoned;
                            self.capture_changed.notify_all();
                            drop(capture);
                            self.interrupt_channels();
                            return Err(CaptureFailure::Poisoned);
                        }
                    };
                }
                CapturePhase::Aborting { id: active } if active == id => {
                    let now = std::time::Instant::now();
                    if now >= wake {
                        return Ok(None);
                    }
                    let (next, timeout) = match self
                        .capture_changed
                        .wait_timeout(capture, wake.saturating_duration_since(now))
                    {
                        Ok(result) => result,
                        Err(poisoned) => {
                            let (mut capture, _) = poisoned.into_inner();
                            capture.phase = CapturePhase::Poisoned;
                            self.capture_changed.notify_all();
                            return Err(CaptureFailure::Poisoned);
                        }
                    };
                    capture = next;
                    if timeout.timed_out() && std::time::Instant::now() >= wake {
                        return Ok(None);
                    }
                }
                CapturePhase::Finished { id: active, result } if active == id => {
                    if let Err(failure) = result {
                        drop(capture);
                        return self
                            .settle_failed_capture(id, failure)
                            .map(|failure| Some(Err(failure)));
                    }
                    capture.capture_result = Some((id, result));
                    capture.phase = CapturePhase::Complete;
                    return Ok(Some(Ok(())));
                }
                CapturePhase::Poisoned => return Ok(Some(Err(CaptureFailure::Poisoned))),
                _ => return Err(CaptureFailure::Busy),
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn committed(&self) -> bool {
        self.committed.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn transaction_state(&self) -> (usize, usize, usize, usize, usize) {
        let state = self.state.lock().unwrap();
        (
            state.open.len(),
            state.staged.len(),
            state.groups.len(),
            state.claims.len(),
            state.digest.len(),
        )
    }
}
