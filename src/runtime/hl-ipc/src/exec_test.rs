use std::sync::Arc;

use hl_memory::{SharedLimits, SharedObjectStore};

use crate::{
    Credentials, IPC_PRIVATE, IpcKey, SEM_UNDO, SemGetRequest, SemaphoreLimits, SemaphoreNamespace, SemaphoreOperation,
    SharedMemoryLimits, SharedMemoryNamespace, ShmGetRequest,
};

const OWNER: Credentials = Credentials { uid: 7, gid: 8 };

fn shared() -> Arc<SharedMemoryNamespace> {
    Arc::new(
        SharedMemoryNamespace::new(
            Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap()),
            SharedMemoryLimits::default(),
        )
        .unwrap(),
    )
}

#[test]
fn prepared_exec_detaches() {
    let namespace = shared();
    let mut tokens = Vec::new();
    for size in [4096, 8192] {
        let id = namespace
            .shmget(ShmGetRequest {
                key: IPC_PRIVATE,
                size,
                create: true,
                exclusive: false,
                mode: 0o600,
                actor: OWNER,
                pid: 10,
                now: 1,
            })
            .unwrap();
        let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
        tokens.push(namespace.commit_attach(plan, 10, 2).unwrap());
    }
    let before = namespace.snapshot();
    let prepared = namespace.prepare_exec(10, 3).unwrap();
    assert_eq!(prepared.attachments(), tokens);
    assert_eq!(namespace.snapshot(), before);
    let committed = prepared.commit().unwrap();
    assert!(namespace.snapshot().attachments.is_empty());
    committed.rollback().unwrap();
    assert_eq!(namespace.snapshot(), before);
}

#[test]
fn exec_detects_stale() {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let namespace = Arc::new(SharedMemoryNamespace::new(store.clone(), SharedMemoryLimits::default()).unwrap());
    let id = namespace
        .shmget(ShmGetRequest {
            key: IPC_PRIVATE,
            size: 4096,
            create: true,
            exclusive: false,
            mode: 0o600,
            actor: OWNER,
            pid: 10,
            now: 1,
        })
        .unwrap();
    let plan = namespace.shmat_plan(id, OWNER, 0).unwrap();
    let token = namespace.commit_attach(plan, 10, 2).unwrap();
    let stale = namespace.prepare_exec(10, 3).unwrap();
    namespace.shmdt(token, 10, 4).unwrap();
    assert!(stale.commit().is_err());

    let token = namespace
        .commit_attach(namespace.shmat_plan(id, OWNER, 0).unwrap(), 10, 5)
        .unwrap();
    namespace.remove(id, OWNER, 10, 6).unwrap();
    let committed = namespace.prepare_exec(10, 7).unwrap().commit().unwrap();
    assert!(namespace.metadata(id).is_err());
    committed.rollback().unwrap();
    assert_eq!(namespace.snapshot().attachments[0].0, token);
    namespace
        .prepare_exec(10, 8)
        .unwrap()
        .commit()
        .unwrap()
        .finish()
        .unwrap();
    assert!(store.snapshot().objects.is_empty());
}

#[test]
fn semaphore_exec_guard() {
    let namespace = Arc::new(SemaphoreNamespace::new(SemaphoreLimits::default()).unwrap());
    let id = namespace
        .semget(SemGetRequest {
            key: IpcKey(1),
            semaphores: 2,
            create: true,
            exclusive: false,
            mode: 0o600,
            actor: OWNER,
            pid: 10,
            now: 1,
        })
        .unwrap();
    namespace
        .operate(
            id,
            OWNER,
            10,
            &[
                SemaphoreOperation {
                    index: 0,
                    delta: 1,
                    flags: SEM_UNDO,
                },
                SemaphoreOperation {
                    index: 1,
                    delta: 2,
                    flags: SEM_UNDO,
                },
            ],
            2,
        )
        .unwrap();
    let before = namespace.snapshot();
    namespace.prepare_exec(10).commit().unwrap();
    assert_eq!(namespace.snapshot(), before);

    let stale = namespace.prepare_exec(10);
    namespace
        .operate(
            id,
            OWNER,
            10,
            &[SemaphoreOperation {
                index: 0,
                delta: 1,
                flags: SEM_UNDO,
            }],
            3,
        )
        .unwrap();
    assert!(stale.commit().is_err());
}
