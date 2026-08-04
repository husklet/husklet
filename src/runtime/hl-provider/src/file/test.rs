use std::sync::{Arc, Mutex};
use std::thread;

use hl_descriptor::{DescriptorFlags, DescriptorTable, ObjectError, OpenFileDescription, Readiness, ReadinessObserver};

use super::*;
use crate::test_support::Endpoint;

#[derive(Debug)]
struct ServerState {
    content: Vec<u8>,
    offset: usize,
    closes: usize,
    next_remote: u64,
    next_errno: Option<i32>,
}

struct Server {
    endpoint: Endpoint,
    state: Arc<Mutex<ServerState>>,
}

impl Server {
    fn run(self) {
        loop {
            let (kind, request, payload) = self.endpoint.receive_frame();
            if matches!(kind, FrameKind::Subscribe | FrameKind::Unsubscribe) {
                continue;
            }
            let operation = payload[0];
            let reply = self.reply(&payload);
            self.endpoint.send_frame(FrameKind::Reply, request, &reply);
            if operation == 7 && self.finished() {
                break;
            }
        }
    }

    fn finished(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .closes
            >= 2
    }

    fn reply(&self, request: &[u8]) -> Vec<u8> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(error) = state.next_errno.take() {
            let mut reply = vec![0xff];
            reply.extend_from_slice(&error.to_le_bytes());
            reply.extend_from_slice(&[0, 0]);
            return reply;
        }
        match request[0] {
            1 => {
                let remote = state.next_remote;
                state.next_remote += 1;
                let mut reply = vec![1];
                reply.extend_from_slice(&remote.to_le_bytes());
                reply
            }
            2 => Self::read(&mut state, request),
            3 => Self::write(&mut state, request),
            4 => Self::seek(&mut state, request),
            5 => {
                let mut reply = vec![5];
                reply.extend_from_slice(&0o640_u32.to_le_bytes());
                reply.extend_from_slice(&1000_u32.to_le_bytes());
                reply.extend_from_slice(&1001_u32.to_le_bytes());
                reply.extend_from_slice(&(state.content.len() as u64).to_le_bytes());
                reply
            }
            6 => vec![6, 3],
            7 => {
                state.closes += 1;
                vec![7]
            }
            _ => vec![0],
        }
    }

    fn read(state: &mut ServerState, request: &[u8]) -> Vec<u8> {
        let offset = u64::from_le_bytes(request[9..17].try_into().unwrap());
        let count = u32::from_le_bytes(request[17..21].try_into().unwrap()) as usize;
        let start = if offset == u64::MAX {
            state.offset
        } else {
            offset as usize
        };
        let end = start.saturating_add(count).min(state.content.len());
        let bytes = state.content.get(start..end).unwrap_or_default().to_vec();
        if offset == u64::MAX {
            state.offset = end;
        }
        let mut reply = vec![2];
        reply.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        reply.extend_from_slice(&bytes);
        reply
    }

    fn write(state: &mut ServerState, request: &[u8]) -> Vec<u8> {
        let offset = u64::from_le_bytes(request[9..17].try_into().unwrap());
        let count = u32::from_le_bytes(request[17..21].try_into().unwrap()) as usize;
        let start = if offset == u64::MAX {
            state.offset
        } else {
            offset as usize
        };
        if state.content.len() < start + count {
            state.content.resize(start + count, 0);
        }
        state.content[start..start + count].copy_from_slice(&request[21..]);
        if offset == u64::MAX {
            state.offset = start + count;
        }
        let mut reply = vec![3];
        reply.extend_from_slice(&(count as u32).to_le_bytes());
        reply
    }

    fn seek(state: &mut ServerState, request: &[u8]) -> Vec<u8> {
        let offset = i64::from_le_bytes(request[9..17].try_into().unwrap());
        let base = match request[17] {
            0 => 0,
            1 => state.offset as i64,
            2 => state.content.len() as i64,
            _ => -1,
        };
        state.offset = base.saturating_add(offset).max(0) as usize;
        let mut reply = vec![4];
        reply.extend_from_slice(&(state.offset as u64).to_le_bytes());
        reply
    }
}

struct Harness {
    provider: Arc<Provider<Endpoint>>,
    files: ProjectedFiles<Endpoint>,
    state: Arc<Mutex<ServerState>>,
    server: Option<thread::JoinHandle<()>>,
}

impl Harness {
    fn new() -> Self {
        let (client, server) = Endpoint::pair(3);
        let provider = Arc::new(Provider::new(client, ClientLimits::new(256, 16).unwrap()).unwrap());
        let state = Arc::new(Mutex::new(ServerState {
            content: b"abcdef".to_vec(),
            offset: 0,
            closes: 0,
            next_remote: 41,
            next_errno: None,
        }));
        let service = Server {
            endpoint: server,
            state: Arc::clone(&state),
        };
        let server = thread::spawn(move || service.run());
        let files = ProjectedFiles::new(Arc::clone(&provider), 16, 9).unwrap();
        Self {
            provider,
            files,
            state,
            server: Some(server),
        }
    }

