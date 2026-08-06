use std::sync::Arc;

use hl_descriptor::{
    ObjectError, OperationCancellation, PreparedAtomicRead as AtomicRead, PreparedSpliceRead as SpliceRead,
};

use crate::pipe::{EndpointDirection, PipeEndpoint, PipeShared};

pub(super) struct PreparedRead {
    pipe: Arc<PipeShared>,
    bytes: Vec<u8>,
    active: bool,
    discard_packet_tail: bool,
}

impl PreparedRead {
    pub(super) fn prepare(
        endpoint: &PipeEndpoint,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
        discard_packet_tail: bool,
    ) -> Result<Box<Self>, ObjectError> {
        if endpoint.direction != EndpointDirection::Read {
            return Err(ObjectError::BadDescriptor);
        }
        let state = endpoint.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if nonblocking && state.bytes.is_empty() && state.writers != 0 {
            return Err(ObjectError::WouldBlock);
        }
        drop(state);
        let mut state = endpoint.wait_for_readable(cancellation)?;
        if state.bytes.is_empty() {
            return Ok(Box::new(Self {
                pipe: endpoint.pipe.clone(),
                bytes: Vec::new(),
                active: false,
                discard_packet_tail,
            }));
        }
        let record = state.packets.front().copied().unwrap_or(state.bytes.len());
        let count = maximum.min(state.bytes.len()).min(record);
        let bytes = state.bytes.iter().take(count).copied().collect();
        state.splice_reserved = true;
        state.waiters += 1;
        drop(state);
        Ok(Box::new(Self {
            pipe: endpoint.pipe.clone(),
            bytes,
            active: true,
            discard_packet_tail,
        }))
    }

    fn release(&mut self, count: usize) -> Result<(), ObjectError> {
        if !self.active {
            return Ok(());
        }
        let mut state = self.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.splice_reserved
            || state
                .bytes
                .iter()
                .take(self.bytes.len())
                .copied()
                .ne(self.bytes.iter().copied())
        {
            return Err(ObjectError::Interrupted);
        }
        let consumed = if self.discard_packet_tail && state.packet_mode {
            state.packets.front().copied().unwrap_or(count)
        } else {
            count
        };
        state.bytes.drain(..consumed);
        if state.bytes.is_empty() {
            state.head_fragment = 0;
        } else {
            state.head_fragment = (state.head_fragment + consumed) % super::PIPE_BUF;
        }
        if state.packet_mode {
            let record = state.packets.front().copied().unwrap_or(0);
            if consumed == record {
                state.packets.pop_front();
            } else if let Some(record) = state.packets.front_mut() {
                *record -= consumed;
            }
        }
        state.splice_reserved = false;
        state.waiters -= 1;
        self.active = false;
        self.pipe.notify_sleepers(&state);
        Ok(())
    }
}

impl AtomicRead for PreparedRead {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn commit(mut self: Box<Self>) -> Result<(), ObjectError> {
        let count = self.bytes.len();
        self.release(count)
    }

    fn commit_prefix(mut self: Box<Self>, count: usize) -> Result<bool, ObjectError> {
        if count > self.bytes.len() {
            return Err(ObjectError::InvalidArgument);
        }
        self.release(count)?;
        Ok(true)
    }
}

impl SpliceRead for PreparedRead {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn commit(mut self: Box<Self>, count: usize) -> Result<(), ObjectError> {
        if count > self.bytes.len() {
            return Err(ObjectError::InvalidArgument);
        }
        self.release(count)
    }
}

impl Drop for PreparedRead {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let mut state = self.pipe.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.splice_reserved = false;
        state.waiters -= 1;
        self.pipe.notify_sleepers(&state);
    }
}
