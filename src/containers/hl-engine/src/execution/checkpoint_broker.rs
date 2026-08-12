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

const ABI: u32 = 1;
const MAGIC_REQUEST: u32 = 0x484b_4351;
const MAGIC_REPLY: u32 = 0x484b_4353;
const NAME_MAX: usize = 512;
const PAYLOAD_MAX: usize = 4 * 1024 * 1024;
const REQUEST_BYTES: usize = 48;
const REPLY_BYTES: usize = 32;
const STATUS_OK: i32 = 0;
const STATUS_ERROR: i32 = -1;
const STATUS_ALREADY: i32 = 1;

const OBJECT_BEGIN: u32 = 1;
const OBJECT_WRITE: u32 = 2;
const OBJECT_WRITE_AT: u32 = 3;
const OBJECT_TELL: u32 = 4;
const OBJECT_FINISH: u32 = 5;
const OBJECT_ABORT: u32 = 6;
const GROUP_BEGIN: u32 = 7;
const GROUP_COMMIT: u32 = 8;
const GROUP_ABORT: u32 = 9;
const CLAIM: u32 = 10;
const UNCLAIM: u32 = 11;
const COMMIT: u32 = 12;
const GROUP_PRESENT: u32 = 13;
const GROUP_COUNT: u32 = 14;
const DIGEST: u32 = 15;
const SOURCE_LIST: u32 = 16;
const SOURCE_SIZE: u32 = 17;
const SOURCE_READ: u32 = 18;

const HASH_BASIS: u64 = 14_695_981_039_346_656_037;
const HASH_PRIME: u64 = 1_099_511_628_211;

#[derive(Debug)]
struct Request {
    op: u32,
    stream: u64,
    offset: u64,
    length: u64,
    name_size: usize,
}

impl Request {
    fn decode(bytes: &[u8; REQUEST_BYTES]) -> Option<Self> {
        let word = |at| u32::from_ne_bytes(bytes[at..at + 4].try_into().expect("fixed request layout"));
        let long = |at| u64::from_ne_bytes(bytes[at..at + 8].try_into().expect("fixed request layout"));
        if word(0) != MAGIC_REQUEST || word(4) != ABI {
            return None;
        }
        let name_size = usize::try_from(word(40)).ok()?;
        let length = long(32);
        if name_size > NAME_MAX || length > PAYLOAD_MAX as u64 {
            return None;
        }
        Some(Self {
            op: word(8),
            stream: long(16),
            offset: long(24),
            length,
            name_size,
        })
    }

    fn carries_payload(&self) -> bool {
        self.length != 0 && self.op != SOURCE_READ
    }
}

struct Reply {
    status: i32,
    value: u64,
    payload: Vec<u8>,
}

impl Reply {
    const fn status(status: i32) -> Self {
        Self {
            status,
            value: 0,
            payload: Vec::new(),
        }
    }

    const fn ok() -> Self {
        Self::status(STATUS_OK)
    }

    const fn error() -> Self {
        Self::status(STATUS_ERROR)
    }

    const fn value(value: u64) -> Self {
        Self {
            status: STATUS_OK,
            value,
            payload: Vec::new(),
        }
    }

    const fn payload(payload: Vec<u8>) -> Self {
        Self {
            status: STATUS_OK,
            value: 0,
            payload,
        }
    }

