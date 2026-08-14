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
#[cfg(test)]
#[path = "checkpoint_test.rs"]
mod test;
use protocol::{
    CLAIM, COMMIT, DIGEST, GROUP_ABORT, GROUP_BEGIN, GROUP_COMMIT, GROUP_COUNT, GROUP_PRESENT, OBJECT_ABORT,
    OBJECT_BEGIN, OBJECT_FINISH, OBJECT_TELL, OBJECT_WRITE, OBJECT_WRITE_AT, PAYLOAD_MAX, REQUEST_BYTES, Reply,
    Request, SOURCE_LIST, SOURCE_READ, SOURCE_SIZE, STATUS_ALREADY, UNCLAIM,
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
            }),
            capture_changed: Condvar::new(),
            channels: Mutex::new(HashMap::new()),
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
        Ok(id)
    }

    pub(crate) fn abort_recovery(&self, id: u64) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        match capture.phase {
            CapturePhase::Recovery { id: active, .. } if active == id && capture.mutations == 0 => {
                capture.phase = CapturePhase::Idle;
                self.capture_changed.notify_all();
                Ok(())
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
        if let Some(id) = capture {
            let _ = self.finish_failed(id, CaptureFailure::Failed);
        }
    }

    fn hash_extend(mut hash: u64, bytes: &[u8]) -> u64 {
        for byte in bytes {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(HASH_PRIME);
        }
        hash
    }

    fn included(name: &str) -> bool {
        name != "MANIFEST" && name != "RECOVERY.jsonl" && !name.starts_with(".RECOVERY.jsonl.tmp.")
    }

    fn object_hash(name: &str, bytes: &[u8]) -> u64 {
        let mut hash = Self::hash_extend(HASH_BASIS, name.as_bytes());
        hash = Self::hash_extend(hash, &[0]);
        hash = Self::hash_extend(hash, &(bytes.len() as u64).to_ne_bytes());
        Self::hash_extend(hash, bytes)
    }

    fn image_hash(objects: &BTreeMap<String, (u64, u64)>) -> (u64, u64, u64) {
        let mut hash = HASH_BASIS;
        let mut bytes = 0;
        for (name, (object, size)) in objects {
            hash = Self::hash_extend(hash, name.as_bytes());
            hash = Self::hash_extend(hash, &[0]);
            hash = Self::hash_extend(hash, &object.to_ne_bytes());
            bytes += size;
        }
        (hash, objects.len() as u64, bytes)
    }

    fn publish(&self, object: &Object, admission: Option<MutationAdmission<'_>>) -> Result<(), ()> {
        if let Some(admission) = admission {
            let deadline = admission.deadline;
            let result = self
                .publish_object(object, Some(deadline))
                .map_err(|error| {
                    if error == crate::composition::CompositionError::DeadlineExceeded {
                        CaptureFailure::Deadline
                    } else {
                        CaptureFailure::Failed
                    }
                })
                .and_then(|()| {
                    (std::time::Instant::now() < deadline)
                        .then_some(())
                        .ok_or(CaptureFailure::Deadline)
                });
            admission.finish(result).map_err(|_| ())?;
        } else {
            self.publish_object(object, None).map_err(|_| ())?;
        }
        Ok(())
    }

    fn publish_object(
        &self,
        object: &Object,
        deadline: Option<std::time::Instant>,
    ) -> Result<(), crate::composition::CompositionError> {
        deadline.map_or_else(
            || self.sink.put(&object.name, &object.bytes),
            |deadline| self.sink.put_until(&object.name, &object.bytes, deadline),
        )?;
        if Self::included(&object.name) {
            let mut state = self
                .state
                .lock()
                .map_err(|_| crate::composition::CompositionError::RuntimeConstruction)?;
            state.digest.insert(
                object.name.clone(),
                (
                    Self::object_hash(&object.name, &object.bytes),
                    object.bytes.len() as u64,
                ),
            );
        }
        Ok(())
    }

    fn stored_digest(&self) -> Result<(u64, u64, u64), ()> {
        let mut objects = BTreeMap::new();
        let deadline = self.source_deadline().map_err(|_| ())?;
        let names = deadline.map_or_else(|| self.source.list(), |deadline| self.source.list_until(deadline));
        for name in names.map_err(|_| ())? {
            if Self::included(&name) {
                let bytes = deadline
                    .map_or_else(
                        || self.source.get(&name),
                        |deadline| self.source.get_until(&name, deadline),
                    )
                    .map_err(|_| ())?;
                objects.insert(name.clone(), (Self::object_hash(&name, &bytes), bytes.len() as u64));
            }
        }
        Ok(Self::image_hash(&objects))
    }

    fn publish_manifest(&self, manifest: &[u8]) -> Result<(), CaptureFailure> {
        let (id, deadline) = {
            let mut capture = self.capture_lock()?;
            loop {
                let CapturePhase::Active { id, deadline } = capture.phase else {
                    return Err(match capture.phase {
                        CapturePhase::Finished { result: Err(error), .. } => error,
                        CapturePhase::Poisoned => CaptureFailure::Poisoned,
                        _ => CaptureFailure::Busy,
                    });
                };
                if std::time::Instant::now() >= deadline {
                    capture.phase = CapturePhase::Poisoned;
                    self.capture_changed.notify_all();
                    drop(capture);
                    self.interrupt_channels();
                    return Err(CaptureFailure::Deadline);
                }
                if capture.mutations == 0 {
                    capture.phase = CapturePhase::Publishing { id };
                    break (id, deadline);
                }
                let wait = deadline.saturating_duration_since(std::time::Instant::now());
                let (next, _) = self
                    .capture_changed
                    .wait_timeout(capture, wait)
                    .map_err(|_| CaptureFailure::Poisoned)?;
                capture = next;
            }
        };

        let result = match self.sink.commit_until(manifest, deadline) {
            Ok(()) => Ok(()),
            Err(crate::composition::CompositionError::PublishedNotDurable) => {
                hl_log::hl_error!(
                    hl_log::tag::CHECKPOINT,
                    "checkpoint generation published but directory durability is uncertain"
                );
                Ok(())
            }
            Err(crate::composition::CompositionError::DeadlineExceeded) => Err(CaptureFailure::Deadline),
            Err(_) => Err(CaptureFailure::Failed),
        };
        let mut capture = self.capture_lock()?;
        if !matches!(capture.phase, CapturePhase::Publishing { id: active } if active == id) {
            capture.phase = CapturePhase::Poisoned;
            self.capture_changed.notify_all();
            return Err(CaptureFailure::Poisoned);
        }
        capture.phase = CapturePhase::Finished { id, result };
        if result.is_ok() {
            self.committed.store(true, Ordering::Release);
        }
        self.capture_changed.notify_all();
        result
    }

    fn source_get(&self, name: &str) -> Result<Vec<u8>, ()> {
        self.source_deadline()
            .map_err(|_| ())?
            .map_or_else(
                || self.source.get(name),
                |deadline| self.source.get_until(name, deadline),
            )
            .map_err(|_| ())
    }

    fn recovery_object_request(&self, connection: u64, request: &Request, name: &str) -> bool {
        match request.op {
            OBJECT_BEGIN => name == "RECOVERY.jsonl",
            OBJECT_WRITE | OBJECT_WRITE_AT | OBJECT_TELL | OBJECT_FINISH | OBJECT_ABORT => self
                .state
                .lock()
                .ok()
                .and_then(|state| {
                    state
                        .open
                        .get(&(connection, request.stream))
                        .map(|object| object.name.as_str() == "RECOVERY.jsonl")
                })
                .unwrap_or(false),
            _ => false,
        }
    }

    fn request_in_scope(&self, connection: u64, request: &Request, name: &str) -> bool {
        let Ok(capture) = self.capture_lock() else { return false };
        match capture.phase {
            CapturePhase::Idle => {
                request.generation == 0 && matches!(request.op, SOURCE_LIST | SOURCE_SIZE | SOURCE_READ | DIGEST)
            }
            CapturePhase::Recovery { id, deadline } => {
                u64::from(request.generation) == id
                    && std::time::Instant::now() < deadline
                    && (matches!(request.op, SOURCE_LIST | SOURCE_SIZE | SOURCE_READ | DIGEST)
                        || self.recovery_object_request(connection, request, name))
            }
            CapturePhase::Complete => false,
            CapturePhase::Active { id, .. } | CapturePhase::Publishing { id } => u64::from(request.generation) == id,
            CapturePhase::Finished { id, .. } => u64::from(request.generation) == id && request.op == COMMIT,
            CapturePhase::Poisoned => false,
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
        let _connection = Connection {
            server: self,
            descriptor,
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

    #[allow(clippy::too_many_lines)]
    fn dispatch(&self, id: u64, request: &Request, name: &str, payload: &[u8]) -> Reply {
        if !self.request_in_scope(id, request, name) {
            return Reply::error();
        }
        let key = (id, request.stream);
        match request.op {
            OBJECT_BEGIN => {
                let Ok(mut state) = self.state.lock() else {
                    return Reply::error();
                };
                if state
                    .open
                    .insert(
                        key,
                        Object {
                            name: name.into(),
                            bytes: Vec::new(),
                        },
                    )
                    .is_some()
                {
                    return Reply::error();
                }
                Reply::ok()
            }
            OBJECT_WRITE | OBJECT_WRITE_AT => {
                let Ok(mut state) = self.state.lock() else {
                    return Reply::error();
                };
                let Some(object) = state.open.get_mut(&key) else {
                    return Reply::error();
                };
                if request.op == OBJECT_WRITE {
                    object.bytes.extend_from_slice(payload);
                } else {
                    let Some(end) = usize::try_from(request.offset)
                        .ok()
                        .and_then(|offset| offset.checked_add(payload.len()))
                    else {
                        return Reply::error();
                    };
                    let offset = end - payload.len();
                    object.bytes.resize(object.bytes.len().max(end), 0);
                    object.bytes[offset..end].copy_from_slice(payload);
                }
                Reply::ok()
            }
            OBJECT_TELL => self
                .state
                .lock()
                .ok()
                .and_then(|state| state.open.get(&key).map(|object| object.bytes.len() as u64))
                .map_or_else(Reply::error, Reply::value),
            OBJECT_FINISH => {
                let admission = self.admit_mutation().map_err(|_| ()).ok().flatten();
                if matches!(
                    self.capture_lock().map(|capture| capture.phase),
                    Ok(CapturePhase::Active { .. })
                ) && admission.is_none()
                {
                    return Reply::error();
                }
                let Some(object) = self.state.lock().ok().and_then(|mut state| state.open.remove(&key)) else {
                    return Reply::error();
                };
                if let Some(group) = object.name.split_once('/').map(|(group, _)| group.to_owned())
                    && let Ok(mut state) = self.state.lock()
                    && let Some(staged) = state.staged.get_mut(&group)
                {
                    staged.push(object);
                    if let Some(admission) = admission {
                        let _ = admission.finish(Ok(()));
                    }
                    return Reply::ok();
                }
                if self.publish(&object, admission).is_ok() {
                    if object.name == "RECOVERY.jsonl" {
                        let mut capture = match self.capture_lock() {
                            Ok(capture) => capture,
                            Err(_) => return Reply::error(),
                        };
                        if matches!(capture.phase, CapturePhase::Recovery { .. }) && capture.mutations == 0 {
                            capture.phase = CapturePhase::Idle;
                            self.capture_changed.notify_all();
                        } else {
                            return Reply::error();
                        }
                    }
                    Reply::ok()
                } else {
                    self.fail(format!("checkpoint store rejected {}", object.name));
                    Reply::error()
                }
            }
            OBJECT_ABORT => {
                if let Ok(mut state) = self.state.lock() {
                    state.open.remove(&key);
                }
                Reply::ok()
            }
            GROUP_BEGIN => {
                let Ok(mut state) = self.state.lock() else {
                    return Reply::error();
                };
                if state.staged.insert(name.into(), Vec::new()).is_some() {
                    Reply::error()
                } else {
                    Reply::ok()
                }
            }
            GROUP_COMMIT => {
                let admission = self.admit_mutation().map_err(|_| ()).ok().flatten();
                if matches!(
                    self.capture_lock().map(|capture| capture.phase),
                    Ok(CapturePhase::Active { .. })
                ) && admission.is_none()
                {
                    return Reply::error();
                }
                let objects = self
                    .state
                    .lock()
                    .ok()
                    .and_then(|mut state| state.staged.remove(name))
                    .unwrap_or_default();
                let deadline = admission.as_ref().map(|admission| admission.deadline);
                let mut result = Ok(());
                for object in &objects {
                    if let Err(error) = self.publish_object(object, deadline) {
                        result = Err(if error == crate::composition::CompositionError::DeadlineExceeded {
                            CaptureFailure::Deadline
                        } else {
                            CaptureFailure::Failed
                        });
                        self.fail(format!("checkpoint store rejected {}", object.name));
                        break;
                    }
                }
                if let Some(admission) = admission
                    && admission.finish(result).is_err()
                {
                    return Reply::error();
                }
                if result.is_err() {
                    return Reply::error();
                }
                if let Ok(mut state) = self.state.lock() {
                    state.groups.insert(name.into());
                }
                Reply::ok()
            }
            GROUP_ABORT => {
                if let Ok(mut state) = self.state.lock() {
                    state.staged.remove(name);
                }
                Reply::ok()
            }
            CLAIM => self.claim(name),
            UNCLAIM => {
                if let Ok(mut state) = self.state.lock() {
                    state.claims.remove(name);
                }
                Reply::ok()
            }
            GROUP_PRESENT => self.state.lock().map_or_else(
                |_| Reply::error(),
                |state| {
                    let present = state.groups.contains(name);
                    Reply::value(u64::from(present))
                },
            ),
            GROUP_COUNT => self.state.lock().map_or_else(
                |_| Reply::error(),
                |state| Reply::value(state.groups.iter().filter(|group| group.starts_with(name)).count() as u64),
            ),
            DIGEST => {
                let digest = self
                    .state
                    .lock()
                    .ok()
                    .and_then(|state| (!state.digest.is_empty()).then(|| Self::image_hash(&state.digest)))
                    .or_else(|| self.stored_digest().ok());
                let Some((hash, files, bytes)) = digest else {
                    self.fail("checkpoint digest could not be computed".into());
                    return Reply::error();
                };
                let mut payload = Vec::with_capacity(24);
                payload.extend_from_slice(&hash.to_ne_bytes());
                payload.extend_from_slice(&files.to_ne_bytes());
                payload.extend_from_slice(&bytes.to_ne_bytes());
                Reply::payload(payload)
            }
            COMMIT => {
                if self.publish_manifest(payload).is_err() {
                    self.fail("checkpoint store rejected manifest".into());
                    return Reply::error();
                }
                Reply::ok()
            }
            SOURCE_LIST => self.source_list(name),
            SOURCE_SIZE => self.source_get(name).map_or_else(
                |()| Reply::status(STATUS_ALREADY),
                |bytes| Reply::value(bytes.len() as u64),
            ),
            SOURCE_READ => {
                let Ok(bytes) = self.source_get(name) else {
                    return Reply::error();
                };
                let Ok(offset) = usize::try_from(request.offset) else {
                    return Reply::error();
                };
                if offset >= bytes.len() {
                    return Reply::payload(Vec::new());
                }
                let length = usize::try_from(request.length).unwrap_or(0).min(PAYLOAD_MAX);
                Reply::payload(bytes[offset..offset.saturating_add(length).min(bytes.len())].to_vec())
            }
            _ => Reply::error(),
        }
    }

    fn claim(&self, name: &str) -> Reply {
        let Ok(mut state) = self.state.lock() else {
            return Reply::error();
        };
        if state.claims.insert(name.into()) {
            Reply::ok()
        } else {
            Reply::status(STATUS_ALREADY)
        }
    }

    fn source_list(&self, prefix: &str) -> Reply {
        let names = self.source_deadline().map_err(|_| ()).and_then(|deadline| {
            deadline
                .map_or_else(|| self.source.list(), |deadline| self.source.list_until(deadline))
                .map_err(|_| ())
        });
        let Ok(names) = names else {
            return Reply::error();
        };
        let mut seen = Vec::new();
        for full in names {
            let entry = full.split_once('/').map_or(full.as_str(), |(head, _)| head);
            if entry.starts_with(prefix) && !seen.iter().any(|held| held == entry) {
                seen.push(entry.to_owned());
            }
        }
        let mut payload = Vec::new();
        for entry in &seen {
            payload.extend_from_slice(entry.as_bytes());
            payload.push(0);
        }
        Reply::counted_payload(seen.len() as u64, payload)
    }
}

struct Connection<'a> {
    server: &'a Server,
    descriptor: i32,
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        if let Ok(mut channels) = self.server.channels.lock() {
            channels.remove(&self.descriptor);
        }
    }
}
