use std::sync::Arc;
use std::thread;

use hl_memory::{SharedLimits, SharedObjectStore};

use crate::{
    Credentials, IPC_PRIVATE, IpcKey, MessageQueueId, SHM_RDONLY, SHM_REMAP, SHM_RND, SemaphoreId, SharedMemoryError,
    SharedMemoryId, SharedMemoryLimits, SharedMemoryLockIntent, SharedMemoryNamespace, ShmGetRequest,
};

#[test]
fn linux_ids_match() {
    assert_eq!(SharedMemoryId { slot: 7, generation: 1 }.linux_id(), Some(7));
    assert_eq!(SharedMemoryId { slot: 7, generation: 2 }.linux_id(), Some(4103));
    assert_eq!(
        SemaphoreId::from_linux_id(513),
        Some(SemaphoreId { slot: 1, generation: 2 })
    );
    assert_eq!(
        MessageQueueId::from_linux_id(1026),
        Some(MessageQueueId { slot: 2, generation: 3 })
    );
    assert_eq!(MessageQueueId::from_linux_id(-1), None);
}

const OWNER: Credentials = Credentials { uid: 10, gid: 20 };
const OTHER: Credentials = Credentials { uid: 11, gid: 21 };

struct Fixture;

impl Fixture {
    fn namespace(limits: SharedMemoryLimits) -> SharedMemoryNamespace {
        let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        SharedMemoryNamespace::new(memory, limits).unwrap()
    }

    fn request(key: IpcKey, size: usize) -> ShmGetRequest {
        ShmGetRequest {
            key,
            size,
            create: true,
            exclusive: false,
            mode: 0o640,
            actor: OWNER,
            pid: 100,
            now: 1,
        }
    }
}

#[test]
fn keys_private_exclusive() {
    let namespace = Fixture::namespace(SharedMemoryLimits::default());
    let id = namespace.shmget(Fixture::request(IpcKey(7), 4096)).unwrap();
    assert_eq!(namespace.shmget(Fixture::request(IpcKey(7), 2048)), Ok(id));
    let mut exclusive = Fixture::request(IpcKey(7), 4096);
    exclusive.exclusive = true;
    assert_eq!(namespace.shmget(exclusive), Err(SharedMemoryError::Exists));
    assert_eq!(
        namespace.shmget(Fixture::request(IpcKey(7), 8192)),
        Err(SharedMemoryError::Size)
    );
    assert_eq!(
        namespace.shmat_plan(id, OTHER, SHM_RDONLY),
        Err(SharedMemoryError::Permission)
    );
    assert_eq!(
        namespace.set_permissions(id, OTHER, OTHER, 0o600, 101, 2),
        Err(SharedMemoryError::Permission)
    );
    namespace.set_permissions(id, OWNER, OTHER, 0o600, 100, 2).unwrap();
    let plan = namespace
        .shmat_plan(id, OTHER, SHM_RDONLY | SHM_RND | SHM_REMAP)
        .unwrap();
    assert!(plan.read_only && plan.round_address && plan.replace);
    assert_eq!(
        namespace.shmat_plan(id, OTHER, 1),
        Err(SharedMemoryError::InvalidArgument)
    );
    assert_ne!(
        namespace.shmget(Fixture::request(IPC_PRIVATE, 4096)).unwrap(),
        namespace.shmget(Fixture::request(IPC_PRIVATE, 4096)).unwrap()
    );
}

#[test]
fn lock_authorization_is_generation_safe_and_state_free() {
    let namespace = Fixture::namespace(SharedMemoryLimits::default());
    let id = namespace.shmget(Fixture::request(IpcKey(70), 4096)).unwrap();
    let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
    let attachment = namespace.commit_attach(plan, 100, 2).unwrap();
    namespace
        .set_permissions(id, OWNER, Credentials { uid: 12, gid: 22 }, 0o004, 101, 3)
        .unwrap();
    let before = namespace.snapshot();

    for (actor, intent) in [
        (Credentials { uid: 0, gid: 0 }, SharedMemoryLockIntent::Lock),
        (Credentials { uid: 12, gid: 22 }, SharedMemoryLockIntent::Unlock),
        (OWNER, SharedMemoryLockIntent::Lock),
    ] {
        assert_eq!(namespace.authorize_lock(id, actor, intent), Ok(()));
        assert_eq!(namespace.snapshot(), before);
    }
    assert_eq!(
        namespace.authorize_lock(id, OTHER, SharedMemoryLockIntent::Lock),
        Err(SharedMemoryError::Permission)
    );
    assert_eq!(namespace.snapshot(), before);

    namespace.remove(id, OWNER, 100, 4).unwrap();
    assert_eq!(
        namespace.authorize_lock(id, OWNER, SharedMemoryLockIntent::Unlock),
        Err(SharedMemoryError::Removed)
    );
    namespace.shmdt(attachment, 100, 5).unwrap();
    let replacement = namespace.shmget(Fixture::request(IpcKey(71), 4096)).unwrap();
    assert_eq!(replacement.slot, id.slot);
    assert_ne!(replacement.generation, id.generation);
    assert_eq!(
        namespace.authorize_lock(id, OWNER, SharedMemoryLockIntent::Lock),
        Err(SharedMemoryError::NotFound)
    );
}

