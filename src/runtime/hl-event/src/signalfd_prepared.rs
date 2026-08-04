use super::{PreparedSignalSelection, SIGNALFD_RECORD_SIZE, SignalFd, SignalFdError};
use hl_descriptor::{ObjectError, PreparedAtomicRead};

pub(super) struct AtomicSignalRead {
    bytes: [u8; SIGNALFD_RECORD_SIZE],
    selection: Box<dyn PreparedSignalSelection>,
    signal: SignalFd,
}

impl AtomicSignalRead {
    pub(super) fn prepare(
        signal: &SignalFd,
        maximum: usize,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        Self::prepare_selection(signal, maximum, |queue, mask| queue.prepare(mask))
    }

    pub(super) fn prepare_context(
        signal: &SignalFd,
        maximum: usize,
        actor: hl_descriptor::OperationActor,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        Self::prepare_selection(signal, maximum, |queue, mask| queue.prepare_context(mask, actor))
    }

    fn prepare_selection(
        signal: &SignalFd,
        maximum: usize,
        prepare: impl FnOnce(
            &dyn super::SignalQueue,
            super::SignalMask,
        ) -> Result<Option<Box<dyn PreparedSignalSelection>>, super::SignalQueueError>,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        if maximum < SIGNALFD_RECORD_SIZE {
            return Err(ObjectError::InvalidArgument);
        }
        let mask = signal.current_mask().map_err(SignalFdError::object_error)?;
        let selection =
            prepare(signal.inner.queue.as_ref(), mask).map_err(|error| SignalFdError::from(error).object_error())?;
        let Some(selection) = selection else {
            return Err(ObjectError::WouldBlock);
        };
        Ok(Some(Box::new(Self {
            bytes: selection.info().encode(),
            selection,
            signal: signal.clone(),
        })))
    }
}

impl PreparedAtomicRead for AtomicSignalRead {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn commit(self: Box<Self>) -> Result<(), ObjectError> {
        match self.selection.commit() {
            Ok(true) => {
                self.signal.inner.readiness.notify();
                Ok(())
            }
            Ok(false) => Err(ObjectError::Interrupted),
            Err(error) => Err(SignalFdError::from(error).object_error()),
        }
    }
}

pub(super) use AtomicSignalRead as AtomicRead;
