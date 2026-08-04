use hl_descriptor::OpenFileDescription;
use hl_ipc::{
    Credentials, IPC_CHECKPOINT_VERSION, IPC_PIPE_MAXIMUM, IpcCheckpointImage, IpcKey, IpcResourceKey, MessageLimits,
    MessageQueueNamespace, MessageQueueSnapshot, MsgGetRequest, Pipe, PipeCheckpoint, SEM_UNDO, SemGetRequest,
    SemaphoreLimits, SemaphoreNamespace, SemaphoreOperation, SemaphoreSnapshot, SharedBackingCheckpoint,
    SharedBackingKey, SharedMemoryLimits, SharedMemoryNamespace, SharedMemorySnapshot, ShmGetRequest, TaskCheckpoint,
};
use hl_memory::{SharedLimits, SharedObjectStore};

use super::{Codec, HEADER_LENGTH, MAGIC, VERSION};
use crate::IpcCheckpointCodec;

fn image() -> IpcCheckpointImage {
    IpcCheckpointImage {
        version: IPC_CHECKPOINT_VERSION,
        pipe_generations: Vec::new(),
        pipes: Vec::new(),
        shared_limits: SharedMemoryLimits::default(),
        shared: SharedMemorySnapshot {
            generations: Vec::new(),
            segments: Vec::new(),
            attachments: Vec::new(),
            next_attachment: 1,
        },
        backings: Vec::new(),
        message_limits: MessageLimits::default(),
        messages: MessageQueueSnapshot {
            generations: Vec::new(),
            queues: Vec::new(),
        },
        semaphore_limits: SemaphoreLimits::default(),
        semaphores: SemaphoreSnapshot {
            generations: Vec::new(),
            sets: Vec::new(),
            undo: Vec::new(),
        },
        tasks: Vec::new(),
    }
}