#[test]
fn limits_reject_without() {
    let limits = SharedMemoryLimits {
        segments: 1,
        segment_bytes: 4096,
        total_bytes: 4096,
        attachments: 1,
    };
    let namespace = Fixture::namespace(limits);
    let id = namespace.shmget(Fixture::request(IpcKey(1), 4096)).unwrap();
    assert_eq!(
        namespace.shmget(Fixture::request(IpcKey(2), 1)),
        Err(SharedMemoryError::ResourceLimit)
    );
    let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
    namespace.commit_attach(plan, 100, 2).unwrap();
    assert_eq!(
        namespace.commit_attach(plan, 100, 3),
        Err(SharedMemoryError::ResourceLimit)
    );
    assert_eq!(namespace.metadata(id).unwrap().attaches, 1);
}

#[test]
fn rmid_is_deferred() {
    let namespace = Fixture::namespace(SharedMemoryLimits::default());
    let old = namespace.shmget(Fixture::request(IpcKey(3), 4096)).unwrap();
    let plan = namespace.shmat_plan(old, OWNER, 0).unwrap();
    let attachment = namespace.commit_attach(plan, 100, 2).unwrap();
    namespace.remove(old, OWNER, 100, 3).unwrap();
    assert!(namespace.metadata(old).unwrap().marked_for_removal);
    assert_eq!(namespace.shmat_plan(old, OWNER, 0), Err(SharedMemoryError::Removed));
    namespace.shmdt(attachment, 100, 4).unwrap();
    assert_eq!(namespace.metadata(old), Err(SharedMemoryError::NotFound));
    let new = namespace.shmget(Fixture::request(IpcKey(3), 4096)).unwrap();
    assert_eq!(new.slot, old.slot);
    assert_ne!(new.generation, old.generation);
}

#[test]
fn fork_inherits_and() {
    let namespace = Fixture::namespace(SharedMemoryLimits::default());
    let id = namespace.shmget(Fixture::request(IpcKey(4), 4096)).unwrap();
    let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
    namespace.commit_attach(plan, 100, 2).unwrap();
    assert_eq!(namespace.fork(100, 200, 3).unwrap().len(), 1);
    assert_eq!(namespace.metadata(id).unwrap().attaches, 2);
    namespace.exit(100, 4).unwrap();
    assert_eq!(namespace.metadata(id).unwrap().attaches, 1);
    namespace.remove(id, OWNER, 200, 5).unwrap();
    namespace.exit(200, 6).unwrap();
    assert_eq!(namespace.metadata(id), Err(SharedMemoryError::NotFound));
}

#[test]
fn prepared_fork_drop() {
    let namespace = Fixture::namespace(SharedMemoryLimits::default());
    for key in [IpcKey(40), IpcKey(41)] {
        let id = namespace.shmget(Fixture::request(key, 4096)).unwrap();
        let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
        namespace.commit_attach(plan, 100, 2).unwrap();
    }
    let before = namespace.snapshot();
    let prepared = namespace.prepare_fork(100, 200, 7).unwrap();
    assert_eq!(prepared.inherited().len(), 2);
    drop(prepared);
    assert_eq!(namespace.snapshot(), before);
}