    fn write(&self, channel: &mut impl Write) -> std::io::Result<()> {
        let mut header = [0_u8; REPLY_BYTES];
        header[0..4].copy_from_slice(&MAGIC_REPLY.to_ne_bytes());
        header[4..8].copy_from_slice(&ABI.to_ne_bytes());
        header[8..12].copy_from_slice(&self.status.to_ne_bytes());
        header[16..24].copy_from_slice(&self.value.to_ne_bytes());
        header[24..32].copy_from_slice(&(self.payload.len() as u64).to_ne_bytes());
        channel.write_all(&header)?;
        channel.write_all(&self.payload)?;
        channel.flush()
    }
}

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

    pub(crate) fn connections(&self) -> usize {
        self.connections.load(Ordering::Acquire)
    }

    pub(crate) fn stop(&self) {
        self.running.store(false, Ordering::Release);
    }

    pub(crate) fn start(server: &Arc<Self>, broker: Broker) -> std::thread::JoinHandle<()> {
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
                let object = match self.state.lock().ok().and_then(|mut state| state.open.remove(&key)) {
                    Some(object) => object,
                    None => return Reply::error(),
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
            CLAIM => self.state.lock().map_or_else(
                |_| Reply::error(),
                |mut state| {
                    if state.claims.insert(name.into()) {
                        Reply::ok()
                    } else {
                        Reply::status(STATUS_ALREADY)
                    }
                },
            ),
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
            COMMIT => match self.sink.commit(payload) {
                Ok(()) => {
                    self.committed.store(true, Ordering::Release);
                    Reply::ok()
                }
                Err(_) => {
                    self.fail("checkpoint store rejected manifest".into());
                    Reply::error()
                }
            },
            SOURCE_LIST => {
                let Ok(names) = self.source.list() else {
                    return Reply::error();
                };
                let mut seen = Vec::new();
                for full in names {
                    let entry = full.split_once('/').map_or(full.as_str(), |(head, _)| head);
                    if entry.starts_with(name) && !seen.iter().any(|held| held == entry) {
                        seen.push(entry.to_owned());
                    }
                }
                let mut payload = Vec::new();
                for entry in &seen {
                    payload.extend_from_slice(entry.as_bytes());
                    payload.push(0);
                }
                Reply {
                    status: STATUS_OK,
                    value: seen.len() as u64,
                    payload,
                }
            }
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
}

pub(crate) struct Broker(std::os::fd::OwnedFd);

impl Broker {
    pub(crate) fn pair() -> std::io::Result<(Self, std::os::fd::OwnedFd)> {
        use std::os::fd::FromRawFd as _;
        let mut parent = 0_u64;
        let mut child = 0_u64;
        // SAFETY: successful creation returns two uniquely owned descriptors.
        if unsafe { super::hl_ckpt_broker_pair(&raw mut parent, &raw mut child) } != 0
            || parent == 0
            || child == 0
            || parent > i32::MAX as u64
            || child > i32::MAX as u64
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: ownership was transferred by the successful C call above.
        unsafe {
            Ok((
                Self(std::os::fd::OwnedFd::from_raw_fd(parent as i32)),
                std::os::fd::OwnedFd::from_raw_fd(child as i32),
            ))
        }
    }

    fn accept(&self, timeout: std::time::Duration) -> Option<(std::os::unix::net::UnixStream, u64)> {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        let timeout = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        let mut host_pid = 0;
        // SAFETY: self keeps the broker live; a nonzero result is a newly owned stream descriptor.
        let channel = unsafe { super::hl_ckpt_broker_accept(self.0.as_raw_fd() as u64, timeout, &raw mut host_pid) };
        if channel == 0 || channel > i32::MAX as u64 {
            return None;
        }
        // SAFETY: the accept call transferred unique ownership.
        Some((
            unsafe { std::os::unix::net::UnixStream::from_raw_fd(channel as i32) },
            host_pid,
        ))
    }
}

pub(crate) struct Trigger {
    descriptor: i32,
    mapping: *mut std::ffi::c_void,
}

// SAFETY: C owns the one-word shared mapping protocol; bump is the only access.
unsafe impl Send for Trigger {}
// SAFETY: capture is serialized by the machine lifecycle; bump is one generation update.
unsafe impl Sync for Trigger {}

impl Trigger {
    pub(crate) fn create() -> std::io::Result<Self> {
        let mut descriptor = 0_u64;
        let mut mapping = std::ptr::null_mut();
        // SAFETY: output pointers are valid and initialized by C on success.
        if unsafe { super::hl_ckpt_trigger_create(&raw mut descriptor, &raw mut mapping) } != 0
            || descriptor == 0
            || descriptor > i32::MAX as u64
            || mapping.is_null()
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self {
            descriptor: descriptor as i32,
            mapping,
        })
    }

    pub(crate) const fn descriptor(&self) -> i32 {
        self.descriptor
    }

    pub(crate) fn bump(&self) -> u32 {
        // SAFETY: mapping remains live for self's lifetime.
        unsafe { super::hl_ckpt_trigger_bump(self.mapping) }
    }
}

impl Drop for Trigger {
    fn drop(&mut self) {
        // SAFETY: this type owns both resources and drops them exactly once.
        unsafe { super::hl_ckpt_trigger_destroy(self.mapping, self.descriptor as u64) };
        self.mapping = std::ptr::null_mut();
        self.descriptor = -1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composition::CompositionError;

    #[derive(Default)]
    struct Store(Mutex<BTreeMap<String, Vec<u8>>>);

    impl CheckpointSink for Store {
        fn replace(&self, _: &[u8]) -> Result<(), CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }
        fn put(&self, name: &str, bytes: &[u8]) -> Result<(), CompositionError> {
            self.0.lock().unwrap().insert(name.into(), bytes.into());
            Ok(())
        }
        fn commit(&self, manifest: &[u8]) -> Result<(), CompositionError> {
            self.put("MANIFEST", manifest)
        }
    }