    fn open(&self, access: FileAccess) -> Arc<ProjectedFile<Endpoint>> {
        self.files.open_service(7, access, b"/projected/file").unwrap()
    }

    fn finish(mut self) {
        self.server.take().unwrap().join().unwrap();
        self.provider.close();
    }
}

#[test]
fn exact_file_operations() {
    let harness = Harness::new();
    let file = harness.open(FileAccess::ReadWrite);
    let mut bytes = [0_u8; 3];
    assert_eq!(file.read(&mut bytes).unwrap(), 3);
    assert_eq!(&bytes, b"abc");
    assert_eq!(file.seek(-1, 2).unwrap(), 5);
    assert_eq!(file.write(b"Z").unwrap(), 1);
    assert_eq!(file.read(&mut bytes).unwrap(), 0);
    assert_eq!(file.read_at(1, &mut bytes).unwrap(), 3);
    assert_eq!(&bytes, b"bcd");
    let stat = file.metadata().unwrap();
    assert_eq!((stat.permissions, stat.user, stat.group), (0o640, 1000, 1001));
    assert_eq!(stat.size, 6);

    drop(file);
    let second = harness.open(FileAccess::Read);
    drop(second);
    harness.finish();
}

#[test]
fn descriptor_dup_and() {
    let harness = Harness::new();
    let state = Arc::clone(&harness.state);
    let file = harness.open(FileAccess::Read);
    let table = DescriptorTable::new(8).unwrap();
    let object: Arc<dyn OpenFileDescription> = file.clone();
    let descriptor = table.install(0, object, DescriptorFlags::default()).unwrap();
    let duplicate = table.duplicate(descriptor, 0, DescriptorFlags::default()).unwrap();
    let child = table.fork();
    let mut byte = [0_u8; 1];
    assert_eq!(table.pin(descriptor).unwrap().read(&mut byte).unwrap(), 1);
    assert_eq!(byte, *b"a");
    assert_eq!(child.pin(duplicate).unwrap().read(&mut byte).unwrap(), 1);
    assert_eq!(byte, *b"b");

    table.close(descriptor).unwrap();
    table.close(duplicate).unwrap();
    drop(table);
    drop(child);
    assert!(matches!(file.read(&mut byte), Err(ObjectError::Retired)));
    drop(file);

    let second = harness.open(FileAccess::Read);
    drop(second);
    harness.finish();
    assert_eq!(state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).closes, 2);
}

#[derive(Default)]
struct Observer {
    calls: Mutex<usize>,
}

impl ReadinessObserver for Observer {
    fn readiness_changed(&self) {
        *self.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
    }
}

#[test]
fn readiness_subscription_status() {
    let harness = Harness::new();
    let file = harness.open(FileAccess::ReadWrite);
    let observer = Arc::new(Observer::default());
    let observer_port: Arc<dyn ReadinessObserver> = observer.clone();
    let subscription = file.subscribe_readiness(observer_port).unwrap();
    let ready = file.readiness(Readiness::from_bits(Readiness::READ | Readiness::WRITE));
    assert_eq!(ready.bits(), Readiness::READ | Readiness::WRITE);
    assert_eq!(
        *observer.calls.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
        1
    );
    file.set_status_flags(hl_descriptor::StatusFlags::from_bits(
        hl_descriptor::StatusFlags::NONBLOCKING | 2,
    ))
    .unwrap();
    let rebind = file.release_for_rebind().unwrap();
    assert_eq!(rebind.snapshot().path, b"/projected/file");
    assert!(matches!(file.metadata(), Err(FileError::Retired)));
    let rebound = harness.files.rebind(rebind).unwrap();
    assert_eq!(
        rebound.snapshot().unwrap().status.bits(),
        hl_descriptor::StatusFlags::NONBLOCKING | 2
    );
    subscription.quiesce();
    drop(file);
    drop(rebound);
    let second = harness.open(FileAccess::Read);
    drop(second);
    harness.finish();
}

#[test]
fn linux_errors_map() {
    let harness = Harness::new();
    let file = harness.open(FileAccess::Read);
    harness
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .next_errno = Some(11);
    let mut output = [0xa5; 4];
    assert_eq!(file.read(&mut output), Err(ObjectError::WouldBlock));
    assert_eq!(output, [0xa5; 4]);
    drop(file);
    let second = harness.open(FileAccess::Read);
    drop(second);
    harness.finish();
}
