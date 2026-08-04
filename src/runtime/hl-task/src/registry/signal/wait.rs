use super::TaskRegistry;
use crate::{
    AlternateStack, DeliveryAction, PreparedForcedDelivery, PreparedSignalWait, SignalAction, SignalInfo, SignalMask,
    TaskError, ThreadId,
};

impl TaskRegistry {
    pub fn prepare_deliverable_signal(&self, thread: ThreadId) -> Result<Option<PreparedSignalWait>, TaskError> {
        let state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let mut reservations = self
            .signals
            .reservations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let process = Self::thread(&state, thread)?.process;
        let signals = &Self::thread(&state, thread)?.signals;
        let blocked = SignalMask::from_bits(signals.mask.bits() | signals.deferred.bits());
        let thread_pending = &Self::thread(&state, thread)?.signals.pending;
        let process_pending = &Self::process(&state, process)?.signals.pending;
        let thread_signal = thread_pending
            .peek_synchronous()
            .or_else(|| thread_pending.peek_eligible(blocked));
        let process_signal = process_pending
            .peek_synchronous()
            .or_else(|| process_pending.peek_eligible(blocked));
        let signal = match (thread_signal, process_signal) {
            (Some(left), Some(right)) => Some(left.max(right)),
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
        Ok(Some(PreparedSignalWait {
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

    pub fn force_signal_delivery(&self, prepared: PreparedSignalWait) -> Result<(), TaskError> {
        let thread = prepared.thread;
        let mut forced = self.signals.forced.lock().unwrap_or_else(|error| error.into_inner());
        if forced.contains_key(&thread) {
            return Err(TaskError::InvalidLifecycle);
        }
        forced.insert(thread, prepared);
        Ok(())
    }

    pub fn prepare_forced_delivery(&self, thread: ThreadId) -> Option<PreparedForcedDelivery> {
        let prepared = self
            .signals
            .forced
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&thread)?;
        Some(PreparedForcedDelivery {
            thread,
            prepared: Some(prepared),
            forced: self.signals.forced.clone(),
        })
    }

    pub fn commit_forced_delivery(
        &self,
        mut forced: PreparedForcedDelivery,
    ) -> Result<Option<(SignalInfo, DeliveryAction, u64)>, TaskError> {
        let prepared = forced.prepared.take().ok_or(TaskError::InvalidLifecycle)?;
        let info = prepared.info();
        if !self.commit_signal_wait(prepared)? {
            return Ok(None);
        }
        let mut state = self.lock();
        let process = Self::thread(&state, forced.thread)?.process;
        let action = Self::delivery_info(&state, process, info)?;
        let epoch = Self::apply_default_transition(
            &mut state,
            process,
            info.signal,
            action,
            self.max_pending_signals,
        )?;
        drop(state);
        if action == DeliveryAction::Stop {
            self.child_ready.notify_all();
            self.signals.activity.notify(crate::SignalActivityKind::Ordinary, None);
        }
        Ok(Some((info, action, epoch)))
    }

    pub fn discard_forced_delivery(&self, mut forced: PreparedForcedDelivery) -> Result<bool, TaskError> {
        let prepared = forced.prepared.take().ok_or(TaskError::InvalidLifecycle)?;
        self.commit_signal_wait(prepared)
    }

    pub fn commit_frame_delivery(
        &self,
        mut forced: PreparedForcedDelivery,
        handler_mask: SignalMask,
        handler_alternate_stack: AlternateStack,
        handler_stack_pointer: u64,
        reset_action: bool,
    ) -> Result<SignalInfo, TaskError> {
        let prepared = forced.prepared.take().ok_or(TaskError::InvalidLifecycle)?;
        let info = prepared.info();
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, prepared.thread)?;
        let pending = if prepared.from_thread {
            &mut Self::thread_mut(&mut state, prepared.thread)?.signals.pending
        } else {
            &mut Self::process_mut(&mut state, prepared.process)?.signals.pending
        };
        if pending.front(info.signal) != Some(info) || pending.pop(info.signal) != Some(info) {
            return Err(TaskError::InvalidLifecycle);
        }
        let process_pending = Self::process(&state, prepared.process)?.signals.pending.snapshot();
        let thread_pending = Self::thread(&state, prepared.thread)?.signals.pending.snapshot();
        let entry_pending = process_pending
            .into_iter()
            .chain(thread_pending)
            .filter(|pending| pending.signal != info.signal)
            .fold(0_u64, |bits, pending| bits | (1_u64 << (pending.signal.get() - 1)));
        let signals = &mut Self::thread_mut(&mut state, prepared.thread)?.signals;
        // The retained engine's fixed 32-entry bookkeeping stack does not
        // reject a deeper guest handler: it publishes the frame but stops
        // recording new defer/unwind state at that depth.
        if signals.frames.len() < crate::SIGNAL_FRAME_MAXIMUM {
            signals.frames.push(crate::SignalFrameScope {
                deferred: signals.deferred,
                stack_pointer: handler_stack_pointer,
            });
            signals.deferred = SignalMask::from_bits(signals.deferred.bits() | entry_pending);
        } else if let Some(frame) = signals.frames.last_mut() {
            // The retained fixed-depth CPU state keeps depth capped but still
            // points its top slot at the most recently installed frame.
            frame.stack_pointer = handler_stack_pointer;
        }
        signals.mask = handler_mask;
        signals.alternate_stack = handler_alternate_stack;
        if reset_action {
            Self::process_mut(&mut state, prepared.process)?.signals.actions[info.signal.get() as usize - 1] =
                SignalAction::DEFAULT;
        }
        Ok(info)
    }

    pub fn replace_signal_context(
        &self,
        thread: ThreadId,
        mask: SignalMask,
        alternate_stack: AlternateStack,
    ) -> Result<(SignalMask, AlternateStack), TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let thread_state = Self::thread_mut(&mut state, thread)?;
        let previous = (thread_state.signals.mask, thread_state.signals.alternate_stack);
        thread_state.signals.mask = mask;
        thread_state.signals.alternate_stack = alternate_stack;
        thread_state.signals.deferred = thread_state
            .signals
            .frames
            .pop()
            .map_or_else(|| SignalMask::from_bits(0), |frame| frame.deferred);
        Ok(previous)
    }

    /// Releases handler scopes abandoned by a non-local stack unwind.
    ///
    /// Execution supplies the live stack position; task owns the bounded
    /// handler positions and matching pending-signal scopes.
    pub fn unwind_signal_frames(&self, thread: ThreadId, stack_pointer: u64) -> Result<usize, TaskError> {
        let mut state = self.lock();
        Self::ensure_thread_unreserved(&state, thread)?;
        let signals = &mut Self::thread_mut(&mut state, thread)?.signals;
        let mut count = 0;
        while signals.frames.last().is_some_and(|frame| stack_pointer > frame.stack_pointer) {
            signals.deferred = signals
                .frames
                .pop()
                .map_or_else(|| SignalMask::from_bits(0), |frame| frame.deferred);
            count += 1;
        }
        Ok(count)
    }
}
