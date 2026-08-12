use super::protocol::{ABI, MAGIC_REQUEST, STATUS_OK};
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

fn command(op: u32, stream: u64, length: u64, name_size: usize) -> Request {
    Request {
        op,
        stream,
        offset: 0,
        length,
        name_size,
    }
}

#[test]
fn object_group_commit_and_manifest_are_transactional() {
    let store = Arc::new(Store::default());
    let server = Server::new(store.clone(), store.clone());
    assert_eq!(
        server.dispatch(1, &command(GROUP_BEGIN, 0, 0, 7), "proc.1", &[]).status,
        STATUS_OK
    );
    assert_eq!(
        server
            .dispatch(1, &command(OBJECT_BEGIN, 4, 0, 12), "proc.1/meta", &[])
            .status,
        STATUS_OK
    );
    assert_eq!(
        server.dispatch(1, &command(OBJECT_WRITE, 4, 5, 0), "", b"state").status,
        STATUS_OK
    );
    assert_eq!(
        server.dispatch(1, &command(OBJECT_FINISH, 4, 0, 0), "", &[]).status,
        STATUS_OK
    );
    assert!(store.get("proc.1/meta").is_err());
    assert_eq!(
        server
            .dispatch(1, &command(GROUP_COMMIT, 0, 0, 7), "proc.1", &[])
            .status,
        STATUS_OK
    );
    assert_eq!(store.get("proc.1/meta").unwrap(), b"state");
    assert_eq!(
        server.dispatch(1, &command(COMMIT, 0, 8, 0), "", b"manifest").status,
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
