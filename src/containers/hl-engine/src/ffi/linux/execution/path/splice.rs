use std::sync::{Arc, Condvar, Mutex};

use hl_descriptor::{ObjectError, OperationCancellation, PreparedSpliceRead};

#[derive(Default)]
pub(super) struct CursorGate {
    reserved: Mutex<bool>,
    changed: Condvar,
}

struct CancellationWake(Arc<CursorGate>);

impl hl_descriptor::CancellationNotification for CancellationWake {
    fn notify(&self) {
        let _guard = self.0.reserved.lock().unwrap_or_else(|error| error.into_inner());
        self.0.changed.notify_all();
    }
}

impl CursorGate {
    pub(super) fn enter(&self) {
        let mut reserved = self.reserved.lock().unwrap_or_else(|error| error.into_inner());
        while *reserved {
            reserved = self.changed.wait(reserved).unwrap_or_else(|error| error.into_inner());
        }
    }

    pub(super) fn prepare(
        self: &Arc<Self>,
        implicit: bool,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
        bytes: impl FnOnce() -> Result<Vec<u8>, ObjectError>,
        commit: impl FnOnce(usize) -> Result<(), ObjectError> + Send + 'static,
    ) -> Result<Box<dyn PreparedSpliceRead>, ObjectError> {
        if cancellation.is_some_and(OperationCancellation::interrupted) {
            return Err(ObjectError::Interrupted);
        }
        let subscription = cancellation.map(|value| value.subscribe(Arc::new(CancellationWake(Arc::clone(self)))));
        let mut reserved = self.reserved.lock().unwrap_or_else(|error| error.into_inner());
        while implicit && *reserved {
            if nonblocking {
                return Err(ObjectError::WouldBlock);
            }
            if cancellation.is_some_and(OperationCancellation::interrupted) {
                return Err(ObjectError::Interrupted);
            }
            reserved = self.changed.wait(reserved).unwrap_or_else(|error| error.into_inner());
        }
        if implicit {
            *reserved = true;
        }
        drop(reserved);
        drop(subscription);
        match bytes() {
            Ok(bytes) => Ok(Box::new(PreparedRead {
                bytes,
                gate: implicit.then(|| Arc::clone(self)),
                commit: Some(Box::new(commit)),
            })),
            Err(error) => {
                if implicit {
                    self.release();
                }
                Err(error)
            }
        }
    }

    fn release(&self) {
        *self.reserved.lock().unwrap_or_else(|error| error.into_inner()) = false;
        self.changed.notify_all();
    }
}

struct PreparedRead {
    bytes: Vec<u8>,
    gate: Option<Arc<CursorGate>>,
    commit: Option<Box<dyn FnOnce(usize) -> Result<(), ObjectError> + Send>>,
}

impl PreparedSpliceRead for PreparedRead {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn commit(mut self: Box<Self>, count: usize) -> Result<(), ObjectError> {
        if count > self.bytes.len() {
            return Err(ObjectError::InvalidArgument);
        }
        let result = self.commit.take().expect("splice commit is single-use")(count);
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
        result
    }
}

impl Drop for PreparedRead {
    fn drop(&mut self) {
        if let Some(gate) = self.gate.take() {
            gate.release();
        }
    }
}
