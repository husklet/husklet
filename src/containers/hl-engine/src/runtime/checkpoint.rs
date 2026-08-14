//! Safe server for the retained C checkpoint object-stream protocol.
//!
//! Descriptor creation and worker inheritance are deliberately outside this
//! module. Keeping the codec/store state independently testable prevents a
//! partially wired product path from advertising checkpoint support.

use crate::composition::{CheckpointSink, CheckpointSource};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    os::{fd::AsRawFd, unix::net::UnixStream},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[path = "checkpoint_protocol.rs"]
mod protocol;
mod publication;
mod request;
#[cfg(test)]
#[path = "checkpoint_test.rs"]
mod test;
use protocol::{
    CLAIM, COMMIT, DIGEST, GROUP_ABORT, GROUP_BEGIN, GROUP_COMMIT, GROUP_COUNT, GROUP_PRESENT, OBJECT_ABORT,
    OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL, OBJECT_WRITE, OBJECT_WRITE_AT, PAYLOAD_MAX, RECOVERY_COMPLETE,
    REQUEST_BYTES, Reply, Request, SOURCE_LIST, SOURCE_READ, SOURCE_SIZE, STATUS_ALREADY, UNCLAIM,
};

const HASH_BASIS: u64 = 14_695_981_039_346_656_037;
const HASH_PRIME: u64 = 1_099_511_628_211;

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
    failure: Option<String>,
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
    capture: Mutex<CaptureState>,
    capture_changed: Condvar,
    channels: Mutex<HashMap<i32, UnixStream>>,
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

    pub(crate) fn begin_capture(&self, generation: u32, deadline: std::time::Instant) -> Result<u64, CaptureFailure> {
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
        let id = u64::from(generation);
        if id == 0 {
            return Err(CaptureFailure::Poisoned);
        }
        *self.state.lock().map_err(|_| CaptureFailure::Poisoned)? = State::default();
        self.committed.store(false, Ordering::Release);
        capture.phase = CapturePhase::Active { id, deadline };
        capture.mutations = 0;
        capture.recovery_report_published = false;
        Ok(id)
    }

    pub(crate) fn begin_recovery(&self, generation: u32, deadline: std::time::Instant) -> Result<u64, CaptureFailure> {
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
        let id = u64::from(generation);
        if id == 0 {
            return Err(CaptureFailure::Poisoned);
        }
        *self.state.lock().map_err(|_| CaptureFailure::Poisoned)? = State::default();
        capture.phase = CapturePhase::Recovery { id, deadline };
        capture.mutations = 0;
        capture.recovery_report_published = false;
        Ok(id)
    }

    pub(crate) fn abort_recovery(&self, id: u64) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        match capture.phase {
            CapturePhase::Recovery { id: active, .. } if active == id => {
                let result = if capture.mutations == 0 {
                    capture.phase = CapturePhase::Idle;
                    Ok(())
                } else {
                    capture.phase = CapturePhase::Poisoned;
                    Err(CaptureFailure::Failed)
                };
                self.capture_changed.notify_all();
                drop(capture);
                if result.is_err() {
                    self.interrupt_channels();
                }
                result
            }
            CapturePhase::Idle => Ok(()),
            CapturePhase::Poisoned => Err(CaptureFailure::Poisoned),
            _ => Err(CaptureFailure::Busy),
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
            capture.phase = CapturePhase::Poisoned;
            self.capture_changed.notify_all();
        }
        drop(capture);
        self.interrupt_channels();
        Ok(())
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
                        capture.phase = CapturePhase::Poisoned;
                        self.capture_changed.notify_all();
                        drop(capture);
                        self.interrupt_channels();
                        return Ok(Some(Err(CaptureFailure::Deadline)));
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
                CapturePhase::Finished { id: active, result } if active == id => {
                    capture.phase = if result.is_ok() {
                        CapturePhase::Complete
                    } else {
                        CapturePhase::Poisoned
                    };
                    return Ok(Some(result));
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

    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Release);
        if let Ok(mut channels) = self.channels.lock() {
            for (_, channel) in channels.drain() {
                let _ = channel.shutdown(std::net::Shutdown::Both);
            }
        }
    }

    pub(crate) fn start(server: &Arc<Self>, broker: hl_native::CheckpointBroker) -> std::thread::JoinHandle<()> {
        let server = Arc::clone(server);
        std::thread::Builder::new()
            .name("hl-checkpoint-broker".into())
            .spawn(move || {
                let mut workers = Vec::new();
                while server.running.load(Ordering::Acquire) {
                    let Some((channel, host_pid)) = broker.accept(std::time::Duration::from_millis(50)) else {
                        continue;
                    };
                    server.connections.fetch_add(1, Ordering::Release);
                    let worker = Arc::clone(&server);
                    workers.push(std::thread::spawn(move || worker.serve(channel, host_pid)));
                }
                for worker in workers {
                    let _ = worker.join();
                }
            })
            .expect("checkpoint broker thread construction")
    }

    fn fail(&self, message: String) {
        let capture = self.active_deadline().ok().map(|(id, _)| id);
        if let Ok(mut state) = self.state.lock()
            && state.failure.is_none()
        {
            state.failure = Some(message);
        }
        if let Some(id) = capture
            && self.finish_failed(id, CaptureFailure::Failed).is_err()
        {
            self.interrupt_channels();
        }
    }

    pub(crate) fn serve(self: &Arc<Self>, mut channel: UnixStream, id: u64) {
        let descriptor = channel.as_raw_fd();
        let Ok(control) = channel.try_clone() else {
            return;
        };
        let Ok(mut channels) = self.channels.lock() else {
            return;
        };
        channels.insert(descriptor, control);
        drop(channels);
        if let Ok(capture) = self.capture_lock()
            && let CapturePhase::Recovery { id: recovery, .. } = capture.phase
            && let Ok(mut connections) = self.recovery_connections.lock()
        {
            connections.insert(id, recovery);
        }
        let _connection = Connection {
            server: self,
            descriptor,
            id,
        };
        if !self.running.load(Ordering::Acquire) {
            return;
        }
        loop {
            let mut header = [0_u8; REQUEST_BYTES];
            if channel.read_exact(&mut header).is_err() {
                return;
            }
            let Some(request) = Request::decode(&header) else {
                self.fail("checkpoint channel framing is invalid".into());
                return;
            };
            let mut encoded_name = vec![0; request.name_size];
            if channel.read_exact(&mut encoded_name).is_err() {
                return;
            }
            let name = match encoded_name.split_last() {
                Some((0, bytes)) => match std::str::from_utf8(bytes) {
                    Ok(name) => name.to_owned(),
                    Err(_) => return,
                },
                None if request.name_size == 0 => String::new(),
                _ => return,
            };
            let mut payload = Vec::new();
            if request.carries_payload() {
                payload.resize(request.length as usize, 0);
                if channel.read_exact(&mut payload).is_err() {
                    return;
                }
            }
            let reply = self.dispatch(id, &request, &name, &payload);
            if reply.write(&mut channel).is_err() {
                return;
            }
        }
    }
}

struct Connection<'a> {
    server: &'a Server,
    descriptor: i32,
    id: u64,
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if let Ok(mut channels) = self.server.channels.lock() {
            channels.remove(&self.descriptor);
        }
        if let Ok(mut connections) = self.server.recovery_connections.lock() {
            connections.remove(&self.id);
        }
    }
}
