mod queue;

use super::{State, TaskRegistry};
use crate::signal::SIGNAL_COUNT;
use crate::{
    AlternateStack, ChildEventKind, DeliveryAction, PendingTarget, ProcessId, ProcessLifecycle, Resource, SignalAction,
    SignalDisposition, SignalInfo, SignalMask, SignalNumber, SignalQueueError, SignalThreadSnapshot, TaskError,
    ThreadId,
};
impl TaskRegistry {
    /// Resolves the minimal authority state for `tkill`/`tgkill` under one
    /// registry lock. This deliberately avoids [`TaskRegistry::snapshot`]: a
    /// signal send is not a checkpoint and must not clone every process,
    /// thread, pending queue, argument vector, or signal frame.
    pub fn signal_thread_target(
        &self,
        sender: ProcessId,
        thread_number: u32,
    ) -> Result<Option<crate::SignalThreadTarget>, TaskError> {
        let state = self.lock();
        let sender_credentials = Self::process(&state, sender)?.credentials.clone();
        let Some(slot) = thread_number
            .checked_sub(1)
            .and_then(|number| usize::try_from(number).ok())
        else {
            return Ok(None);
        };
        let Some(entry) = state.threads.get(slot).filter(|entry| entry.value.is_some()) else {
            return Ok(None);
        };
        let thread = ThreadId::new(slot as u32, entry.generation);
        let process = entry.value.as_ref().ok_or(TaskError::InvalidLifecycle)?.process;
        let target_credentials = Self::process(&state, process)?.credentials.clone();
        Ok(Some(crate::SignalThreadTarget {
            thread,
            process,
            sender_credentials,
            target_credentials,
        }))
    }

    pub(crate) fn publish_orphaned(&self, processes: Vec<ProcessId>) {
        let hangup = SignalNumber::new(1).expect("SIGHUP is a valid Linux signal");
        for process in processes {
            // Linux publishes these in this order when a newly orphaned group
            // contains a stopped member. Enqueue through the ordinary signal
            // path so SIGCONT advances the control generation and wakes the
            // scheduler; exit and setpgid cannot fail after their task-state
            // transition merely because a standard signal coalesced.
            let target = PendingTarget::Process(process);
            self.enqueue_best_effort(target, SignalInfo::bare(hangup), "orphan SIGHUP");
            self.enqueue_best_effort(target, SignalInfo::bare(SignalNumber::CONTINUE), "orphan SIGCONT");
        }
    }

    pub fn activity_observation(&self) -> u64 {
        self.signals.activity.observation()
    }
    pub fn subscribe_signal_activity(
        &self,
        observer: std::sync::Arc<dyn crate::SignalActivityWake>,
    ) -> crate::SignalActivitySubscription {
        self.signals.activity.subscribe(observer)
    }
    pub fn action(&self, process: ProcessId, signal: SignalNumber) -> Result<SignalAction, TaskError> {
        let state = self.lock();
        Ok(Self::process(&state, process)?.signals.actions[signal.get() as usize - 1])
    }

    pub fn deliver_thread_state(&self, thread: ThreadId) -> Result<SignalThreadSnapshot, TaskError> {
        let state = self.lock();
        let thread = Self::thread(&state, thread)?;
        Ok(SignalThreadSnapshot {
            mask: thread.signals.mask,
            alternate_stack: thread.signals.alternate_stack,
            pending: thread.signals.pending.snapshot(),
            deferred: thread.signals.deferred,
            frames: thread.signals.frames.clone(),
        })
    }

    pub fn pending_signal_mask(&self, thread: ThreadId) -> Result<SignalMask, TaskError> {
        let state = self.lock();
        let thread_state = Self::thread(&state, thread)?;
        let process_state = Self::process(&state, thread_state.process)?;
        Ok(SignalMask::from_bits(
            thread_state.signals.pending.mask().bits() | process_state.signals.pending.mask().bits(),
        ))
    }