#[test]
fn stale_prepared_fork() {
    let namespace = Fixture::namespace(SharedMemoryLimits::default());
    let id = namespace.shmget(Fixture::request(IpcKey(42), 4096)).unwrap();
    let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
    namespace.commit_attach(plan, 100, 2).unwrap();
    let prepared = namespace.prepare_fork(100, 200, 7).unwrap();

    let concurrent = namespace.shmat_plan(id, OWNER, 0).unwrap();
    namespace.commit_attach(concurrent, 300, 8).unwrap();
    let before_commit = namespace.snapshot();
    assert_eq!(prepared.commit(), Err(SharedMemoryError::InvalidArgument),);
    assert_eq!(namespace.snapshot(), before_commit);
    assert!(
        namespace
            .snapshot()
            .attachments
            .iter()
            .all(|(_, _, process)| *process != 200)
    );
}

#[test]
fn prepared_fork_capacity() {
    let limits = SharedMemoryLimits {
        attachments: 3,
        ..SharedMemoryLimits::default()
    };
    let namespace = Fixture::namespace(limits);
    for key in [IpcKey(43), IpcKey(44)] {
        let id = namespace.shmget(Fixture::request(key, 4096)).unwrap();
        let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
        namespace.commit_attach(plan, 100, 2).unwrap();
    }
    let before_capacity = namespace.snapshot();
    assert!(matches!(
        namespace.prepare_fork(100, 200, 7),
        Err(SharedMemoryError::ResourceLimit),
    ));
    assert_eq!(namespace.snapshot(), before_capacity);

    let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let source = SharedMemoryNamespace::new(
        Arc::clone(&memory) as Arc<dyn crate::SharedBackingAccess>,
        SharedMemoryLimits::default(),
    )
    .unwrap();
    let id = source.shmget(Fixture::request(IpcKey(45), 4096)).unwrap();
    let plan = source.shmat_plan(id, OWNER, 0).unwrap();
    source.commit_attach(plan, 100, 2).unwrap();
    let mut boundary = source.snapshot();
    boundary.next_attachment = u64::MAX;
    let restored = SharedMemoryNamespace::restore(memory, SharedMemoryLimits::default(), boundary.clone()).unwrap();
    assert!(matches!(
        restored.prepare_fork(100, 200, 7),
        Err(SharedMemoryError::ResourceLimit),
    ));
    assert_eq!(restored.snapshot(), boundary);
}

#[test]
fn pointer_free_snapshot() {
    let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let namespace = SharedMemoryNamespace::new(memory.clone(), SharedMemoryLimits::default()).unwrap();
    let id = namespace.shmget(Fixture::request(IpcKey(5), 4096)).unwrap();
    let plan = namespace.shmat_plan(id, OWNER, SHM_RDONLY).unwrap();
    namespace.commit_attach(plan, 100, 2).unwrap();
    let snapshot = namespace.snapshot();
    let restored =
        SharedMemoryNamespace::restore(memory.clone(), SharedMemoryLimits::default(), snapshot.clone()).unwrap();
    assert_eq!(restored.snapshot(), snapshot);
    assert_eq!(restored.metadata(id).unwrap().attaches, 1);
    memory.remove(snapshot.segments[0].backing).unwrap();
    assert!(matches!(
        SharedMemoryNamespace::restore(memory, SharedMemoryLimits::default(), snapshot,),
        Err(SharedMemoryError::Shared(_))
    ));
}

#[test]
fn concurrent_same_key() {
    let namespace = Arc::new(Fixture::namespace(SharedMemoryLimits::default()));
    let workers: Vec<_> = (0..16)
        .map(|_| {
            let namespace = namespace.clone();
            thread::spawn(move || namespace.shmget(Fixture::request(IpcKey(9), 4096)).unwrap())
        })
        .collect();
    let ids: Vec<_> = workers.into_iter().map(|worker| worker.join().unwrap()).collect();
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(namespace.snapshot().segments.len(), 1);
}

#[test]
fn deterministic_lifecycle_model() {
    let namespace = Fixture::namespace(SharedMemoryLimits::default());
    let ids: Vec<_> = (0..8)
        .map(|key| namespace.shmget(Fixture::request(IpcKey(100 + key), 4096)).unwrap())
        .collect();
    let mut attachments = Vec::new();
    for (index, id) in ids.iter().copied().enumerate() {
        let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
        attachments.push((id, namespace.commit_attach(plan, 100 + index as u32, 2).unwrap()));
    }
    for (index, (id, token)) in attachments.into_iter().enumerate() {
        let pid = 100 + index as u32;
        if index % 2 == 0 {
            namespace.remove(id, OWNER, pid, 3).unwrap();
        }
        namespace.shmdt(token, pid, 4).unwrap();
        assert_eq!(namespace.metadata(id).is_ok(), index % 2 != 0,);
    }
}
