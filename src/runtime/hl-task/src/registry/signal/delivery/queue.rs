//! Signal enqueue, source-tagged queueing, and removal.

use crate::registry::TaskRegistry;
use crate::{DeliveryAction, PendingTarget, ProcessId, SignalInfo, SignalNumber, TaskError};

impl TaskRegistry {
    pub fn enqueue_signal(&self, target: PendingTarget, info: SignalInfo) -> Result<bool, TaskError> {
        let mut state = self.lock();
        let process = match target {
            PendingTarget::Process(process) => process,
            PendingTarget::Thread(thread) => Self::thread(&state, thread)?.process,
        };
        Self::ensure_process_unreserved(&state, process)?;
        let control_epoch = Self::apply_generation_effect(&mut state, process, info.signal, self.max_pending_signals)?;
        let action = Self::delivery_action(&state, process, info.signal)?;
        if matches!(action, DeliveryAction::Ignore) && !info.is_synchronous() {
            let resumes = info.signal == SignalNumber::CONTINUE;
            drop(state);
            if resumes {
                self.child_ready.notify_all();
                self.signals
                    .activity
                    .notify(Self::activity_kind(process, info.signal), control_epoch);
            }
            return Ok(false);
        }
        let result = match target {
            PendingTarget::Process(process) => Self::process_mut(&mut state, process)?
                .signals
                .pending
                .enqueue(info, self.max_pending_signals),
            PendingTarget::Thread(thread) => Self::thread_mut(&mut state, thread)?
                .signals
                .pending
                .enqueue(info, self.max_pending_signals),
        };
        let queued = result.map_err(Self::queue_error)?;
        let interrupts = if queued {
            match target {
                PendingTarget::Thread(thread) => vec![thread],
                PendingTarget::Process(process) => Self::process(&state, process)?.threads.iter().copied().collect(),
            }
        } else {
            Vec::new()
        };
        if queued {
            state.wait_epoch = state.wait_epoch.wrapping_add(1).max(1);
        }
        // Observers may query readiness and therefore re-enter the registry.
        // Publish the completed queue mutation before invoking them.
        drop(state);
        if queued {
            self.interrupt_queued(interrupts);
            self.child_ready.notify_all();
            self.signals
                .activity
                .notify(Self::activity_kind(process, info.signal), control_epoch);
        }
        Ok(queued)
    }

    /// Wakes the threads a freshly queued signal is visible to. A thread that has
    /// exited between the queue mutation and this wake has nothing to acknowledge.
    fn interrupt_queued(&self, threads: Vec<crate::ThreadId>) {
        for thread in threads {
            if let Err(error) = self.acknowledge_interrupt(thread) {
                hl_log::hl_debug!(
                    hl_log::tag::SIGNAL,
                    "queued signal could not interrupt {thread:?}: {error:?}"
                );
            }
        }
    }

    /// Enqueues an internally generated signal whose target may already be gone.
    pub(crate) fn enqueue_best_effort(&self, target: PendingTarget, info: SignalInfo, reason: &str) {
        if let Err(error) = self.enqueue_signal(target, info) {
            hl_log::hl_debug!(hl_log::tag::SIGNAL, "{reason} not queued for {target:?}: {error:?}");
        }
    }

    /// Queues at most one pending instance for an engine-owned source.
    ///
    /// Linux POSIX timers queue distinct timer identities independently while
    /// folding repeat expirations from the same timer into its overrun count.
    pub fn enqueue_source_signal(&self, target: PendingTarget, info: SignalInfo) -> Result<bool, TaskError> {
        let mut state = self.lock();
        let process = match target {
            PendingTarget::Process(process) => process,
            PendingTarget::Thread(thread) => Self::thread(&state, thread)?.process,
        };
        Self::ensure_process_unreserved(&state, process)?;
        if let PendingTarget::Thread(thread) = target {
            Self::ensure_thread_unreserved(&state, thread)?;
        }
        let control_epoch = Self::apply_generation_effect(&mut state, process, info.signal, self.max_pending_signals)?;
        let action = Self::delivery_action(&state, process, info.signal)?;
        if matches!(action, DeliveryAction::Ignore) && !info.is_synchronous() {
            let resumes = info.signal == SignalNumber::CONTINUE;
            drop(state);
            if resumes {
                self.child_ready.notify_all();
                self.signals
                    .activity
                    .notify(Self::activity_kind(process, info.signal), control_epoch);
            }
            return Ok(false);
        }
        let result = match target {
            PendingTarget::Process(process) => Self::process_mut(&mut state, process)?
                .signals
                .pending
                .enqueue_unique_source(info, self.max_pending_signals),
            PendingTarget::Thread(thread) => Self::thread_mut(&mut state, thread)?
                .signals
                .pending
                .enqueue_unique_source(info, self.max_pending_signals),
        };
        let queued = result.map_err(Self::queue_error)?;
        let interrupts = if queued {
            match target {
                PendingTarget::Thread(thread) => vec![thread],
                PendingTarget::Process(process) => Self::process(&state, process)?.threads.iter().copied().collect(),
            }
        } else {
            Vec::new()
        };
        if queued {
            state.wait_epoch = state.wait_epoch.wrapping_add(1).max(1);
        }
        drop(state);
        if queued {
            self.interrupt_queued(interrupts);
            self.child_ready.notify_all();
            self.signals
                .activity
                .notify(Self::activity_kind(process, info.signal), control_epoch);
        }
        Ok(queued)
    }

    fn activity_kind(process: ProcessId, signal: SignalNumber) -> crate::SignalActivityKind {
        let action = match signal {
            SignalNumber::CONTINUE => Some(crate::ProcessControlAction::Continue),
            SignalNumber::KILL => Some(crate::ProcessControlAction::Kill),
            _ => None,
        };
        action.map_or(crate::SignalActivityKind::Ordinary, |action| {
            crate::SignalActivityKind::ProcessControl { process, action }
        })
    }

    /// Removes a still-pending instance owned by the exact source identity.
    pub fn remove_source_signal(
        &self,
        target: PendingTarget,
        signal: SignalNumber,
        source_tag: u32,
    ) -> Result<bool, TaskError> {
        let mut state = self.lock();
        let process = match target {
            PendingTarget::Process(process) => process,
            PendingTarget::Thread(thread) => Self::thread(&state, thread)?.process,
        };
        Self::ensure_process_unreserved(&state, process)?;
        if let PendingTarget::Thread(thread) = target {
            Self::ensure_thread_unreserved(&state, thread)?;
        }
        let removed = match target {
            PendingTarget::Process(process) => Self::process_mut(&mut state, process)?
                .signals
                .pending
                .remove_source(signal, source_tag),
            PendingTarget::Thread(thread) => Self::thread_mut(&mut state, thread)?
                .signals
                .pending
                .remove_source(signal, source_tag),
        };
        Ok(removed)
    }
}
