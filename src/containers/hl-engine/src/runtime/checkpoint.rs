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

mod broker;
#[path = "checkpoint_protocol.rs"]
mod protocol;
mod publication;
mod request;
#[cfg(test)]
#[path = "checkpoint_test.rs"]
mod test;
mod transaction;
use protocol::{
    CLAIM, COMMIT, DIGEST, GROUP_ABORT, GROUP_BEGIN, GROUP_COMMIT, GROUP_COUNT, GROUP_PRESENT, OBJECT_ABORT,
    OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL, OBJECT_WRITE, OBJECT_WRITE_AT, PAYLOAD_MAX, RECOVERY_COMPLETE,
    REQUEST_BYTES, Reply, Request, SOURCE_LIST, SOURCE_READ, SOURCE_SIZE, STATUS_ALREADY, UNCLAIM,
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
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureFailure {
    Deadline,
    Failed,
    Poisoned,
    Busy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CapturePhase {
    Idle,
    Recovery {
        id: u64,
        deadline: std::time::Instant,
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
    committed: AtomicBool,
    running: AtomicBool,
    connections: AtomicUsize,
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
            }),
            capture_changed: Condvar::new(),
            channels: Mutex::new(HashMap::new()),
            recovery_connections: Mutex::new(HashMap::new()),
            committed: AtomicBool::new(false),
            running: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
        }
    }

    #[cfg(test)]
    pub(crate) fn begin_capture(&self, generation: u32, deadline: std::time::Instant) -> Result<u64, CaptureFailure> {
        self.begin_capture_after_admission(deadline, || generation)
    }

    pub(crate) fn begin_capture_after_admission(
        &self,
        deadline: std::time::Instant,
        activate: impl FnOnce() -> u32,
    ) -> Result<u64, CaptureFailure> {
        if std::time::Instant::now() >= deadline {
            return Err(CaptureFailure::Deadline);
        }
        let mut capture = self.capture_lock()?;
        if !matches!(capture.phase, CapturePhase::Idle) {
            return Err(match capture.phase {
                CapturePhase::Poisoned => CaptureFailure::Poisoned,
                _ => CaptureFailure::Busy,
            });
        }
        self.begin_transaction(deadline)?;
        let id = u64::from(activate());
        if id == 0 {
            let _ = self.discard_transaction(deadline);
            return Err(CaptureFailure::Poisoned);
        }
        self.committed.store(false, Ordering::Release);
        capture.phase = CapturePhase::Active { id, deadline };
        capture.mutations = 0;
        capture.recovery_report_published = false;
        Ok(id)
    }

    #[cfg(test)]
    pub(crate) fn begin_recovery(&self, generation: u32, deadline: std::time::Instant) -> Result<u64, CaptureFailure> {
        self.begin_recovery_after_admission(deadline, || generation)
    }

    pub(crate) fn begin_recovery_after_admission(
        &self,
        deadline: std::time::Instant,
        activate: impl FnOnce() -> u32,
    ) -> Result<u64, CaptureFailure> {
        if std::time::Instant::now() >= deadline {
            return Err(CaptureFailure::Deadline);
        }
        let mut capture = self.capture_lock()?;
        if !matches!(capture.phase, CapturePhase::Idle) {
            return Err(match capture.phase {
                CapturePhase::Poisoned => CaptureFailure::Poisoned,
                _ => CaptureFailure::Busy,
            });
        }
        self.begin_transaction(deadline)?;
        let id = u64::from(activate());
        if id == 0 {
            let _ = self.discard_transaction(deadline);
            return Err(CaptureFailure::Poisoned);
        }
        capture.phase = CapturePhase::Recovery { id, deadline };
        capture.mutations = 0;
        capture.recovery_report_published = false;
        Ok(id)
    }

    pub(crate) fn abort_recovery(&self, id: u64) -> Result<(), CaptureFailure> {
        let settlement_deadline = std::time::Instant::now() + ABORT_SETTLEMENT_TIMEOUT;
        let mut capture = self.capture_lock()?;
        match capture.phase {
            CapturePhase::Recovery { id: active, .. } if active == id => {
                capture.phase = CapturePhase::Aborting { id };
                self.capture_changed.notify_all();
                drop(capture);
                self.interrupt_channels();
                let mut capture = self.capture_lock()?;
                while capture.mutations != 0 {
                    let now = std::time::Instant::now();
                    if now >= settlement_deadline {
                        capture.phase = CapturePhase::Poisoned;
                        self.capture_changed.notify_all();
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
                        return Err(CaptureFailure::Deadline);
                    }
                }
                drop(capture);
                let discarded = self.discard_transaction(settlement_deadline);
                let mut capture = self.capture_lock()?;
                if !matches!(capture.phase, CapturePhase::Aborting { id: active } if active == id) {
                    capture.phase = CapturePhase::Poisoned;
                    self.capture_changed.notify_all();
                    return Err(CaptureFailure::Poisoned);
                }
                capture.phase = if discarded.is_ok() {
                    CapturePhase::Idle
                } else {
                    CapturePhase::Poisoned
                };
                self.capture_changed.notify_all();
                discarded
            }
            CapturePhase::Idle => Ok(()),
            CapturePhase::Poisoned => Err(CaptureFailure::Poisoned),
            _ => Err(CaptureFailure::Busy),
        }
    }

    pub(crate) fn wait_recovery(&self, id: u64) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        loop {
            match capture.phase {
                CapturePhase::Recovery { id: active, deadline } if active == id => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        drop(capture);
                        self.abort_recovery(id)?;
                        return Err(CaptureFailure::Deadline);
                    }
                    let (next, timeout) = self
                        .capture_changed
                        .wait_timeout(capture, deadline.saturating_duration_since(now))
                        .map_err(|_| CaptureFailure::Poisoned)?;
                    capture = next;
                    if timeout.timed_out() {
                        drop(capture);
                        self.abort_recovery(id)?;
                        return Err(CaptureFailure::Deadline);
                    }
                }
                CapturePhase::Aborting { id: active } if active == id => {
                    capture = self
                        .capture_changed
                        .wait(capture)
                        .map_err(|_| CaptureFailure::Poisoned)?;
                }
                CapturePhase::Idle => return Ok(()),
                CapturePhase::Poisoned => return Err(CaptureFailure::Poisoned),
                _ => return Err(CaptureFailure::Busy),
            }
        }
    }

    fn capture_lock(&self) -> Result<std::sync::MutexGuard<'_, CaptureState>, CaptureFailure> {
        match self.capture.lock() {
            Ok(capture) => Ok(capture),
            Err(poisoned) => {
                let mut capture = poisoned.into_inner();
                capture.phase = CapturePhase::Poisoned;
                self.capture_changed.notify_all();
                Err(CaptureFailure::Poisoned)
            }
        }
    }

    fn active_deadline(&self) -> Result<(u64, std::time::Instant), CaptureFailure> {
        let capture = self.capture_lock()?;
        match capture.phase {
            CapturePhase::Active { id, deadline } if std::time::Instant::now() < deadline => Ok((id, deadline)),
            CapturePhase::Active { .. } => Err(CaptureFailure::Deadline),
            CapturePhase::Poisoned => Err(CaptureFailure::Poisoned),
            _ => Err(CaptureFailure::Busy),
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