    /// Reports whether every currently deliverable handled signal requests
    /// restart of an interrupted slow syscall. `None` means no userspace
    /// handler is currently deliverable, so a cancellation must retain its
    /// own non-signal meaning.
    pub fn restart_interrupted_signal(&self, thread: ThreadId) -> Result<Option<bool>, TaskError> {
        const SA_RESTART: u64 = 0x1000_0000;

        let state = self.lock();
        let thread_state = Self::thread(&state, thread)?;
        let process = Self::process(&state, thread_state.process)?;
        let mut handled = false;
        let mut restart = true;
        for info in thread_state
            .signals
            .pending
            .iter()
            .chain(process.signals.pending.iter())
        {
            let blocked =
                SignalMask::from_bits(thread_state.signals.mask.bits() | thread_state.signals.deferred.bits());
            if !info.is_synchronous() && blocked.contains(info.signal) {
                continue;
            }
            let action = process.signals.actions[info.signal.get() as usize - 1];
            if matches!(action.disposition, SignalDisposition::Handler(_)) {
                handled = true;
                restart &= action.flags & SA_RESTART != 0;
            }
        }
        Ok(handled.then_some(restart))
    }

    /// Reports whether signal delivery must bring an interruptible syscall
    /// back to the task boundary. `temporary_mask` models the atomic mask used
    /// by ppoll/pselect; otherwise the thread's installed mask is authoritative.
    pub fn has_interrupting_signal(
        &self,
        thread: ThreadId,
        temporary_mask: Option<SignalMask>,
    ) -> Result<bool, TaskError> {
        let state = self.lock();
        let thread_state = Self::thread(&state, thread)?;
        let blocked = SignalMask::from_bits(
            temporary_mask.unwrap_or(thread_state.signals.mask).bits() | thread_state.signals.deferred.bits(),
        );
        let process = Self::process(&state, thread_state.process)?;
        let thread_signal = thread_state
            .signals
            .pending
            .peek_synchronous()
            .or_else(|| thread_state.signals.pending.peek_eligible(blocked));
        let process_signal = process
            .signals
            .pending
            .peek_synchronous()
            .or_else(|| process.signals.pending.peek_eligible(blocked));
        let signal = match (thread_signal, process_signal) {
            (Some(left), Some(right)) => Some(left.max(right)),
            (left, right) => left.or(right),
        };
        let Some(signal) = signal else { return Ok(false) };
        Ok(!matches!(
            Self::delivery_action(&state, thread_state.process, signal)?,
            DeliveryAction::Ignore
        ))
    }

    pub fn set_action(&self, process: ProcessId, signal: SignalNumber, action: SignalAction) -> Result<(), TaskError> {
        if matches!(signal, SignalNumber::KILL | SignalNumber::STOP) && action != SignalAction::DEFAULT {
            return Err(TaskError::InvalidLifecycle);
        }
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        {
            let process_state = Self::process_mut(&mut state, process)?;
            if !matches!(
                process_state.lifecycle,
                ProcessLifecycle::Running | ProcessLifecycle::Stopped
            ) {
                return Err(TaskError::InvalidLifecycle);
            }
            process_state.signals.actions[signal.get() as usize - 1] = action;
        }
        if action.disposition == SignalDisposition::Ignore {
            Self::flush_process_signal(&mut state, process, signal)?;
        }
        Ok(())
    }