fn rich_image() -> IpcCheckpointImage {
    const OWNER: Credentials = Credentials { uid: 10, gid: 20 };
    let memory = std::sync::Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let shared = SharedMemoryNamespace::new(memory, SharedMemoryLimits::default()).unwrap();
    let segment = shared
        .shmget(ShmGetRequest {
            key: IpcKey(7),
            size: 4096,
            create: true,
            exclusive: false,
            mode: 0o600,
            actor: OWNER,
            pid: 100,
            now: 1,
        })
        .unwrap();
    let plan = shared.shmat_plan(segment, OWNER, 0).unwrap();
    shared.commit_attach(plan, 100, 2).unwrap();
    shared.remove(segment, OWNER, 100, 3).unwrap();
    let shared_snapshot = shared.snapshot();

    let messages = MessageQueueNamespace::new(MessageLimits::default()).unwrap();
    let queue = messages
        .msgget(MsgGetRequest {
            key: IpcKey(8),
            create: true,
            exclusive: false,
            mode: 0o600,
            actor: OWNER,
            pid: 100,
            now: 4,
        })
        .unwrap();
    messages.send(queue, OWNER, 100, 5, b"first", 0, 5).unwrap();
    messages.send(queue, OWNER, 100, 2, b"second", 0, 6).unwrap();

    let semaphores = SemaphoreNamespace::new(SemaphoreLimits::default()).unwrap();
    let set = semaphores
        .semget(SemGetRequest {
            key: IpcKey(9),
            semaphores: 1,
            create: true,
            exclusive: false,
            mode: 0o600,
            actor: OWNER,
            pid: 200,
            now: 7,
        })
        .unwrap();
    semaphores.set_value(set, 0, 2, OWNER, 200, 8).unwrap();
    semaphores
        .operate(
            set,
            OWNER,
            200,
            &[SemaphoreOperation {
                index: 0,
                delta: -1,
                flags: SEM_UNDO,
            }],
            9,
        )
        .unwrap();

    let pipe = Pipe::new_packet(true);
    pipe.writer.write(b"pipe").unwrap();
    IpcCheckpointImage {
        version: IPC_CHECKPOINT_VERSION,
        pipe_generations: vec![1],
        pipes: vec![PipeCheckpoint {
            id: hl_ipc::IpcPipeId { slot: 0, generation: 1 },
            snapshot: pipe.snapshot().unwrap(),
            reader: IpcResourceKey::new(11).unwrap(),
            writer: IpcResourceKey::new(12).unwrap(),
        }],
        shared_limits: SharedMemoryLimits::default(),
        shared: shared_snapshot.clone(),
        backings: vec![SharedBackingCheckpoint {
            segment,
            object: shared_snapshot.segments[0].backing,
            resource: SharedBackingKey::new(13).unwrap(),
        }],
        message_limits: MessageLimits::default(),
        messages: messages.snapshot(),
        semaphore_limits: SemaphoreLimits::default(),
        semaphores: semaphores.snapshot(),
        tasks: vec![
            TaskCheckpoint {
                process: 100,
                resource: IpcResourceKey::new(14).unwrap(),
            },
            TaskCheckpoint {
                process: 200,
                resource: IpcResourceKey::new(15).unwrap(),
            },
        ],
    }
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

fn framed(payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MAGIC.to_le_bytes());
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&checksum(payload).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

#[test]
fn portable_round_trip() {
    let image = image();
    let bytes = crate::PortableIpcCodec.encode(&image).unwrap();
    assert_eq!(crate::PortableIpcCodec.decode(&bytes).unwrap(), image);
}

#[test]
fn rich_state_roundtrip() {
    let image = rich_image();
    let bytes = crate::PortableIpcCodec.encode(&image).unwrap();
    let restored = crate::PortableIpcCodec.decode(&bytes).unwrap();
    assert_eq!(restored, image);
    assert!(restored.shared.segments[0].marked_for_removal);
    assert_eq!(restored.messages.queues[0].messages[0].message_type, 5);
    assert_eq!(restored.messages.queues[0].messages[1].bytes, b"second");
    assert_eq!(restored.semaphores.undo[0].0, 200);
    assert_eq!(restored.shared.attachments[0].2, 100);
}

#[test]
fn corruption_rejected() {
    let original = Codec::encode(&image()).unwrap();
    for offset in 0..original.len() {
        let mut corrupt = original.clone();
        corrupt[offset] ^= 0x80;
        assert!(Codec::decode(&corrupt).is_err(), "accepted byte {offset}");
    }
    let mut trailing = original;
    trailing.push(0);
    assert!(Codec::decode(&trailing).is_err());
}

#[test]
fn invalid_input_rejected() {
    let encoded = Codec::encode(&image()).unwrap();
    let mut payload = encoded[HEADER_LENGTH..encoded.len() - 1].to_vec();
    payload.extend_from_slice(b",\"unknown\":0}");
    assert!(Codec::decode(&framed(&payload)).is_err());

    let mut oversized = image();
    oversized.pipe_generations = vec![1; IPC_PIPE_MAXIMUM + 1];
    assert!(Codec::encode(&oversized).is_err());
    assert!(Codec::decode(&vec![0; super::IPC_CHECKPOINT_BYTES_MAXIMUM + 1]).is_err());
}

#[test]
fn semantic_corruption() {
    let encoded = Codec::encode(&image()).unwrap();
    let payload = std::str::from_utf8(&encoded[HEADER_LENGTH..]).unwrap();
    let corrupt = payload.replace("\"next_attachment\":1", "\"next_attachment\":0");
    assert_ne!(corrupt, payload);
    assert!(Codec::decode(&framed(corrupt.as_bytes())).is_err());
}

#[test]
fn empty_image_golden() {
    let bytes = Codec::encode(&image()).unwrap();
    assert_eq!(bytes.len(), 419);
    assert_eq!(checksum(&bytes), 0x924c_c446_3cfd_2185);
}
