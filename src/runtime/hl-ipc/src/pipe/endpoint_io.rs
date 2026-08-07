//! The endpoint read and write path, including the waits it blocks on.
use crate::pipe::{EndpointDirection, PIPE_BUF, PipeCancellationWake, PipeEndpoint, PipeState};
use hl_descriptor::{CancellationSubscription, ObjectError, OperationCancellation};
use std::io::{IoSlice, IoSliceMut};
use std::sync::Arc;
use std::sync::MutexGuard;
use std::sync::atomic::Ordering;
impl PipeEndpoint {
    /// Routes cancellation wakes at this pipe so a blocked wait is interrupted.
    fn subscribe_wake(
        &self,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Option<Box<dyn CancellationSubscription>> {
        cancellation.map(|cancellation| {
            cancellation.subscribe(Arc::new(PipeCancellationWake {
                pipe: Arc::downgrade(&self.pipe),
            }))
        })
    }

    pub(super) fn read_bytes(
        &self,
        output: &mut [u8],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        if self.direction != EndpointDirection::Read {
            return Err(ObjectError::BadDescriptor);
        }
        if output.is_empty() {
            return Ok(0);
        }
        let mut state = self.wait_for_readable(cancellation)?;
        if state.bytes.is_empty() {
            return Ok(0);
        }
        let available = state.packets.front().copied().unwrap_or(state.bytes.len());
        let count = output.len().min(available);
        for byte in &mut output[..count] {
            *byte = state.bytes.pop_front().expect("count is bounded by length");
        }
        if state.packet_mode {
            for _ in count..available {
                state.bytes.pop_front();
            }
            state.packets.pop_front();
        }
        let consumed = if state.packet_mode { available } else { count };
        if state.bytes.is_empty() {
            state.head_fragment = 0;
        } else {
            state.head_fragment = (state.head_fragment + consumed) % PIPE_BUF;
        }
        self.pipe.notify_sleepers(&state);
        drop(state);
        self.notify_readiness();
        Ok(count)
    }

    pub(super) fn write_bytes(
        &self,
        input: &[u8],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        if self.direction != EndpointDirection::Write {
            return Err(ObjectError::BadDescriptor);
        }
        if input.is_empty() {
            return Ok(0);
        }
        let _subscription = self.subscribe_wake(cancellation);
        if input.len() <= PIPE_BUF {
            return self.write_atomic(input, cancellation);
        }
        self.write_large(input, cancellation)
    }

    pub(super) fn read_vector(
        &self,
        output: &mut [IoSliceMut<'_>],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        let maximum = output.iter().try_fold(0_usize, |total, segment| {
            total.checked_add(segment.len()).ok_or(ObjectError::ResourceLimit)
        })?;
        let mut bytes = vec![0; maximum];
        let count = self.read_bytes(&mut bytes, cancellation)?.min(maximum);
        let mut copied = 0;
        for segment in output {
            let length = segment.len().min(count - copied);
            segment[..length].copy_from_slice(&bytes[copied..copied + length]);
            copied += length;
            if copied == count {
                break;
            }
        }
        Ok(count)
    }

    pub(super) fn write_vector(
        &self,
        input: &[IoSlice<'_>],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        let length = input.iter().try_fold(0_usize, |total, segment| {
            total.checked_add(segment.len()).ok_or(ObjectError::ResourceLimit)
        })?;
        let mut bytes = Vec::with_capacity(length);
        for segment in input {
            bytes.extend_from_slice(segment);
        }
        self.write_bytes(&bytes, cancellation)
    }

    pub(super) fn wait_for_readable(
        &self,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<MutexGuard<'_, PipeState>, ObjectError> {
        let mut state = self
            .pipe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut _subscription = None;
        let mut subscribed = cancellation.is_none();
        loop {
            if self.retired.load(Ordering::Acquire) {
                return Err(ObjectError::Retired);
            }
            if (!state.bytes.is_empty() && !state.splice_reserved) || state.writers == 0 {
                return Ok(state);
            }
            if self.nonblocking.load(Ordering::Acquire) {
                return Err(ObjectError::WouldBlock);
            }
            if cancellation.is_some_and(OperationCancellation::interrupted) {
                return Err(ObjectError::Interrupted);
            }
            if !subscribed {
                drop(state);
                _subscription = self.subscribe_wake(cancellation);
                subscribed = true;
                state = self
                    .pipe
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            state = self.wait(state);
        }
    }

    fn write_atomic(
        &self,
        input: &[u8],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        let mut state = self.wait_write_space(input.len(), cancellation)?;
        state.bytes.extend(input);
        if state.packet_mode {
            state.packets.push_back(input.len());
        }
        self.pipe.notify_sleepers(&state);
        drop(state);
        self.notify_readiness();
        Ok(input.len())
    }

    fn write_large(
        &self,
        input: &[u8],
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        let mut written = 0;
        while written < input.len() {
            let required = {
                let state = self
                    .pipe
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.packet_mode {
                    PIPE_BUF.min(input.len() - written)
                } else {
                    1
                }
            };
            let mut state = match self.wait_write_space(required, cancellation) {
                Ok(state) => state,
                Err(ObjectError::BrokenPipe) if written != 0 => return Ok(written),
                Err(ObjectError::Interrupted) if written != 0 => return Ok(written),
                Err(error) => return Err(error),
            };
            let count = (state.capacity - state.bytes.len() - state.head_fragment)
                .min(input.len() - written)
                .min(if state.packet_mode { PIPE_BUF } else { usize::MAX });
            state.bytes.extend(&input[written..written + count]);
            if state.packet_mode {
                state.packets.push_back(count);
            }
            written += count;
            let nonblocking = self.nonblocking.load(Ordering::Acquire);
            self.pipe.notify_sleepers(&state);
            drop(state);
            self.notify_readiness();
            if nonblocking {
                return Ok(written);
            }
        }
        Ok(written)
    }

    pub(super) fn wait_write_space(
        &self,
        required: usize,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<MutexGuard<'_, PipeState>, ObjectError> {
        let mut state = self
            .pipe
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut _subscription = None;
        let mut subscribed = cancellation.is_none();
        loop {
            if self.retired.load(Ordering::Acquire) {
                return Err(ObjectError::Retired);
            }
            if state.readers == 0 {
                return Err(ObjectError::BrokenPipe);
            }
            if state.capacity - state.bytes.len() - state.head_fragment >= required {
                return Ok(state);
            }
            if self.nonblocking.load(Ordering::Acquire) {
                return Err(ObjectError::WouldBlock);
            }
            if cancellation.is_some_and(OperationCancellation::interrupted) {
                return Err(ObjectError::Interrupted);
            }
            if !subscribed {
                drop(state);
                _subscription = self.subscribe_wake(cancellation);
                subscribed = true;
                state = self
                    .pipe
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                continue;
            }
            state = self.wait(state);
        }
    }

    fn wait<'state>(&self, state: MutexGuard<'state, PipeState>) -> MutexGuard<'state, PipeState> {
        let mut state = state;
        state.sleepers += 1;
        #[cfg(test)]
        if let Some(sender) = &state.sleeper_registration {
            let _ = sender.send(());
        }
        let mut state = self
            .pipe
            .changed
            .wait(state)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.sleepers -= 1;
        state
    }
}
