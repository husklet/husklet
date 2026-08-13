//! Safe server for the retained C checkpoint object-stream protocol.
//!
//! Descriptor creation and worker inheritance are deliberately outside this
//! module. Keeping the codec/store state independently testable prevents a
//! partially wired product path from advertising checkpoint support.

use crate::composition::{CheckpointSink, CheckpointSource};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[path = "checkpoint_protocol.rs"]
mod protocol;
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

pub(crate) struct Server {
    sink: Arc<dyn CheckpointSink>,
    source: Arc<dyn CheckpointSource>,
    state: Mutex<State>,
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
            committed: AtomicBool::new(false),
            running: AtomicBool::new(true),
            connections: AtomicUsize::new(0),
        }
    }

    pub(crate) fn committed(&self) -> bool {
        self.committed.load(Ordering::Acquire)
    }

    pub(crate) fn failure(&self) -> Option<String> {
        self.state.lock().ok()?.failure.clone()
    }

    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    pub(crate) fn start(server: &Arc<Self>, broker: hl_native::CheckpointBroker) -> std::thread::JoinHandle<()> {
        let server = Arc::clone(server);
        std::thread::Builder::new()
            .name("hl-checkpoint-broker".into())
            .spawn(move || {
                let mut workers = Vec::new();
                while server.running.load(Ordering::Acquire) {
                    let Some((mut channel, host_pid)) = broker.accept(std::time::Duration::from_millis(50)) else {
                        continue;
                    };
                    server.connections.fetch_add(1, Ordering::Release);
                    let worker = Arc::clone(&server);
                    workers.push(std::thread::spawn(move || worker.serve(&mut channel, host_pid)));
                }
                for worker in workers {
                    let _ = worker.join();
                }
            })
            .expect("checkpoint broker thread construction")
    }

    fn fail(&self, message: String) {
        if let Ok(mut state) = self.state.lock()
            && state.failure.is_none()
        {
            state.failure = Some(message);
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

    fn publish(&self, object: &Object) -> Result<(), ()> {
        self.sink.put(&object.name, &object.bytes).map_err(|_| ())?;
        if Self::included(&object.name) {
            let mut state = self.state.lock().map_err(|_| ())?;
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
        for name in self.source.list().map_err(|_| ())? {
            if Self::included(&name) {
                let bytes = self.source.get(&name).map_err(|_| ())?;
                objects.insert(name.clone(), (Self::object_hash(&name, &bytes), bytes.len() as u64));
            }
        }
        Ok(Self::image_hash(&objects))
    }

    pub(crate) fn serve(self: &Arc<Self>, channel: &mut (impl Read + Write), id: u64) {
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
            if reply.write(channel).is_err() {
                return;
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn dispatch(&self, id: u64, request: &Request, name: &str, payload: &[u8]) -> Reply {
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
                let Some(object) = self.state.lock().ok().and_then(|mut state| state.open.remove(&key)) else {
                    return Reply::error();
                };
                if let Some(group) = object.name.split_once('/').map(|(group, _)| group.to_owned())
                    && let Ok(mut state) = self.state.lock()
                    && let Some(staged) = state.staged.get_mut(&group)
                {
                    staged.push(object);
                    return Reply::ok();
                }
                if self.publish(&object).is_ok() {
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
                let objects = self
                    .state
                    .lock()
                    .ok()
                    .and_then(|mut state| state.staged.remove(name))
                    .unwrap_or_default();
                for object in &objects {
                    if self.publish(object).is_err() {
                        self.fail(format!("checkpoint store rejected {}", object.name));
                        return Reply::error();
                    }
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
                    return Reply::error();
                };
                let mut payload = Vec::with_capacity(24);
                payload.extend_from_slice(&hash.to_ne_bytes());
                payload.extend_from_slice(&files.to_ne_bytes());
                payload.extend_from_slice(&bytes.to_ne_bytes());
                Reply::payload(payload)
            }
            COMMIT => {
                if self.sink.commit(payload).is_err() {
                    self.fail("checkpoint store rejected manifest".into());
                    return Reply::error();
                }
                self.committed.store(true, Ordering::Release);
                Reply::ok()
            }
            SOURCE_LIST => self.source_list(name),
            SOURCE_SIZE => self.source.get(name).map_or_else(
                |_| Reply::status(STATUS_ALREADY),
                |bytes| Reply::value(bytes.len() as u64),
            ),
            SOURCE_READ => {
                let Ok(bytes) = self.source.get(name) else {
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
        let Ok(names) = self.source.list() else {
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
