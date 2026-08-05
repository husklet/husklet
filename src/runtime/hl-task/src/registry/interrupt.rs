use std::sync::Arc;

use super::TaskRegistry;
use crate::{InterruptSink, SignalDisposition, SignalMask, TaskError, ThreadId, ThreadLifecycle};

impl TaskRegistry {
    pub fn register_interrupt(&self, thread: ThreadId, sink: Arc<dyn InterruptSink>) -> Result<(), TaskError> {
        let state = self.lock();
        let task = Self::thread(&state, thread)?;
        let process = Self::process(&state, task.process)?;
        if process.pending_transaction.is_some()
            || (task.pending_transaction.is_some() && task.lifecycle != ThreadLifecycle::Starting)
        {
            return Err(TaskError::InvalidLifecycle);
        }
        let interrupted = Self::interrupt_pending(&state, thread)?;
        self.interrupts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(thread, Arc::downgrade(&sink));
        sink.set_interrupted(interrupted);
        Ok(())
    }

    pub fn unregister_interrupt(&self, thread: ThreadId) {
        self.interrupts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&thread);
    }

    pub(crate) fn rebind_interrupt(&self, source: ThreadId, target: ThreadId) {
        if source == target {
            return;
        }
        let mut interrupts = self
            .interrupts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let source = interrupts.remove(&source);
        interrupts.remove(&target);
        if let Some(source) = source {
            interrupts.insert(target, source);
        }
    }

    pub fn acknowledge_interrupt(&self, thread: ThreadId) -> Result<bool, TaskError> {
        let state = self.lock();
        let interrupted = Self::interrupt_pending(&state, thread)?;
        self.publish_interrupt(thread, interrupted);
        Ok(interrupted)
    }

    pub(crate) fn publish_interrupt(&self, thread: ThreadId, interrupted: bool) {
        let mut sinks = self
            .interrupts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(sink) = sinks.get(&thread).and_then(std::sync::Weak::upgrade) else {
            sinks.remove(&thread);
            return;
        };
        sink.set_interrupted(interrupted);
    }

    pub(super) fn interrupt_pending(state: &super::State, thread: ThreadId) -> Result<bool, TaskError> {
        let thread_state = Self::thread(state, thread)?;
        if thread_state.cancellation_pending || thread_state.signal_pending {
            return Ok(true);
        }
        let process = Self::process(state, thread_state.process)?;
        let blocked = SignalMask::from_bits(thread_state.signals.mask.bits() | thread_state.signals.deferred.bits());
        let deliverable = thread_state
            .signals
            .pending
            .snapshot()
            .into_iter()
            .chain(process.signals.pending.snapshot())
            .any(|info| {
                info.is_synchronous()
                    || (!blocked.contains(info.signal)
                        && process.signals.actions[info.signal.get() as usize - 1].disposition
                            != SignalDisposition::Ignore)
            });
        Ok(deliverable)
    }
}

#[cfg(test)]
mod test {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use super::*;
    use crate::{
        PendingTarget, ProcessCredentials, ProcessLimits, RegistryConfig, SignalInfo, SignalMask, SignalNumber,
    };

    #[derive(Default)]
    struct Token(AtomicBool);

    impl InterruptSink for Token {
        fn set_interrupted(&self, interrupted: bool) {
            self.0.store(interrupted, Ordering::Release);
        }
    }

    struct OrderedToken {
        value: AtomicBool,
        false_entered: mpsc::Sender<()>,
        false_release: Mutex<mpsc::Receiver<()>>,
        true_seen: mpsc::Sender<()>,
    }

    impl InterruptSink for OrderedToken {
        fn set_interrupted(&self, interrupted: bool) {
            if interrupted {
                self.value.store(true, Ordering::Release);
                let _ = self.true_seen.send(());
            } else {
                let _ = self.false_entered.send(());
                let _ = self.false_release.lock().unwrap().recv();
                self.value.store(false, Ordering::Release);
            }
        }
    }

    fn registry() -> (TaskRegistry, crate::ProcessId, ThreadId) {
        let registry = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let (process, thread) = registry
            .create_init(ProcessCredentials::new(0, 0, &[], 8).unwrap(), ProcessLimits::default())
            .unwrap();
        (registry, process, thread)
    }

