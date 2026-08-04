use hl_sync::{Interruption, WaitError, WaitOutcome, WaitQueue};
use hl_time::{ClockError, Deadline, MonotonicInstant};
use std::sync::Arc;

use hl_descriptor::{CancellationNotification, CancellationSubscription, OperationCancellation};

/// Current-thread wait services consumed by blocking network syscalls.
pub trait SocketWait: Send + Sync {
    fn interruption(&self) -> Arc<Interruption>;
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError>;
    fn wait(&self, queue: &WaitQueue, observed: u64, deadline: Option<Deadline>) -> Result<WaitOutcome, WaitError>;
}

pub struct SafeNetworkWait {
    interruption: Arc<Interruption>,
    clock: Arc<dyn hl_time::Clock>,
}

/// Adapts a task interruption to descriptor operations without introducing a
/// network-specific polling loop.
pub(crate) struct SocketCancellation {
    interruption: Arc<Interruption>,
}

impl SocketCancellation {
    pub(crate) fn new(interruption: Arc<Interruption>) -> Self {
        Self { interruption }
    }
}

struct NotificationWake(Arc<dyn CancellationNotification>);

impl hl_sync::InterruptionWake for NotificationWake {
    fn wake(&self) {
        self.0.notify();
    }
}

struct CancellationObservation {
    _wake: Arc<NotificationWake>,
    _observation: hl_sync::InterruptionObservation,
}

impl CancellationSubscription for CancellationObservation {}

impl OperationCancellation for SocketCancellation {
    fn interrupted(&self) -> bool {
        self.interruption.take_pending()
    }

    fn subscribe(&self, notification: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
        let wake = Arc::new(NotificationWake(notification));
        let observation = self.interruption.observe(wake.clone());
        Box::new(CancellationObservation {
            _wake: wake,
            _observation: observation,
        })
    }
}

impl SafeNetworkWait {
    #[must_use]
    pub fn new(interruption: Arc<Interruption>, clock: Arc<dyn hl_time::Clock>) -> Self {
        Self { interruption, clock }
    }
}

impl SocketWait for SafeNetworkWait {
    fn interruption(&self) -> Arc<Interruption> {
        self.interruption.clone()
    }

    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        self.clock.monotonic_now()
    }

    fn wait(&self, queue: &WaitQueue, observed: u64, deadline: Option<Deadline>) -> Result<WaitOutcome, WaitError> {
        queue.wait(observed, &self.interruption, deadline, self.clock.as_ref())
    }
}
