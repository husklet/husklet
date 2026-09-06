use super::{
    ABORT_SETTLEMENT_TIMEOUT, CaptureFailure, CapturePhase, CaptureState, Ordering, ParticipantLedger, Server,
    authority,
};

impl Server {
    #[cfg(test)]
    pub(crate) fn begin_capture(&self, generation: u32, deadline: std::time::Instant) -> Result<u64, CaptureFailure> {
        self.begin_capture_after_admission(deadline, || generation)
    }

    pub(crate) fn begin_capture_after_admission(
        &self,
        deadline: std::time::Instant,
        activate: impl FnOnce() -> u32,
    ) -> Result<u64, CaptureFailure> {
        if std::time::Instant::now() >= deadline {
            return Err(CaptureFailure::Deadline);
        }
        let mut capture = self.capture_lock()?;
        if !matches!(capture.phase, CapturePhase::Idle) {
            return Err(match capture.phase {
                CapturePhase::Poisoned => CaptureFailure::Poisoned,
                _ => CaptureFailure::Busy,
            });
        }
        self.begin_transaction(deadline)?;
        let id = u64::from(activate());
        if id == 0 {
            let discarded = self.discard_transaction(deadline);
            capture.phase = CapturePhase::Poisoned;
            self.capture_changed.notify_all();
            discarded?;
            return Err(CaptureFailure::Poisoned);
        }
        self.committed.store(false, Ordering::Release);
        // Each capture seals its own membership: a ledger from an earlier capture
        // must never admit a process into this one.
        *self.participants.lock().map_err(|_| CaptureFailure::Poisoned)? =
            Some(ParticipantLedger::new(id).map_err(|_| CaptureFailure::Poisoned)?);
        // The announced members are the processes this capture is about to freeze and retire. Holding
        // them past it would hand a caller a capability on a process the image has replaced.
        self.members.clear();
        // Same reasoning for the terminals: an unclaimed registration names a member of the tree this
        // capture is sealing, and the next restore registers its own.
        self.member_terminals.clear();
        capture.phase = CapturePhase::Active { id, deadline };
        capture.mutations = 0;
        capture.recovery_report_published = false;
        Ok(id)
    }

