use super::{CaptureFailure, CapturePhase, Server, State};
use std::num::NonZeroU64;

pub(super) struct AbortTransition<'a> {
    pub(super) server: &'a Server,
    pub(super) id: u64,
    pub(super) finished: bool,
}

impl Drop for AbortTransition<'_> {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut capture = match self.server.capture.lock() {
            Ok(capture) => capture,
            Err(poisoned) => poisoned.into_inner(),
        };
        if matches!(capture.phase, CapturePhase::Aborting { id } if id == self.id) {
            capture.phase = CapturePhase::Poisoned;
        }
        self.server.capture.clear_poison();
        self.server.capture_changed.notify_all();
        drop(capture);
        self.server.interrupt_channels();
    }
}

impl Server {
    pub(super) fn transaction_token(&self) -> Result<NonZeroU64, CaptureFailure> {
        self.transaction
            .lock()
            .map_err(|_| CaptureFailure::Poisoned)?
            .ok_or(CaptureFailure::Poisoned)
    }

    pub(super) fn begin_transaction(&self, deadline: std::time::Instant) -> Result<(), CaptureFailure> {
        let transaction = self.sink.begin_until(deadline).map_err(Self::publication_failure)?;
        let installed = match self.transaction.lock() {
            Ok(mut active) if active.is_none() => {
                *active = Some(transaction);
                true
            }
            _ => false,
        };
        if !installed {
            let _ = self.sink.abort_until(transaction, deadline);
            return Err(CaptureFailure::Poisoned);
        }
        if let Ok(mut state) = self.state.lock() {
            *state = State::default();
            Ok(())
        } else {
            let _ = self.sink.abort_until(transaction, deadline);
            if let Ok(mut active) = self.transaction.lock() {
                *active = None;
            }
            Err(CaptureFailure::Poisoned)
        }
    }

    pub(super) fn discard_transaction(&self, deadline: std::time::Instant) -> Result<(), CaptureFailure> {
        let transaction = self.transaction_token()?;
        let storage = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.sink.abort_until(transaction, deadline)
        }))
        .map_err(|_| CaptureFailure::Poisoned)
        .and_then(|result| result.map_err(Self::publication_failure));
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        *state = State::default();
        self.state.clear_poison();
        if let Ok(mut active) = self.transaction.lock()
            && *active == Some(transaction)
        {
            *active = None;
        }
        storage
    }
}