    pub fn set_signal_mask(&self, thread: ThreadId, mask: SignalMask) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        Self::thread_mut(&mut state, thread)?.signals.mask = SignalMask::from_bits(mask.bits());
        drop(state);
        let _ = self.acknowledge_interrupt(thread)?;
        Ok(())
    }

    pub fn replace_signal_mask(&self, thread: ThreadId, mask: SignalMask) -> Result<SignalMask, TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let thread_state = Self::thread_mut(&mut state, thread)?;
        let previous = thread_state.signals.mask;
        thread_state.signals.mask = SignalMask::from_bits(mask.bits());
        drop(state);
        let _ = self.acknowledge_interrupt(thread)?;
        Ok(previous)
    }

    pub fn set_alternate_stack(&self, thread: ThreadId, stack: AlternateStack) -> Result<(), TaskError> {
        if matches!(stack, AlternateStack::Enabled { size: 0, .. }) {
            return Err(TaskError::InvalidLifecycle);
        }
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        Self::thread_mut(&mut state, thread)?.signals.alternate_stack = stack;
        Ok(())
    }

    pub fn dequeue_signal(&self, thread: ThreadId) -> Result<Option<(SignalInfo, DeliveryAction)>, TaskError> {
        if let Some(prepared) = self.prepare_forced_delivery(thread) {
            return self
                .commit_forced_delivery(prepared)
                .map(|value| value.map(|(info, action, _)| (info, action)));
        }
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let process = Self::thread(&state, thread)?.process;
        let signals = &Self::thread(&state, thread)?.signals;
        let blocked = SignalMask::from_bits(signals.mask.bits() | signals.deferred.bits());
        let thread_signal = signals
            .pending
            .peek_synchronous()
            .or_else(|| signals.pending.peek_eligible(blocked));
        let process_pending = &Self::process(&state, process)?.signals.pending;
        let process_signal = process_pending
            .peek_synchronous()
            .or_else(|| process_pending.peek_eligible(blocked));
        let selected = match (thread_signal, process_signal) {
            (Some(thread_signal), Some(process_signal)) => Some(thread_signal.max(process_signal)),
            (thread_signal, process_signal) => thread_signal.or(process_signal),
        };
        let Some(signal) = selected else {
            return Ok(None);
        };
        let info = if thread_signal == Some(signal) {
            Self::thread_mut(&mut state, thread)?.signals.pending.pop(signal)
        } else {
            Self::process_mut(&mut state, process)?.signals.pending.pop(signal)
        }
        .ok_or(TaskError::InvalidLifecycle)?;
        let action = Self::delivery_action(&state, process, signal)?;
        Self::apply_default_transition(&mut state, process, signal, action, self.max_pending_signals)?;
        drop(state);
        if action == DeliveryAction::Stop {
            self.child_ready.notify_all();
            self.signals.activity.notify(crate::SignalActivityKind::Ordinary, None);
        }
        Ok(Some((info, action)))
    }

    pub fn consume_signal_wait(&self, thread: ThreadId, selected: SignalMask) -> Result<Option<SignalInfo>, TaskError> {
        let Some(prepared) = self.prepare_signal_wait(thread, selected)? else {
            return Ok(None);
        };
        let info = prepared.info();
        self.commit_signal_wait(prepared)?
            .then_some(info)
            .map_or(Ok(None), |info| Ok(Some(info)))
    }

    pub fn prepare_signal_wait(
        &self,
        thread: ThreadId,
        selected: SignalMask,
    ) -> Result<Option<crate::PreparedSignalWait>, TaskError> {
        let state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let mut reservations = self
            .signals
            .reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let process = Self::thread(&state, thread)?.process;
        let thread_signal = Self::thread(&state, thread)?.signals.pending.peek_selected(selected);
        let process_signal = Self::process(&state, process)?.signals.pending.peek_selected(selected);
        let signal = match (thread_signal, process_signal) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (left, right) => left.or(right),
        };
        let Some(signal) = signal else { return Ok(None) };
        let from_thread = thread_signal == Some(signal);
        let info = if from_thread {
            Self::thread(&state, thread)?.signals.pending.front(signal)
        } else {
            Self::process(&state, process)?.signals.pending.front(signal)
        }
        .ok_or(TaskError::InvalidLifecycle)?;
        let key = crate::signal::SignalReservationKey {
            thread,
            process,
            info,
            from_thread,
        };
        if !reservations.insert(key) {
            return Ok(None);
        }
        drop(reservations);
        Ok(Some(crate::PreparedSignalWait {
            thread,
            process,
            info,
            from_thread,
            _reservation: crate::signal::SignalReservation {
                key,
                reservations: self.signals.reservations.clone(),
            },
        }))
    }

    // Consumes the prepared wait so it cannot be committed twice.
    #[allow(clippy::needless_pass_by_value)]
    pub fn commit_signal_wait(&self, prepared: crate::PreparedSignalWait) -> Result<bool, TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, prepared.thread)?;
        let pending = if prepared.from_thread {
            &mut Self::thread_mut(&mut state, prepared.thread)?.signals.pending
        } else {
            &mut Self::process_mut(&mut state, prepared.process)?.signals.pending
        };
        if pending.front(prepared.info.signal) != Some(prepared.info) {
            return Ok(false);
        }
        Ok(pending.pop(prepared.info.signal) == Some(prepared.info))
    }

    pub fn has_signal_wait(&self, thread: ThreadId, selected: SignalMask) -> Result<bool, TaskError> {
        let state = self.lock();
        let thread_state = Self::thread(&state, thread)?;
        let process = Self::process(&state, thread_state.process)?;
        Ok(thread_state.signals.pending.peek_selected(selected).is_some()
            || process.signals.pending.peek_selected(selected).is_some())
    }

    pub fn has_deliverable_except(&self, thread: ThreadId, excluded: SignalMask) -> Result<bool, TaskError> {
        let state = self.lock();
        let thread_state = Self::thread(&state, thread)?;
        let process = Self::process(&state, thread_state.process)?;
        let blocked = SignalMask::from_bits(
            thread_state.signals.mask.bits() | thread_state.signals.deferred.bits() | excluded.bits(),
        );
        Ok(thread_state.signals.pending.peek_synchronous().is_some()
            || thread_state.signals.pending.peek_eligible(blocked).is_some()
            || process.signals.pending.peek_synchronous().is_some()
            || process.signals.pending.peek_eligible(blocked).is_some())
    }

    pub(super) fn delivery_action(
        state: &State,
        process: ProcessId,
        signal: SignalNumber,
    ) -> Result<DeliveryAction, TaskError> {
        let action = Self::process(state, process)?.signals.actions[signal.get() as usize - 1];
        Ok(match action.disposition {
            SignalDisposition::Default => Self::default_delivery(state, process, signal)?,
            SignalDisposition::Ignore => DeliveryAction::Ignore,
            SignalDisposition::Handler(_) => DeliveryAction::Handle(action),
        })
    }

    pub(super) fn delivery_info(
        state: &State,
        process: ProcessId,
        info: SignalInfo,
    ) -> Result<DeliveryAction, TaskError> {
        let action = Self::delivery_action(state, process, info.signal)?;
        if info.is_synchronous() && action == DeliveryAction::Ignore {
            return Self::default_delivery(state, process, info.signal);
        }
        Ok(action)
    }

    fn default_delivery(state: &State, process: ProcessId, signal: SignalNumber) -> Result<DeliveryAction, TaskError> {
        let default = signal.default_action();
        if default != (DeliveryAction::Terminate { dumped_core: true }) {
            return Ok(default);
        }
        let core_enabled = Self::process(state, process)?
            .limits
            .get(Resource::Core)
            .is_some_and(|limit| limit.soft != 0);
        Ok(DeliveryAction::Terminate {
            dumped_core: core_enabled,
        })
    }

    pub(super) fn apply_default_transition(
        state: &mut State,
        process: ProcessId,
        signal: SignalNumber,
        action: DeliveryAction,
        max_pending: usize,
    ) -> Result<u64, TaskError> {
        let previous = Self::process(state, process)?.lifecycle;
        let process_state = Self::process_mut(state, process)?;
        match action {
            DeliveryAction::Stop => {
                process_state.lifecycle = ProcessLifecycle::Stopped;
                process_state.control_epoch = process_state
                    .control_epoch
                    .checked_add(1)
                    .ok_or(TaskError::InvalidLifecycle)?;
            }
            DeliveryAction::Continue if process_state.lifecycle == ProcessLifecycle::Stopped => {
                process_state.lifecycle = ProcessLifecycle::Running;
                hl_log::hl_debug!(hl_log::tag::SIGNAL, "signal {} continues {process:?}", signal.get());
            }
            DeliveryAction::Terminate { dumped_core } => {
                process_state.lifecycle = ProcessLifecycle::Exiting;
                let number = signal.get();
                hl_log::hl_debug!(
                    hl_log::tag::SIGNAL,
                    "signal {number} terminates {process:?} from {previous:?} dumped_core={dumped_core}"
                );
            }
            _ => {}
        }
        if action == DeliveryAction::Stop && previous != ProcessLifecycle::Stopped {
            hl_log::hl_debug!(
                hl_log::tag::SIGNAL,
                "signal {} stops {process:?} from {previous:?}",
                signal.get()
            );
            Self::record_child_transition(state, process, ChildEventKind::Stopped(signal), max_pending)?;
        }
        Ok(Self::process(state, process)?.control_epoch)
    }

    fn apply_generation_effect(
        state: &mut State,
        process: ProcessId,
        signal: SignalNumber,
        max_pending: usize,
    ) -> Result<Option<u64>, TaskError> {
        let control = matches!(signal, SignalNumber::CONTINUE | SignalNumber::KILL);
        let epoch = if control {
            let process_state = Self::process_mut(state, process)?;
            process_state.control_epoch = process_state
                .control_epoch
                .checked_add(1)
                .ok_or(TaskError::InvalidLifecycle)?;
            Some(process_state.control_epoch)
        } else {
            None
        };
        if signal == SignalNumber::CONTINUE {
            Self::apply_continue_generation(state, process, max_pending)?;
            return Ok(epoch);
        }
        if (19..=22).contains(&signal.get()) {
            Self::flush_process_signal(state, process, SignalNumber::CONTINUE)?;
        }
        Ok(epoch)
    }

    fn apply_continue_generation(state: &mut State, process: ProcessId, max_pending: usize) -> Result<(), TaskError> {
        let was_stopped = Self::process(state, process)?.lifecycle == ProcessLifecycle::Stopped;
        if was_stopped {
            Self::process_mut(state, process)?.lifecycle = ProcessLifecycle::Running;
            Self::record_child_transition(state, process, ChildEventKind::Continued, max_pending)?;
        }
        for number in 19..=22 {
            let stop = SignalNumber::new(number).map_err(Self::queue_error)?;
            Self::flush_process_signal(state, process, stop)?;
        }
        Ok(())
    }

    fn flush_process_signal(state: &mut State, process: ProcessId, signal: SignalNumber) -> Result<(), TaskError> {
        Self::process_mut(state, process)?.signals.pending.flush(signal);
        for entry in &mut state.threads {
            let Some(thread) = &mut entry.value else {
                continue;
            };
            if thread.process == process {
                thread.signals.pending.flush(signal);
            }
        }
        Ok(())
    }

    pub(super) fn nondefault_actions(actions: &[SignalAction; SIGNAL_COUNT]) -> Vec<(SignalNumber, SignalAction)> {
        actions
            .iter()
            .enumerate()
            .filter_map(|(index, action)| {
                let number = u8::try_from(index + 1).ok()?;
                (*action != SignalAction::DEFAULT).then_some((SignalNumber::new(number).ok()?, *action))
            })
            .collect()
    }

    fn queue_error(error: SignalQueueError) -> TaskError {
        match error {
            SignalQueueError::QueueFull => TaskError::SignalQueueLimit,
            SignalQueueError::InvalidSignal | SignalQueueError::InvalidAction => TaskError::InvalidLifecycle,
        }
    }
}
