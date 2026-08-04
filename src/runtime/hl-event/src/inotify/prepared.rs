use super::{Inotify, InotifyError, QueuedEvent};
use std::sync::{Arc, Weak};

use hl_descriptor::{CancellationNotification, ObjectError, OperationCancellation, PreparedAtomicRead};

use super::InotifyInner;

struct ReadNotification(Weak<InotifyInner>);

impl CancellationNotification for ReadNotification {
    fn notify(&self) {
        if let Some(inner) = self.0.upgrade() {
            let state = inner.state.lock().unwrap_or_else(|error| error.into_inner());
            drop(state);
            inner.changed.notify_all();
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreparedInotifyRead {
    events: Vec<QueuedEvent>,
    bytes: Vec<u8>,
}

impl PreparedInotifyRead {
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl Inotify {
    pub fn prepare_read(&self, capacity: usize) -> Result<PreparedInotifyRead, InotifyError> {
        self.prepare_read_context(capacity, None)
    }

    fn prepare_read_context(
        &self,
        capacity: usize,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<PreparedInotifyRead, InotifyError> {
        let _subscription = cancellation
            .map(|cancellation| cancellation.subscribe(Arc::new(ReadNotification(Arc::downgrade(&self.inner)))));
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        loop {
            Self::ensure_active(&state)?;
            if !state.queue.is_empty() {
                break;
            }
            if state.nonblocking {
                return Err(InotifyError::WouldBlock);
            }
            if cancellation.is_some_and(OperationCancellation::interrupted) {
                return Err(InotifyError::Interrupted);
            }
            state = self
                .inner
                .changed
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
        let mut events = Vec::new();
        let mut size = 0;
        for event in &state.queue {
            let encoded = event.encoded_len();
            if encoded > capacity.saturating_sub(size) {
                break;
            }
            size += encoded;
            events.push(event.clone());
        }
        if events.is_empty() {
            return Err(InotifyError::InvalidArgument);
        }
        let mut bytes = vec![0; size];
        let mut offset = 0;
        for event in &events {
            let encoded = event.encoded_len();
            event.encode(&mut bytes[offset..offset + encoded]);
            offset += encoded;
        }
        Ok(PreparedInotifyRead { events, bytes })
    }

    pub fn commit_read(&self, prepared: &PreparedInotifyRead) -> Result<(), InotifyError> {
        let mut state = self.inner.state.lock().unwrap_or_else(|error| error.into_inner());
        Self::ensure_active(&state)?;
        if !state
            .queue
            .iter()
            .zip(&prepared.events)
            .all(|(queued, expected)| queued == expected)
            || state.queue.len() < prepared.events.len()
        {
            return Err(InotifyError::Interrupted);
        }
        for _ in &prepared.events {
            let event = state.queue.pop_front().expect("prepared prefix was validated");
            let encoded = event.encoded_len();
            state.queue_bytes -= encoded;
            if event.mask.contains(crate::InotifyMask::QUEUE_OVERFLOW) {
                state.overflow_queued = false;
            }
        }
        drop(state);
        self.inner.readiness.notify();
        Ok(())
    }
}

pub(crate) struct AtomicInotifyRead {
    object: Inotify,
    prepared: PreparedInotifyRead,
}

impl AtomicInotifyRead {
    pub(crate) fn prepare(
        object: &Inotify,
        maximum: usize,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        let prepared = object.prepare_read(maximum).map_err(InotifyError::object_error)?;
        Ok(Some(Box::new(Self {
            object: object.clone(),
            prepared,
        })))
    }

    pub(crate) fn prepare_context(
        object: &Inotify,
        maximum: usize,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        let prepared = object
            .prepare_read_context(maximum, cancellation)
            .map_err(InotifyError::object_error)?;
        Ok(Some(Box::new(Self {
            object: object.clone(),
            prepared,
        })))
    }
}

impl PreparedAtomicRead for AtomicInotifyRead {
    fn bytes(&self) -> &[u8] {
        self.prepared.bytes()
    }
    fn commit(self: Box<Self>) -> Result<(), ObjectError> {
        self.object
            .commit_read(&self.prepared)
            .map_err(InotifyError::object_error)
    }
}

pub(crate) use AtomicInotifyRead as AtomicRead;
