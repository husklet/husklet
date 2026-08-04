use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use hl_memory::{SharedLimits, SharedObjectStore};

use crate::{
    IpcCatalog, IpcCatalogError, IpcResourceKey, MessageLimits, MessageQueueNamespace, Pipe, PipeEndpointBinding,
    SemaphoreLimits, SemaphoreNamespace, SharedMemoryLimits, SharedMemoryNamespace,
};

struct Binding;

impl PipeEndpointBinding for Binding {
    fn bind(&self, _: Arc<crate::PipeEndpoint>) -> Result<(), crate::IpcCheckpointError> {
        Ok(())
    }
}

struct CatalogScenario;

impl CatalogScenario {
    fn catalog() -> IpcCatalog {
        let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        IpcCatalog::new(
            Arc::new(SharedMemoryNamespace::new(memory, SharedMemoryLimits::default()).unwrap()),
            SharedMemoryLimits::default(),
            Vec::new(),
            Arc::new(MessageQueueNamespace::new(MessageLimits::default()).unwrap()),
            MessageLimits::default(),
            Arc::new(SemaphoreNamespace::new(SemaphoreLimits::default()).unwrap()),
            SemaphoreLimits::default(),
            Vec::new(),
        )
    }
}

#[test]
fn pipe_generation_and() {
    let catalog = CatalogScenario::catalog();
    let stale = catalog
        .insert_pipe(
            Arc::new(Pipe::new(true)),
            IpcResourceKey::new(1).unwrap(),
            IpcResourceKey::new(2).unwrap(),
            Arc::new(Binding),
            Arc::new(Binding),
        )
        .unwrap();
    catalog.remove_pipe(stale).unwrap();
    let pipe = Arc::new(Pipe::new(true));
    hl_descriptor::OpenFileDescription::write(&*pipe.writer, b"ordered").unwrap();
    let current = catalog
        .insert_pipe(
            pipe,
            IpcResourceKey::new(3).unwrap(),
            IpcResourceKey::new(4).unwrap(),
            Arc::new(Binding),
            Arc::new(Binding),
        )
        .unwrap();
    assert_ne!(stale.generation, current.generation);
    assert_eq!(catalog.with_pipe(stale, |_| ()), Err(IpcCatalogError::Stale));
    catalog.freeze_checkpoint();
    let image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    assert_eq!(image.pipe_generations, vec![current.generation]);
    assert_eq!(image.pipes[0].snapshot.bytes, b"ordered");
}

#[test]
fn freeze_waits_for() {
    let catalog = Arc::new(CatalogScenario::catalog());
    let pipe = catalog
        .insert_pipe(
            Arc::new(Pipe::new(true)),
            IpcResourceKey::new(1).unwrap(),
            IpcResourceKey::new(2).unwrap(),
            Arc::new(Binding),
            Arc::new(Binding),
        )
        .unwrap();
    let (entered_send, entered) = mpsc::channel();
    let (release_send, release) = mpsc::channel();
    let worker_catalog = catalog.clone();
    let worker = thread::spawn(move || {
        worker_catalog
            .with_pipe(pipe, |_| {
                entered_send.send(()).unwrap();
                release.recv().unwrap();
            })
            .unwrap();
    });
    entered.recv().unwrap();
    let freeze_catalog = catalog.clone();
    let (frozen_send, frozen) = mpsc::channel();
    let freezer = thread::spawn(move || {
        freeze_catalog.freeze_checkpoint();
        frozen_send.send(()).unwrap();
    });
    assert!(frozen.recv_timeout(Duration::from_millis(20)).is_err());
    release_send.send(()).unwrap();
    frozen.recv_timeout(Duration::from_secs(1)).unwrap();
    catalog.thaw_checkpoint();
    worker.join().unwrap();
    freezer.join().unwrap();
}

#[test]
fn unsupported_bypassing_pipe() {
    let catalog = CatalogScenario::catalog();
    let pipe = Arc::new(Pipe::new(false));
    catalog
        .insert_pipe(
            pipe.clone(),
            IpcResourceKey::new(1).unwrap(),
            IpcResourceKey::new(2).unwrap(),
            Arc::new(Binding),
            Arc::new(Binding),
        )
        .unwrap();
    let (started_send, started) = mpsc::channel();
    let reader = pipe.reader.clone();
    let worker = thread::spawn(move || {
        started_send.send(()).unwrap();
        let mut byte = [0_u8; 1];
        hl_descriptor::OpenFileDescription::read(&*reader, &mut byte)
    });
    started.recv().unwrap();
    thread::sleep(Duration::from_millis(20));
    catalog.freeze_checkpoint();
    assert_eq!(catalog.checkpoint_image(), Err(IpcCatalogError::Busy));
    catalog.thaw_checkpoint();
    hl_descriptor::OpenFileDescription::write(&*pipe.writer, b"x").unwrap();
    assert_eq!(worker.join().unwrap(), Ok(1));
}
