use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use hl_checkpoint::{Section, SectionKind};
use hl_event::{
    EVENT_CHECKPOINT_VERSION, Epoll, EpollSnapshot, EpollTargetCheckpoint, EventCatalog, EventCatalogRestore,
    EventCheckpointError, EventCheckpointImage, EventCheckpointRebind, EventFd, EventFdFlags, EventResourceKey,
    Inotify, InotifySnapshot, InotifyWatchCheckpoint, SignalFd, SignalFdSnapshot, TimerFd, TimerFdSnapshot,
};

use crate::{CheckpointEventCatalog, CheckpointParticipant, EventCheckpointParticipant, EventWireCodec};

#[derive(Default)]
struct State {
    failure: AtomicUsize,
    rollbacks: AtomicUsize,
    rebound: AtomicUsize,
}

struct Rebind {
    state: Arc<State>,
}

struct Transaction {
    state: Arc<State>,
}

impl EventCatalogRestore for Transaction {
    fn timerfd(&mut self, _: TimerFdSnapshot, _: EventResourceKey) -> Result<Arc<TimerFd>, EventCheckpointError> {
        Err(EventCheckpointError::InvalidImage)
    }
    fn signalfd(&mut self, _: SignalFdSnapshot, _: EventResourceKey) -> Result<Arc<SignalFd>, EventCheckpointError> {
        Err(EventCheckpointError::InvalidImage)
    }
    fn epoll(&mut self, _: &EpollSnapshot, _: &[EpollTargetCheckpoint]) -> Result<Arc<Epoll>, EventCheckpointError> {
        let rebound = self.state.rebound.fetch_add(1, Ordering::Relaxed) + 1;
        if self.state.failure.load(Ordering::Relaxed) == 4 && rebound == 2 {
            return Err(EventCheckpointError::InvalidImage);
        }
        Ok(Arc::new(Epoll::new()))
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
        if self.state.failure.load(Ordering::Relaxed) == 2 {
            return Err(EventCheckpointError::InvalidImage);
        }
        Ok(())
    }
    fn rollback(&mut self) {
        self.state.rollbacks.fetch_add(1, Ordering::Relaxed);
    }
    fn resume(&mut self) -> Result<(), EventCheckpointError> {
        if self.state.failure.load(Ordering::Relaxed) == 3 {
            return Err(EventCheckpointError::InvalidImage);
        }
        Ok(())
    }
}

#[test]
fn partial_catalog_publication() {
    let catalog = Arc::new(EventCatalog::new(2).unwrap());
    catalog.insert_epoll(Arc::new(Epoll::new()), Vec::new()).unwrap();
    catalog.insert_epoll(Arc::new(Epoll::new()), Vec::new()).unwrap();
    let handle = Arc::new(CheckpointEventCatalog::new(catalog));
    let state = Arc::new(State::default());
    state.failure.store(4, Ordering::Relaxed);
    let participant = EventCheckpointParticipant::new(
        handle.clone(),
        Arc::new(Rebind { state: state.clone() }),
        Arc::new(EventWireCodec),
    );
    let previous = handle.current();
    assert!(participant.stage(&Scenario::section(&participant)).is_err());
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.rebound.load(Ordering::Relaxed), 2);
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
}

impl EventCheckpointRebind for Rebind {
    fn stage(&self, _: &EventCheckpointImage) -> Result<Box<dyn EventCatalogRestore>, EventCheckpointError> {
        if self.state.failure.load(Ordering::Relaxed) == 1 {
            return Err(EventCheckpointError::InvalidImage);
        }
        Ok(Box::new(Transaction {
            state: self.state.clone(),
        }))
    }
}

fn fixture() -> (Arc<CheckpointEventCatalog>, Arc<State>, EventCheckpointParticipant) {
    let catalog = Arc::new(EventCatalog::new(2).unwrap());
    catalog
        .insert_eventfd(Arc::new(EventFd::new(7, EventFdFlags::default()).unwrap()))
        .unwrap();
    let handle = Arc::new(CheckpointEventCatalog::new(catalog));
    let state = Arc::new(State::default());
    let participant = EventCheckpointParticipant::new(
        handle.clone(),
        Arc::new(Rebind { state: state.clone() }),
        Arc::new(EventWireCodec),
    );
    (handle, state, participant)
}

struct Scenario;

impl Scenario {
    fn section(participant: &EventCheckpointParticipant) -> Section {
        participant.freeze().unwrap();
        let section = Section::new(
            SectionKind::new(5).unwrap(),
            EVENT_CHECKPOINT_VERSION,
            participant.snapshot().unwrap(),
        );
        participant.thaw().unwrap();
        section
    }

    fn assert_failure(failure: usize) {
        let (handle, state, participant) = fixture();
        let previous = handle.current();
        state.failure.store(failure, Ordering::Relaxed);
        let staged = participant.stage(&Self::section(&participant));
        if let Ok(reservation) = staged {
            let _ = participant
                .commit(reservation)
                .and_then(|()| participant.resume(reservation));
            participant.rollback(reservation);
        }
        assert!(Arc::ptr_eq(&handle.current(), &previous));
    }
}

#[test]
fn participant_releases_previous() {
    let (handle, _, participant) = fixture();
    let previous = handle.current();
    let weak = Arc::downgrade(&previous);
    let reservation = participant.stage(&Scenario::section(&participant)).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    drop(previous);
    assert!(weak.upgrade().is_some());
    participant.finish(reservation);
    assert!(weak.upgrade().is_none());
}

#[test]
fn external_previous_catalog() {
    for failure in 1..=3 {
        Scenario::assert_failure(failure);
    }
    let (handle, state, participant) = fixture();
    let previous = handle.current();
    let reservation = participant.stage(&Scenario::section(&participant)).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
}
