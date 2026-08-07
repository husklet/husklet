//! Placeholder open file description used while memfd bindings are restored.

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{ObjectError, ObjectKind, OpenFileDescription, PreparedSpliceRead, SeekPosition};

use crate::memfd::RuntimeMemfd;

pub(super) struct Proxy {
    state: Mutex<ProxyState>,
    #[cfg(test)]
    fail_bind: AtomicBool,
    #[cfg(test)]
    close_on_bind: AtomicBool,
}

struct ProxyState {
    object: Option<Arc<RuntimeMemfd>>,
    closed: bool,
    published: bool,
}

impl Proxy {
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(ProxyState {
                object: None,
                closed: false,
                published: false,
            }),
            #[cfg(test)]
            fail_bind: AtomicBool::new(false),
            #[cfg(test)]
            close_on_bind: AtomicBool::new(false),
        }
    }

    pub(super) fn bind(&self, object: Arc<RuntimeMemfd>) -> Result<(), ()> {
        #[cfg(test)]
        if self.fail_bind.swap(false, Ordering::AcqRel) {
            return Err(());
        }
        let mut state = self.state.lock().map_err(|_| ())?;
        #[cfg(test)]
        if self.close_on_bind.swap(false, Ordering::AcqRel) {
            state.closed = true;
            return Err(());
        }
        if state.closed || state.object.is_some() {
            return Err(());
        }
        state.object = Some(object);
        Ok(())
    }

    pub(super) fn unbind(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.published = false;
            state.object.take();
        }
    }

    pub(super) fn publish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.published = true;
            if state.closed
                && let Some(object) = state.object.take()
            {
                object.close();
            }
        }
    }

    #[cfg(test)]
    pub(super) fn fail_next_bind(&self) {
        self.fail_bind.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub(super) fn close_during_bind(&self) {
        self.close_on_bind.store(true, Ordering::Release);
    }

    pub(super) fn object(&self) -> Result<Arc<RuntimeMemfd>, ObjectError> {
        let state = self.state.lock().map_err(|_| ObjectError::Io)?;
        if state.closed {
            return Err(ObjectError::Retired);
        }
        state.object.clone().ok_or(ObjectError::Retired)
    }
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PendingMemfd").finish_non_exhaustive()
    }
}

impl OpenFileDescription for Proxy {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }
    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        self.object()?.metadata()
    }
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.object()?.read(output)
    }
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.object()?.write(input)
    }
    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.object()?.read_at(offset, output)
    }
    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.object()?.write_at(offset, input)
    }
    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        self.object()?.seek(position)
    }
    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        self.object()?
            .prepare_splice_read(offset, maximum, nonblocking, cancellation)
    }
    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.closed {
                return;
            }
            state.closed = true;
            if state.published
                && let Some(object) = state.object.take()
            {
                object.close();
            }
        }
    }
}