    impl CheckpointSource for Store {
        fn read(&self, _: usize) -> Result<Vec<u8>, CompositionError> {
            Err(CompositionError::RuntimeConstruction)
        }
        fn get(&self, name: &str) -> Result<Vec<u8>, CompositionError> {
            self.0
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or(CompositionError::RuntimeConstruction)
        }
        fn list(&self) -> Result<Vec<String>, CompositionError> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }
    }

    fn request(op: u32, stream: u64, name: &str, payload: &[u8]) -> Vec<u8> {
        let mut header = [0; REQUEST_BYTES];
        header[0..4].copy_from_slice(&MAGIC_REQUEST.to_ne_bytes());
        header[4..8].copy_from_slice(&ABI.to_ne_bytes());
        header[8..12].copy_from_slice(&op.to_ne_bytes());
        header[16..24].copy_from_slice(&stream.to_ne_bytes());
        header[32..40].copy_from_slice(&(payload.len() as u64).to_ne_bytes());
        header[40..44].copy_from_slice(&((name.len() + 1) as u32).to_ne_bytes());
        let mut frame = header.to_vec();
        frame.extend_from_slice(name.as_bytes());
        frame.push(0);
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn object_group_commit_and_manifest_are_transactional() {
        let store = Arc::new(Store::default());
        let server = Server::new(store.clone(), store.clone());
        assert_eq!(
            server
                .dispatch(
                    1,
                    &Request {
                        op: GROUP_BEGIN,
                        stream: 0,
                        offset: 0,
                        length: 0,
                        name_size: 7
                    },
                    "proc.1",
                    &[]
                )
                .status,
            STATUS_OK
        );
        assert_eq!(
            server
                .dispatch(
                    1,
                    &Request {
                        op: OBJECT_BEGIN,
                        stream: 4,
                        offset: 0,
                        length: 0,
                        name_size: 12
                    },
                    "proc.1/meta",
                    &[]
                )
                .status,
            STATUS_OK
        );
        assert_eq!(
            server
                .dispatch(
                    1,
                    &Request {
                        op: OBJECT_WRITE,
                        stream: 4,
                        offset: 0,
                        length: 5,
                        name_size: 0
                    },
                    "",
                    b"state"
                )
                .status,
            STATUS_OK
        );
        assert_eq!(
            server
                .dispatch(
                    1,
                    &Request {
                        op: OBJECT_FINISH,
                        stream: 4,
                        offset: 0,
                        length: 0,
                        name_size: 0
                    },
                    "",
                    &[]
                )
                .status,
            STATUS_OK
        );
        assert!(store.get("proc.1/meta").is_err());
        assert_eq!(
            server
                .dispatch(
                    1,
                    &Request {
                        op: GROUP_COMMIT,
                        stream: 0,
                        offset: 0,
                        length: 0,
                        name_size: 7
                    },
                    "proc.1",
                    &[]
                )
                .status,
            STATUS_OK
        );
        assert_eq!(store.get("proc.1/meta").unwrap(), b"state");
        assert_eq!(
            server
                .dispatch(
                    1,
                    &Request {
                        op: COMMIT,
                        stream: 0,
                        offset: 0,
                        length: 8,
                        name_size: 0
                    },
                    "",
                    b"manifest"
                )
                .status,
            STATUS_OK
        );
        assert!(server.committed());
    }

    #[test]
    fn wire_server_rejects_non_terminated_names() {
        let store = Arc::new(Store::default());
        let server = Arc::new(Server::new(store.clone(), store));
        let (mut client, mut host) = std::os::unix::net::UnixStream::pair().unwrap();
        let worker = {
            let server = server.clone();
            std::thread::spawn(move || server.serve(&mut host, 1))
        };
        let mut frame = request(OBJECT_BEGIN, 1, "safe", &[]);
        frame[REQUEST_BYTES + 4] = b'x';
        client.write_all(&frame).unwrap();
        drop(client);
        worker.join().unwrap();
        assert!(!server.committed());
    }
}