    pub(crate) fn wait_capture_ready(&self, deadline: std::time::Instant) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        loop {
            match capture.phase {
                CapturePhase::Idle => return Ok(()),
                CapturePhase::Recovery { .. }
                | CapturePhase::RecoveryFinished { .. }
                | CapturePhase::Aborting { .. } => {
                    let now = std::time::Instant::now();
                    if now >= deadline {
                        return Err(CaptureFailure::Deadline);
                    }
                    let (next, timeout) = self
                        .capture_changed
                        .wait_timeout(capture, deadline.saturating_duration_since(now))
                        .map_err(|_| CaptureFailure::Poisoned)?;
                    capture = next;
                    if timeout.timed_out() && !matches!(capture.phase, CapturePhase::Idle) {
                        return Err(CaptureFailure::Deadline);
                    }
                }
                CapturePhase::Poisoned => return Err(CaptureFailure::Poisoned),
                _ => return Err(CaptureFailure::Busy),
            }
        }
    }

    /// Selects the reader only after the offered generation proves it is finalized.
    ///
    /// The checkpoint byte store is adversarial: it can offer a staged
    /// generation, a truncated one, or a directory an attacker assembled. Native
    /// restore reads whatever recovery admits, so both finalization and the
    /// bounded `IMAGE` discriminator have to be validated here, before the
    /// recovery generation is activated and before any payload object is served.
    fn recovery_reader(&self, deadline: std::time::Instant) -> Result<super::image_envelope::Reader, CaptureFailure> {
        let names = self
            .source
            .list_until(deadline)
            .map_err(|_| CaptureFailure::Unfinalized)?;
        if !authority::RecordState::of_generation(&names).admits_recovery() {
            hl_log::hl_error!(
                hl_log::tag::CHECKPOINT,
                "checkpoint recovery refused: generation is prepared, not finalized"
            );
            return Err(CaptureFailure::Unfinalized);
        }
        if !names.iter().any(|name| name == super::image_envelope::OBJECT) {
            // Images written before the discriminator existed are translated
            // images whose current reader starts at MANIFEST.
            return Ok(super::image_envelope::Reader::Translated);
        }
        let bytes = self
            .source
            .get_until(super::image_envelope::OBJECT, deadline)
            .map_err(|error| match error {
                crate::composition::CompositionError::DeadlineExceeded => CaptureFailure::Deadline,
                _ => CaptureFailure::InvalidImage,
            })?;
        super::image_envelope::Reader::decode(&bytes).map_err(|_| CaptureFailure::InvalidImage)
    }

    #[cfg(test)]
    pub(crate) fn begin_recovery(&self, generation: u32, deadline: std::time::Instant) -> Result<u64, CaptureFailure> {
        self.begin_recovery_after_admission(deadline, || generation)
    }

    pub(crate) fn begin_recovery_after_admission(
        &self,
        deadline: std::time::Instant,
        activate: impl FnOnce() -> u32,
    ) -> Result<u64, CaptureFailure> {
        if std::time::Instant::now() >= deadline {
            return Err(CaptureFailure::Deadline);
        }
        match self.recovery_reader(deadline)? {
            super::image_envelope::Reader::Translated => {}
            super::image_envelope::Reader::NativeX86V1 => return Err(CaptureFailure::UnsupportedImage),
        }
        let mut capture = self.capture_lock()?;
        if !matches!(capture.phase, CapturePhase::Idle) {
            return Err(match capture.phase {
                CapturePhase::Poisoned => CaptureFailure::Poisoned,
                _ => CaptureFailure::Busy,
            });
        }
        self.begin_transaction(deadline)?;
        let id = u64::from(activate());
        if id == 0 {
            let discarded = self.discard_transaction(deadline);
            capture.phase = CapturePhase::Poisoned;
            self.capture_changed.notify_all();
            discarded?;
            return Err(CaptureFailure::Poisoned);
        }
        // RecoveryAdmission is a linear owner: its scoped waiter must finish before another recovery
        // can admit. Clearing the sole retained result here is therefore both bounded retention and
        // an explicit fence against 32-bit generation reuse.
        capture.recovery_result = None;
        capture.phase = CapturePhase::Recovery { id, deadline };
        capture.mutations = 0;
        capture.recovery_report_published = false;
        Ok(id)
    }

    pub(crate) fn abort_recovery(&self, id: u64) -> Result<(), CaptureFailure> {
        self.settle_recovery(id, None)
    }

    pub(crate) fn fail_recovery(&self, id: u64, failure: CaptureFailure) -> Result<(), CaptureFailure> {
        self.settle_recovery(id, Some(failure))
    }

    fn settle_recovery(&self, id: u64, failure: Option<CaptureFailure>) -> Result<(), CaptureFailure> {
        let settlement_deadline = std::time::Instant::now() + ABORT_SETTLEMENT_TIMEOUT;
        let mut capture = self.capture_lock()?;
        match capture.phase {
            CapturePhase::Recovery { id: active, .. } if active == id => {
                capture.phase = CapturePhase::Aborting { id };
                self.capture_changed.notify_all();
                drop(capture);
                self.interrupt_channels();
                let mut capture = self.capture_lock()?;
                while capture.mutations != 0 {
                    let now = std::time::Instant::now();
                    if now >= settlement_deadline {
                        capture.phase = CapturePhase::Poisoned;
                        self.capture_changed.notify_all();
                        return Err(CaptureFailure::Deadline);
                    }
                    let (next, timeout) = self
                        .capture_changed
                        .wait_timeout(capture, settlement_deadline.saturating_duration_since(now))
                        .map_err(|_| CaptureFailure::Poisoned)?;
                    capture = next;
                    if timeout.timed_out() && capture.mutations != 0 {
                        capture.phase = CapturePhase::Poisoned;
                        self.capture_changed.notify_all();
                        return Err(CaptureFailure::Deadline);
                    }
                }
                drop(capture);
                let discarded = self.discard_transaction(settlement_deadline);
                let mut capture = self.capture_lock()?;
                if !matches!(capture.phase, CapturePhase::Aborting { id: active } if active == id) {
                    capture.phase = CapturePhase::Poisoned;
                    self.capture_changed.notify_all();
                    return Err(CaptureFailure::Poisoned);
                }
                capture.phase = if discarded.is_ok() {
                    failure.map_or(CapturePhase::Idle, |failure| CapturePhase::RecoveryFinished {
                        id,
                        result: Err(failure),
                    })
                } else {
                    CapturePhase::Poisoned
                };
                self.capture_changed.notify_all();
                discarded
            }
            CapturePhase::RecoveryFinished { id: active, result } if active == id => {
                capture.phase = CapturePhase::Idle;
                self.capture_changed.notify_all();
                result
            }
            CapturePhase::Idle => Ok(()),
            CapturePhase::Poisoned => {
                drop(capture);
                let discarded = self.discard_transaction(settlement_deadline);
                if discarded.is_ok() {
                    let mut capture = self.capture_lock()?;
                    capture.phase = failure.map_or(CapturePhase::Idle, |failure| CapturePhase::RecoveryFinished {
                        id,
                        result: Err(failure),
                    });
                    self.capture_changed.notify_all();
                }
                discarded
            }
            _ => Err(CaptureFailure::Busy),
        }
    }

    /// Waits for a running recovery to change phase, failing it when its deadline passes first.
    ///
    /// Answers the reacquired guard either way, so the caller re-reads the phase from the top.
    fn await_recovery_change<'a>(
        &'a self,
        capture: std::sync::MutexGuard<'a, CaptureState>,
        id: u64,
        deadline: std::time::Instant,
    ) -> Result<std::sync::MutexGuard<'a, CaptureState>, CaptureFailure> {
        let now = std::time::Instant::now();
        if now >= deadline {
            drop(capture);
            self.fail_recovery(id, CaptureFailure::Deadline)?;
            return self.capture_lock();
        }
        let (next, timeout) = self
            .capture_changed
            .wait_timeout(capture, deadline.saturating_duration_since(now))
            .map_err(|_| CaptureFailure::Poisoned)?;
        if !timeout.timed_out() {
            return Ok(next);
        }
        drop(next);
        self.fail_recovery(id, CaptureFailure::Deadline)?;
        self.capture_lock()
    }

    pub(crate) fn wait_recovery(&self, id: u64) -> Result<(), CaptureFailure> {
        let mut capture = self.capture_lock()?;
        loop {
            if let Some((completed, result)) = capture.recovery_result
                && completed == id
            {
                return result;
            }
            match capture.phase {
                CapturePhase::Recovery { id: active, deadline } if active == id => {
                    capture = self.await_recovery_change(capture, id, deadline)?;
                }
                CapturePhase::Aborting { id: active } if active == id => {
                    capture = self
                        .capture_changed
                        .wait(capture)
                        .map_err(|_| CaptureFailure::Poisoned)?;
                }
                CapturePhase::RecoveryFinished { id: active, result } if active == id => {
                    capture.recovery_result = Some((id, result));
                    capture.phase = CapturePhase::Idle;
                    self.capture_changed.notify_all();
                    return result;
                }
                CapturePhase::Idle => return Ok(()),
                CapturePhase::Poisoned => {
                    drop(capture);
                    self.fail_recovery(id, CaptureFailure::Poisoned)?;
                    capture = self.capture_lock()?;
                }
                _ => return Err(CaptureFailure::Busy),
            }
        }
    }
}
