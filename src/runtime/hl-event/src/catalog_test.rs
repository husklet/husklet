use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use hl_descriptor::{DescriptionIdentity, Readiness};

use crate::{
    EVENT_CHECKPOINT_VERSION, Epoll, EpollInterest, EpollSnapshot, EpollTargetCheckpoint, EpollWatchKey,
    EpollWatchSnapshot, EventCatalog, EventCatalogError, EventCatalogRestore, EventCheckpointError,
    EventCheckpointImage, EventFd, EventFdFlags, EventObjectCheckpoint, EventObjectId, EventObjectState,
    EventResourceKey, Inotify, InotifySnapshot, InotifyWatchCheckpoint, SignalFd, SignalFdSnapshot, TimerFd,
    TimerFdSnapshot,
};

struct LocalRestore;

impl EventCatalogRestore for LocalRestore {
    fn timerfd(&mut self, _: TimerFdSnapshot, _: EventResourceKey) -> Result<Arc<TimerFd>, EventCheckpointError> {
        Err(EventCheckpointError::InvalidImage)
    }
    fn signalfd(&mut self, _: SignalFdSnapshot, _: EventResourceKey) -> Result<Arc<SignalFd>, EventCheckpointError> {
        Err(EventCheckpointError::InvalidImage)
    }
    fn epoll(&mut self, _: &EpollSnapshot, _: &[EpollTargetCheckpoint]) -> Result<Arc<Epoll>, EventCheckpointError> {
        Err(EventCheckpointError::InvalidImage)
    }
    fn inotify(
        &mut self,
        _: &InotifySnapshot,
        _: EventResourceKey,
        _: &[InotifyWatchCheckpoint],
    ) -> Result<Arc<Inotify>, EventCheckpointError> {
        Err(EventCheckpointError::InvalidImage)
    }
    fn commit(&mut self) -> Result<(), EventCheckpointError> {
        Ok(())
    }
    fn rollback(&mut self) {}
    fn resume(&mut self) -> Result<(), EventCheckpointError> {
        Ok(())
    }
}

#[test]
fn catalog_generation_restore() {
    let catalog = EventCatalog::new(2).unwrap();
    let stale = catalog
        .insert_eventfd(Arc::new(EventFd::new(1, EventFdFlags::default()).unwrap()))
        .unwrap();
    catalog.remove(stale).unwrap();
    let current = catalog
        .insert_eventfd(Arc::new(
            EventFd::new(7, EventFdFlags::from_bits(EventFdFlags::SEMAPHORE)).unwrap(),
        ))
        .unwrap();
    assert_ne!(current.generation, stale.generation);
    catalog.freeze_checkpoint();
    let image = catalog.checkpoint_image().unwrap();
    catalog.thaw_checkpoint();
    let restored = EventCatalog::restore_checkpoint(&image, &mut LocalRestore).unwrap();
    assert_eq!(restored.with_eventfd(current, EventFd::counter), Ok(7));
    assert_eq!(
        restored.with_eventfd(stale, EventFd::counter),
        Err(EventCatalogError::NotFound)
    );
}

#[test]
fn freeze_waits_access() {
    let catalog = Arc::new(EventCatalog::new(1).unwrap());
    let id = catalog
        .insert_eventfd(Arc::new(EventFd::new(0, EventFdFlags::default()).unwrap()))
        .unwrap();
    let (entered_send, entered) = mpsc::channel();
    let (release_send, release) = mpsc::channel();
    let worker_catalog = catalog.clone();
    let worker = thread::spawn(move || {
        worker_catalog
            .with_eventfd(id, |_| {
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
fn aggregate_validation_reference() {
    let first = EventObjectId { slot: 0, generation: 1 };
    let second = EventObjectId { slot: 1, generation: 1 };
    let watch = EpollWatchSnapshot {
        key: EpollWatchKey {
            descriptor_number: 3,
            descriptor_generation: 1,
            description: DescriptionIdentity {
                identity: 1,
                generation: 1,
            },
        },
        interests: EpollInterest::from_bits(EpollInterest::READ),
        data: 0,
        previous: Readiness::default(),
        disabled: false,
    };
    let state = |nested| EventObjectState::Epoll {
        snapshot: EpollSnapshot {
            watch_limit: 1,
            next_token: 2,
            epoch: 0,
            watches: vec![watch],
            ready: Vec::new(),
        },
        targets: vec![EpollTargetCheckpoint {
            watch: 0,
            descriptor: EventResourceKey::new(1).unwrap(),
            nested: Some(nested),
        }],
    };
    let image = EventCheckpointImage {
        version: EVENT_CHECKPOINT_VERSION,
        generations: vec![1, 1],
        objects: vec![
            EventObjectCheckpoint {
                id: first,
                state: state(second),
            },
            EventObjectCheckpoint {
                id: second,
                state: state(first),
            },
        ],
    };
    assert_eq!(image.validate(), Err(EventCheckpointError::Cycle));
    let mut stale = image;
    if let EventObjectState::Epoll { targets, .. } = &mut stale.objects[1].state {
        targets[0].nested = Some(EventObjectId { slot: 2, generation: 1 });
    }
    assert_eq!(stale.validate(), Err(EventCheckpointError::InvalidImage));
}