    #[test]
    fn blocked_signal_requests_after_unblock() {
        let (registry, process, thread) = registry();
        let token = Arc::new(Token::default());
        registry.register_interrupt(thread, token.clone()).unwrap();
        let signal = SignalNumber::new(10).unwrap();
        registry
            .set_signal_mask(thread, SignalMask::from_bits(0).with(signal))
            .unwrap();
        registry
            .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(signal))
            .unwrap();
        assert!(!token.0.load(Ordering::Acquire));
        registry.set_signal_mask(thread, SignalMask::from_bits(0)).unwrap();
        assert!(token.0.load(Ordering::Acquire));
    }

    #[test]
    fn boundary_clear_rechecks_authoritative_state() {
        let (registry, process, thread) = registry();
        let token = Arc::new(Token::default());
        registry.register_interrupt(thread, token.clone()).unwrap();
        let signal = SignalNumber::new(10).unwrap();
        registry
            .enqueue_signal(PendingTarget::Thread(thread), SignalInfo::bare(signal))
            .unwrap();
        assert!(token.0.load(Ordering::Acquire));
        let prepared = registry.prepare_deliverable_signal(thread).unwrap().unwrap();
        assert!(registry.commit_signal_wait(prepared).unwrap());
        assert!(!registry.acknowledge_interrupt(thread).unwrap());
        assert!(!token.0.load(Ordering::Acquire));

        registry
            .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(signal))
            .unwrap();
        assert!(registry.acknowledge_interrupt(thread).unwrap());
        assert!(token.0.load(Ordering::Acquire));
    }

    #[test]
    fn cancellation_and_exit_own_token_lifetime() {
        let (registry, _, source) = registry();
        let clone = registry.begin_clone_thread(source).unwrap();
        let thread = registry.commit_clone_thread(clone).unwrap();
        let token = Arc::new(Token::default());
        registry.register_interrupt(thread, token.clone()).unwrap();
        registry.request_cancellation(thread).unwrap();
        assert!(token.0.load(Ordering::Acquire));
        registry.clear_cancellation(thread).unwrap();
        assert!(!token.0.load(Ordering::Acquire));
        registry.exit_thread(thread, crate::ExitStatus::Code(0)).unwrap();
        assert!(matches!(
            registry.register_interrupt(thread, token),
            Err(TaskError::InvalidThread)
        ));
    }

    #[test]
    fn clone_stage_registration_rolls_back() {
        let (registry, _, source) = registry();
        let clone = registry.begin_clone_thread(source).unwrap();
        let thread = clone.thread();
        let token = Arc::new(Token::default());
        registry.register_interrupt(thread, token.clone()).unwrap();
        registry.rollback_clone_thread(clone).unwrap();
        assert!(matches!(
            registry.register_interrupt(thread, token),
            Err(TaskError::InvalidThread)
        ));
    }

    #[test]
    fn fork_commit_activates_interrupt() {
        let (registry, _, source) = registry();
        let fork = registry.begin_fork_process(source).unwrap();
        let process = fork.process();
        let thread = fork.thread();
        let token = Arc::new(Token::default());
        registry.commit_fork_interrupt(&fork, token.clone()).unwrap();
        registry.request_cancellation(thread).unwrap();
        assert!(token.0.load(Ordering::Acquire));
        registry.clear_cancellation(thread).unwrap();
        assert!(!token.0.load(Ordering::Acquire));
        registry.exit_process(process, crate::ExitStatus::Code(0)).unwrap();
        assert!(matches!(
            registry.register_interrupt(thread, token),
            Err(TaskError::InvalidThread)
        ));
    }

    #[test]
    fn fork_interrupt_initialization_cannot_overwrite_cancellation() {
        let (registry, _, source) = registry();
        let registry = Arc::new(registry);
        let fork = registry.begin_fork_process(source).unwrap();
        let thread = fork.thread();
        let (false_entered, entered) = mpsc::channel();
        let (false_release, release) = mpsc::channel();
        let (true_seen, seen) = mpsc::channel();
        let token = Arc::new(OrderedToken {
            value: AtomicBool::new(false),
            false_entered,
            false_release: Mutex::new(release),
            true_seen,
        });
        let commit = {
            let registry = Arc::clone(&registry);
            let token = Arc::clone(&token);
            std::thread::spawn(move || registry.commit_fork_interrupt(&fork, token).unwrap())
        };
        entered.recv().unwrap();
        let cancel = {
            let registry = Arc::clone(&registry);
            std::thread::spawn(move || registry.request_cancellation(thread).unwrap())
        };
        let overtook = seen.recv_timeout(Duration::from_millis(50)).is_ok();
        false_release.send(()).unwrap();
        commit.join().unwrap();
        cancel.join().unwrap();
        assert!(!overtook);
        assert!(token.value.load(Ordering::Acquire));
    }

    #[test]
    fn process_and_thread_targets_publish_exactly() {
        let (registry, process, first) = registry();
        let clone = registry.begin_clone_thread(first).unwrap();
        let second = registry.commit_clone_thread(clone).unwrap();
        let first_token = Arc::new(Token::default());
        let second_token = Arc::new(Token::default());
        registry.register_interrupt(first, first_token.clone()).unwrap();
        registry.register_interrupt(second, second_token.clone()).unwrap();
        let signal = SignalNumber::new(10).unwrap();

        registry
            .enqueue_signal(PendingTarget::Thread(second), SignalInfo::bare(signal))
            .unwrap();
        assert!(!first_token.0.load(Ordering::Acquire));
        assert!(second_token.0.load(Ordering::Acquire));
        let prepared = registry.prepare_deliverable_signal(second).unwrap().unwrap();
        assert!(registry.commit_signal_wait(prepared).unwrap());
        registry.acknowledge_interrupt(second).unwrap();

        registry
            .enqueue_signal(PendingTarget::Process(process), SignalInfo::bare(signal))
            .unwrap();
        assert!(first_token.0.load(Ordering::Acquire));
        assert!(second_token.0.load(Ordering::Acquire));
    }

    #[test]
    fn concurrent_clear_cannot_lose_pending_set() {
        let (registry, process, thread) = registry();
        let registry = Arc::new(registry);
        let token = Arc::new(Token::default());
        registry.register_interrupt(thread, token.clone()).unwrap();
        let clearer = {
            let registry = registry.clone();
            std::thread::spawn(move || {
                for _ in 0..10_000 {
                    let _ = registry.acknowledge_interrupt(thread);
                }
            })
        };
        registry
            .enqueue_signal(
                PendingTarget::Process(process),
                SignalInfo::bare(SignalNumber::new(10).unwrap()),
            )
            .unwrap();
        clearer.join().unwrap();
        assert!(registry.acknowledge_interrupt(thread).unwrap());
        assert!(token.0.load(Ordering::Acquire));
    }
}
