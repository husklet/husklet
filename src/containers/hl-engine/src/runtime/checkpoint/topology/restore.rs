use super::{
    close::Reaper,
    model::{AdmissionError, ChildRole, CloseId, Epoch, ParentRole, ProcessIdentity, SavedProcessIdentity, TicketId},
    ticket::{Authority, Event, ForkAdmission, Phase},
};
use std::{collections::HashSet, hash::Hash};

pub(super) struct PendingCleanup {
    reaper: Box<dyn Reaper>,
    exact: HashSet<ProcessIdentity>,
    epoch: Epoch,
    close: CloseId,
}

pub(crate) struct RestoreAdmission<'a, D: Copy + Eq + Hash> {
    pub(super) authority: &'a Authority<D>,
    pub(super) close: CloseId,
    pub(super) reaper: Option<Box<dyn Reaper>>,
    pub(super) settled: bool,
}

impl<D: Copy + Eq + Hash> RestoreAdmission<'_, D> {
    pub(super) fn reserve(
        &self,
        ticket: TicketId,
        parent_role: ParentRole,
        child_role: ChildRole,
        parent: ProcessIdentity,
        expected: SavedProcessIdentity,
    ) -> Result<ForkAdmission, AdmissionError> {
        self.authority
            .reserve_restore(ticket, parent_role, child_role, parent, expected)
    }

    pub(super) fn commit(mut self) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        if state.phase != Phase::Restoring
            || state.close != self.close
            || !state.expected_restore.is_empty()
            || state.restore_live != state.members
            || !state.tickets.is_empty()
        {
            return Err(Authority::<D>::poison(&mut state, AdmissionError::Conflict));
        }
        state.phase = Phase::Open;
        state.epoch = state.epoch.next()?;
        self.settled = true;
        self.reaper.take();
        self.authority.changed.notify_all();
        Ok(())
    }

    fn authenticate_parent(&self, event: Event<ParentRole>) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        if state.phase != Phase::Restoring || event.epoch != state.epoch || event.close != state.close {
            return Err(AdmissionError::Stale);
        }
        let ticket = state.tickets.get(&event.ticket).ok_or(AdmissionError::Stale)?;
        if ticket.parent_role != event.role {
            return Err(AdmissionError::Unauthorized);
        }
        state.phase = Phase::Poisoned;
        self.authority.changed.notify_all();
        Ok(())
    }

    fn authenticate_child(&self, event: Event<ChildRole>) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        if state.phase != Phase::Restoring || event.epoch != state.epoch || event.close != state.close {
            return Err(AdmissionError::Stale);
        }
        let ticket = state.tickets.get(&event.ticket).ok_or(AdmissionError::Stale)?;
        if ticket.child_role != event.role {
            return Err(AdmissionError::Unauthorized);
        }
        state.phase = Phase::Poisoned;
        self.authority.changed.notify_all();
        Ok(())
    }

    pub(super) fn parent_lost(&mut self, event: Event<ParentRole>) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        self.authenticate_parent(event)?;
        self.abort_owned()
    }

    pub(super) fn child_lost(&mut self, event: Event<ChildRole>) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        self.authenticate_child(event)?;
        self.abort_owned()
    }

    fn install_pending(&mut self, exact: HashSet<ProcessIdentity>) {
        let Some(reaper) = self.reaper.take() else {
            return;
        };
        if let Ok(mut state) = self.authority.state.lock() {
            state.phase = Phase::Poisoned;
            state.cleanup = Some(PendingCleanup {
                reaper,
                exact,
                epoch: state.epoch,
                close: state.close,
            });
            self.settled = true;
            self.authority.changed.notify_all();
        }
    }

    fn abort_owned(&mut self) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        let exact = self.authority.lock()?.restore_processes();
        let result = self
            .reaper
            .as_mut()
            .ok_or(AdmissionError::Poisoned)?
            .kill_and_reap(&exact);
        if result.as_ref() != Ok(&exact) {
            self.install_pending(exact);
            return Err(AdmissionError::Poisoned);
        }
        self.authority.finish_cleanup()?;
        self.settled = true;
        self.reaper.take();
        Ok(exact)
    }

    pub(super) fn abort(mut self) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        self.abort_owned()
    }
}

impl<D: Copy + Eq + Hash> Drop for RestoreAdmission<'_, D> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let exact = self
            .authority
            .state
            .lock()
            .map(|state| state.restore_processes())
            .unwrap_or_default();
        let result = self
            .reaper
            .as_mut()
            .and_then(|reaper| reaper.kill_and_reap(&exact).ok());
        if result.as_ref() == Some(&exact) {
            let _ = self.authority.finish_cleanup();
            self.settled = true;
            self.reaper.take();
        } else {
            self.install_pending(exact);
        }
    }
}

impl<D: Copy + Eq + Hash> Authority<D> {
    fn finish_cleanup(&self) -> Result<(), AdmissionError> {
        let mut state = self.lock()?;
        state.tickets.clear();
        state.expected_restore.clear();
        state.reserved_restore.clear();
        state.reservation_members = None;
        state.restore_live.clear();
        state.expected_snapshot = None;
        state.members.clear();
        state.publications.clear();
        state.lifecycles.clear();
        state.member_ordinals.clear();
        state.epoch = state.epoch.next()?;
        state.phase = Phase::Open;
        state.cleanup = None;
        self.changed.notify_all();
        Ok(())
    }

    pub(super) fn retry_cleanup(&self) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        let mut pending = {
            let mut state = self.lock()?;
            if state.phase != Phase::Poisoned {
                return Err(AdmissionError::Closed);
            }
            state.cleanup.take().ok_or(AdmissionError::Poisoned)?
        };
        let result = pending.reaper.kill_and_reap(&pending.exact);
        let mut state = self.lock()?;
        if state.epoch != pending.epoch || state.close != pending.close {
            state.cleanup = Some(pending);
            return Err(AdmissionError::Stale);
        }
        if result.as_ref() != Ok(&pending.exact) {
            state.cleanup = Some(pending);
            return Err(if result.is_err() {
                AdmissionError::Poisoned
            } else {
                AdmissionError::Conflict
            });
        }
        let exact = pending.exact.clone();
        drop(state);
        self.finish_cleanup()?;
        Ok(exact)
    }
}
