use std::sync::Arc;

use hl_descriptor::{CancellationNotification, CancellationSubscription, OperationCancellation};
use hl_linux::{Errno, LinuxResult, RestartKind};
use hl_sync::{Interruption, InterruptionObservation, InterruptionWake};
use hl_task::{TaskRegistry, ThreadId};

use crate::PipeCancellationPort;

pub struct RuntimePipeCancellation {
    interruption: Arc<Interruption>,
    signal: Option<(Arc<TaskRegistry>, ThreadId)>,
}

impl RuntimePipeCancellation {
    #[must_use]
    pub const fn new(interruption: Arc<Interruption>) -> Self {
        Self {
            interruption,
            signal: None,
        }
    }

    #[must_use]
    pub fn with_signals(mut self, tasks: Arc<TaskRegistry>, thread: ThreadId) -> Self {
        self.signal = Some((tasks, thread));
        self
    }
}

struct NotificationWake(Arc<dyn CancellationNotification>);

impl InterruptionWake for NotificationWake {
    fn wake(&self) {
        self.0.notify();
    }
}

struct RuntimeCancellationSubscription {
    _wake: Arc<NotificationWake>,
    _observation: InterruptionObservation,
}

impl CancellationSubscription for RuntimeCancellationSubscription {}

impl OperationCancellation for RuntimePipeCancellation {
    fn interrupted(&self) -> bool {
        self.interruption.take_pending()
    }

    fn subscribe(&self, notification: Arc<dyn CancellationNotification>) -> Box<dyn CancellationSubscription> {
        let wake = Arc::new(NotificationWake(notification));
        let observation = self.interruption.observe(wake.clone());
        Box::new(RuntimeCancellationSubscription {
            _wake: wake,
            _observation: observation,
        })
    }
}

impl PipeCancellationPort for RuntimePipeCancellation {
    fn observation(&self) -> &dyn OperationCancellation {
        self
    }

    fn interrupted_result(&self) -> LinuxResult {
        match self
            .signal
            .as_ref()
            .and_then(|(tasks, thread)| tasks.restart_interrupted_signal(*thread).ok())
            .flatten()
        {
            Some(true) => LinuxResult::Restart(RestartKind::NoInterrupt),
            Some(false) | None => LinuxResult::Error(Errno::EINTR),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::PipeCancellationPort;
    use hl_task::{
        PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig, SignalAction, SignalDisposition, SignalInfo,
        SignalMask, SignalNumber,
    };

    fn fixture() -> (Arc<TaskRegistry>, hl_task::ProcessId, ThreadId) {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(0, 0, &[], 32).unwrap();
        let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        (tasks, process, thread)
    }

    fn action(flags: u64) -> SignalAction {
        SignalAction {
            disposition: SignalDisposition::Handler(1),
            flags,
            restorer: 0,
            mask: SignalMask::from_bits(0),
        }
    }

    #[test]
    fn restart_requires_all_handlers() {
        let (tasks, process, thread) = fixture();
        let restart = SignalNumber::new(14).unwrap();
        tasks.set_action(process, restart, action(0x1000_0000)).unwrap();
        tasks
            .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(restart))
            .unwrap();
        let cancellation =
            RuntimePipeCancellation::new(Arc::new(Interruption::new())).with_signals(Arc::clone(&tasks), thread);
        assert_eq!(
            cancellation.interrupted_result(),
            LinuxResult::Restart(RestartKind::NoInterrupt)
        );

        let interrupt = SignalNumber::new(10).unwrap();
        tasks.set_action(process, interrupt, action(0)).unwrap();
        tasks
            .enqueue_signal(PendingTarget::Thread(thread), SignalInfo::bare(interrupt))
            .unwrap();
        assert_eq!(cancellation.interrupted_result(), LinuxResult::Error(Errno::EINTR));
    }

    #[test]
    fn lifecycle_cancel_stays_eintr() {
        let (tasks, _, thread) = fixture();
        let cancellation = RuntimePipeCancellation::new(Arc::new(Interruption::new())).with_signals(tasks, thread);
        assert_eq!(cancellation.interrupted_result(), LinuxResult::Error(Errno::EINTR));
    }
}
