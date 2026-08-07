use std::sync::Arc;

use hl_isa::AddressRange;

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
        *self.observer.write().unwrap_or_else(std::sync::PoisonError::into_inner) = observer;
    }

    /// Publishes one committed mapping transition. Every such transition
    /// advances the executable era so range evidence witnesses reuse.
    pub(crate) fn publish_transition(&self, transition: &mut Transition, generation: u64) {
        self.host.executable.rotate();
        transition.published(generation);
    }

    /// A transition confined to known ranges can only change which bytes those
    /// ranges denote, so it supersedes them instead of the whole executable era.
    pub(crate) fn publish_transition_ranges(
        &self,
        transition: &mut Transition,
        generation: u64,
        ranges: impl IntoIterator<Item = AddressRange>,
    ) {
        let ranges: Vec<_> = ranges.into_iter().collect();
        if ranges.is_empty() {
            self.host.executable.rotate();
        } else {
            self.host.executable.publish(ranges);
        }
        transition.published(generation);
    }

    pub(crate) fn transition(&self) -> Transition {
        let observer = Arc::clone(&self.observer.read().unwrap_or_else(std::sync::PoisonError::into_inner));
        observer.begin();
        Transition { observer }
    }
}
