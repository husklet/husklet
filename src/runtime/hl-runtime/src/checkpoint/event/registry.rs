use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use hl_event::{
    EventCheckpointError, EventResourceKey, Inotify, InotifySnapshot, InotifyWatchCheckpoint, SignalFd,
    SignalFdSnapshot, SignalQueue, TimerClockSource, TimerFd, TimerFdSnapshot, WatchSource,
};

use super::ResourceRestore;

#[derive(Default)]
struct State {
    clocks: BTreeMap<EventResourceKey, Arc<dyn TimerClockSource>>,
    signals: BTreeMap<EventResourceKey, Arc<dyn SignalQueue>>,
    watches: BTreeMap<EventResourceKey, Arc<dyn WatchSource>>,
}

#[derive(Default)]
pub struct ResourceRegistry {
    state: RwLock<State>,
}

impl ResourceRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_clock(
        &self,
        key: EventResourceKey,
        clock: Arc<dyn TimerClockSource>,
    ) -> Result<(), EventCheckpointError> {
        let mut state = self.state.write().map_err(|_| EventCheckpointError::InvalidImage)?;
        if let Some(current) = state.clocks.get(&key) {
            return Arc::ptr_eq(current, &clock)
                .then_some(())
                .ok_or(EventCheckpointError::InvalidImage);
        }
        state.clocks.insert(key, clock);
        Ok(())
    }

    pub fn register_signal(
        &self,
        key: EventResourceKey,
        queue: Arc<dyn SignalQueue>,
    ) -> Result<(), EventCheckpointError> {
        let mut state = self.state.write().map_err(|_| EventCheckpointError::InvalidImage)?;
        if let Some(current) = state.signals.get(&key) {
            return Arc::ptr_eq(current, &queue)
                .then_some(())
                .ok_or(EventCheckpointError::InvalidImage);
        }
        state.signals.insert(key, queue);
        Ok(())
    }

    pub fn register_watch(
        &self,
        key: EventResourceKey,
        source: Arc<dyn WatchSource>,
    ) -> Result<(), EventCheckpointError> {
        let mut state = self.state.write().map_err(|_| EventCheckpointError::InvalidImage)?;
        if let Some(current) = state.watches.get(&key) {
            return Arc::ptr_eq(current, &source)
                .then_some(())
                .ok_or(EventCheckpointError::InvalidImage);
        }
        state.watches.insert(key, source);
        Ok(())
    }
}

impl ResourceRestore for ResourceRegistry {
    fn timerfd(
        &self,
        snapshot: TimerFdSnapshot,
        clock: EventResourceKey,
    ) -> Result<Arc<TimerFd>, EventCheckpointError> {
        let source = self
            .state
            .read()
            .map_err(|_| EventCheckpointError::InvalidImage)?
            .clocks
            .get(&clock)
            .cloned()
            .ok_or(EventCheckpointError::InvalidImage)?;
        TimerFd::from_snapshot(snapshot, source)
            .map(Arc::new)
            .map_err(|_| EventCheckpointError::InvalidImage)
    }

    fn signalfd(
        &self,
        snapshot: SignalFdSnapshot,
        queue: EventResourceKey,
    ) -> Result<Arc<SignalFd>, EventCheckpointError> {
        let source = self
            .state
            .read()
            .map_err(|_| EventCheckpointError::InvalidImage)?
            .signals
            .get(&queue)
            .cloned()
            .ok_or(EventCheckpointError::InvalidImage)?;
        SignalFd::from_snapshot(snapshot, source)
            .map(Arc::new)
            .map_err(|_| EventCheckpointError::InvalidImage)
    }

    fn inotify(
        &self,
        snapshot: &InotifySnapshot,
        source: EventResourceKey,
        watches: &[InotifyWatchCheckpoint],
    ) -> Result<Arc<Inotify>, EventCheckpointError> {
        if watches.len() != snapshot.watches.len() || watches.iter().any(|watch| watch.source != source) {
            return Err(EventCheckpointError::InvalidImage);
        }
        let template = self
            .state
            .read()
            .map_err(|_| EventCheckpointError::InvalidImage)?
            .watches
            .get(&source)
            .cloned()
            .ok_or(EventCheckpointError::InvalidImage)?;
        let restored = template
            .checkpoint_clone()
            .map_err(|_| EventCheckpointError::InvalidImage)?;
        Inotify::from_snapshot(snapshot, restored)
            .map(Arc::new)
            .map_err(|_| EventCheckpointError::InvalidImage)
    }
}
