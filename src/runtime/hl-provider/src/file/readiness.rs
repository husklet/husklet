//! Provider event projection into descriptor readiness observers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use hl_descriptor::{ObjectError, Readiness, ReadinessObserver, ReadinessRegistry, ReadinessSubscription};

use crate::{EventObserver, ProviderEvent};

#[derive(Debug)]
pub(crate) struct ReadinessState {
    ready: AtomicU32,
    observers: ReadinessRegistry,
}
pub(crate) type FileReadiness = ReadinessState;

impl FileReadiness {
    pub(crate) fn new(readiness: Readiness) -> Self {
        Self {
            ready: AtomicU32::new(readiness.bits()),
            observers: ReadinessRegistry::new(),
        }
    }

    pub(crate) fn current(&self) -> Readiness {
        Readiness::from_bits(self.ready.load(Ordering::Acquire))
    }

    pub(crate) fn update(&self, readiness: Readiness, lost: u64) {
        let mut bits = readiness.bits();
        if lost != 0 {
            bits |= Readiness::ERROR;
        }
        let previous = self.ready.swap(bits, Ordering::AcqRel);
        if previous != bits || lost != 0 {
            self.observers.notify();
        }
    }

    // Takes the observer by value so the subscription owns its share of the handle.
    #[allow(clippy::needless_pass_by_value)]
    pub(crate) fn subscribe(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        let subscription = self.observers.subscribe(Arc::clone(&observer))?;
        if self.ready.load(Ordering::Acquire) != 0 {
            observer.readiness_changed();
        }
        Ok(subscription)
    }

    pub(crate) fn close(&self) {
        self.observers.close();
    }

    pub(crate) const fn to_wire(readiness: Readiness) -> u8 {
        (if readiness.contains(Readiness::READ) { 1 } else { 0 })
            | (if readiness.contains(Readiness::WRITE) { 2 } else { 0 })
            | (if readiness.contains(Readiness::PRIORITY) { 4 } else { 0 })
    }

    pub(crate) const fn from_wire(wire: u8) -> Readiness {
        Readiness::from_bits(
            (if wire & 1 != 0 { Readiness::READ } else { 0 })
                | (if wire & 2 != 0 { Readiness::WRITE } else { 0 })
                | (if wire & 8 != 0 { Readiness::ERROR } else { 0 })
                | (if wire & 4 != 0 { Readiness::HANGUP } else { 0 }),
        )
    }
}

pub(crate) struct Observer {
    pub readiness: Arc<FileReadiness>,
}
pub(crate) type FileEventObserver = Observer;

impl EventObserver for FileEventObserver {
    fn provider_event(&self, event: ProviderEvent) {
        if event.payload.len() != 2 || event.payload[0] != 6 {
            self.readiness
                .update(Readiness::from_bits(Readiness::ERROR), event.lost.saturating_add(1));
            return;
        }
        self.readiness
            .update(FileReadiness::from_wire(event.payload[1]), event.lost);
    }
}
