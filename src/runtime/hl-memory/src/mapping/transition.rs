use std::sync::Arc;

use super::host::Coordinator;
use super::port::Host;

/// Consumer hook around one complete mapping publication transaction.
pub trait TransitionObserver: std::fmt::Debug + Send + Sync {
    fn begin(&self) {}
    fn published(&self, _generation: u64) {}
    fn end(&self) {}
}

#[derive(Debug, Default)]
pub(crate) struct NoopObserver;

impl TransitionObserver for NoopObserver {}

pub(crate) struct Transition {
    observer: Arc<dyn TransitionObserver>,
}

impl Transition {
    pub(crate) fn published(&mut self, generation: u64) {
        self.observer.published(generation);
    }
}

impl Drop for Transition {
    fn drop(&mut self) {
        self.observer.end();
    }
}

impl<H: Host> Coordinator<H> {
    #[must_use]
    pub fn with_transition_observer(self, observer: Arc<dyn TransitionObserver>) -> Self {
        self.set_transition_observer(observer);
        self
    }

    pub fn set_transition_observer(&self, observer: Arc<dyn TransitionObserver>) {
        *self.observer.write().unwrap_or_else(|error| error.into_inner()) = observer;
    }

    pub(crate) fn transition(&self) -> Transition {
        let observer = Arc::clone(&self.observer.read().unwrap_or_else(|error| error.into_inner()));
        observer.begin();
        Transition { observer }
    }
}
