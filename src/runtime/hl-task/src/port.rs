use crate::{ProcessGroupId, ProcessId, SessionId, ThreadId};
use std::sync::{Arc, Mutex, Weak};

/// Execution-owned flag polled by translated or native guest execution.
pub trait InterruptSink: Send + Sync {
    fn set_interrupted(&self, interrupted: bool);
}

/// Consumer-owned wake/cancellation capability implemented by execution.
pub trait CancellationSink {
    type Error;

    fn request_cancellation(&self, thread: ThreadId) -> Result<(), Self::Error>;
}

/// Consumer-owned pending-signal wake capability implemented by task/runtime
/// integration. Signal identity and frames remain outside this foundation.
pub trait SignalPendingSink {
    type Error;

    fn pending_changed(&self, thread: ThreadId, pending: bool) -> Result<(), Self::Error>;
}

/// Wake target for consumers waiting on a change to pending signals.
pub trait SignalActivityWake: Send + Sync {
    fn signal_activity_changed(&self);

    fn process_control_activity(&self, _: SignalActivityEvent) {
        self.signal_activity_changed();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessControlAction {
    Continue,
    Kill,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalActivityKind {
    Ordinary,
    ProcessControl {
        process: ProcessId,
        action: ProcessControlAction,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalActivityEvent {
    pub control_epoch: u64,
    pub kind: SignalActivityKind,
}

#[derive(Default)]
pub(crate) struct SignalActivity {
    sequence: Mutex<u64>,
    observers: Mutex<Vec<Weak<dyn SignalActivityWake>>>,
}

impl SignalActivity {
    pub(crate) fn observation(&self) -> u64 {
        *self.sequence.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn subscribe(self: &Arc<Self>, observer: Arc<dyn SignalActivityWake>) -> SignalActivitySubscription {
        self.observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(Arc::downgrade(&observer));
        SignalActivitySubscription {
            activity: self.clone(),
            observer,
        }
    }

    pub(crate) fn notify(&self, kind: SignalActivityKind, control_epoch: Option<u64>) {
        let generation = {
            let mut sequence = self.sequence.lock().unwrap_or_else(|error| error.into_inner());
            *sequence = sequence.wrapping_add(1);
            *sequence
        };
        let activity = SignalActivityEvent {
            control_epoch: control_epoch.unwrap_or(generation),
            kind,
        };
        let observers = {
            let mut entries = self.observers.lock().unwrap_or_else(|error| error.into_inner());
            let mut live = Vec::new();
            entries.retain(|entry| {
                let Some(observer) = entry.upgrade() else {
                    return false;
                };
                live.push(observer);
                true
            });
            live
        };
        for observer in observers {
            match activity.kind {
                SignalActivityKind::Ordinary => observer.signal_activity_changed(),
                SignalActivityKind::ProcessControl { .. } => observer.process_control_activity(activity),
            }
        }
    }
}

pub struct SignalActivitySubscription {
    activity: Arc<SignalActivity>,
    observer: Arc<dyn SignalActivityWake>,
}

impl Drop for SignalActivitySubscription {
    fn drop(&mut self) {
        let observer = Arc::downgrade(&self.observer);
        self.activity
            .observers
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .retain(|entry| !Weak::ptr_eq(entry, &observer) && entry.strong_count() != 0);
    }
}

/// Consumer-owned terminal-control boundary. A tty domain may project the
/// registry's foreground group without exposing a host descriptor here.
pub trait TerminalControl {
    type Error;

    fn foreground_changed(&self, session: SessionId, group: ProcessGroupId) -> Result<(), Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForegroundGroupEvent {
    pub session: SessionId,
    pub group: ProcessGroupId,
}

impl ForegroundGroupEvent {
    pub(crate) const fn new(session: SessionId, group: ProcessGroupId) -> Self {
        Self { session, group }
    }

    pub fn deliver<T: TerminalControl>(self, terminal: &T) -> Result<(), T::Error> {
        terminal.foreground_changed(self.session, self.group)
    }
}
