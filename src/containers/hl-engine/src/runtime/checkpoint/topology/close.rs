use super::super::authority::CommitOutcome;
use super::super::authority::PrepareId;
use super::{
    model::{AdmissionError, CaptureChannel, CloseId, Epoch, ProcessIdentity, ResourceSnapshot, validate_topology},
    ticket::{Authority, Phase},
};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    time::Instant,
};

/// Owns the unpublished checkpoint generation. Dropping the admission never
/// loses the only object capable of rolling it back.
pub(crate) trait StorageGuard: Send {
    fn commit(&mut self) -> CommitOutcome;
    fn reconcile(&mut self) -> CommitOutcome;
    fn rollback(&mut self) -> Result<(), AdmissionError>;
}

/// Owns the resource freeze. A snapshot is immutable after construction and
/// release is explicit, so a second freeze cannot replace its digest.
pub(crate) trait FreezeGuard<D> {
    fn snapshot(&self) -> &ResourceSnapshot<D>;
    fn release(&mut self) -> Result<(), AdmissionError>;
}

/// Physical restore cleanup. Success must name exactly the processes proved
/// killed and reaped; bookkeeping alone never reopens admission.
pub(crate) trait Reaper: Send {
    fn kill_and_reap(&mut self, exact: &HashSet<ProcessIdentity>) -> Result<HashSet<ProcessIdentity>, AdmissionError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureOutcome {
    Commit,
    Abort,
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CaptureEvent {
    pub(super) epoch: Epoch,
    pub(super) close: CloseId,
    pub(super) process: ProcessIdentity,
    pub(super) channel: CaptureChannel,
    pub(super) task: PrepareId,
    pub(super) resource: PrepareId,
    pub(super) outcome: CaptureOutcome,
}

pub(super) struct CaptureState<D> {
    pub(super) snapshot: ResourceSnapshot<D>,
    terminal: HashMap<ProcessIdentity, CaptureOutcome>,
}

pub(super) struct CloseAdmission<'a, D, S: StorageGuard, F: FreezeGuard<D>> {
    authority: &'a Authority<D>,
    close: CloseId,
    storage: Option<S>,
    freeze: Option<F>,
    published: bool,
    settled: bool,
}

impl<'a, D: Copy + Eq + Hash, S: StorageGuard + 'static, F: FreezeGuard<D>> CloseAdmission<'a, D, S, F> {
    fn abort_resources(&mut self) -> Result<(), AdmissionError> {
        if self.freeze.as_mut().is_some_and(|freeze| freeze.release().is_ok()) {
            self.freeze.take();
        }
        if self.storage.as_mut().is_some_and(|storage| storage.rollback().is_ok()) {
            self.storage.take();
        }
        if self.freeze.is_none() && self.storage.is_none() {
            self.settled = true;
            Ok(())
        } else {
            Err(AdmissionError::Poisoned)
        }
    }

    pub(super) fn begin(authority: &'a Authority<D>, close: CloseId, storage: S) -> Result<Self, AdmissionError> {
        let mut state = authority.lock()?;
        if state.phase != Phase::Open || state.close != close {
            return Err(AdmissionError::Closed);
        }
        state.phase = Phase::Closing;
        drop(state);
        Ok(Self {
            authority,
            close,
            storage: Some(storage),
            freeze: None,
            published: false,
            settled: false,
        })
    }

    pub(super) fn drain(&self, deadline: Instant) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        while !state.tickets.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                return Err(AdmissionError::Deadline);
            }
            let waited = self
                .authority
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(now))
                .map_err(|_| AdmissionError::Poisoned)?;
            state = waited.0;
            if waited.1.timed_out() && !state.tickets.is_empty() {
                return Err(AdmissionError::Deadline);
            }
        }
        Ok(())
    }

    pub(super) fn freeze(&mut self, guard: F) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        if state.phase != Phase::Closing || self.freeze.is_some() || !state.tickets.is_empty() {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Conflict);
        }
        let snapshot = guard.snapshot();
        let exact = snapshot.channels.keys().copied().collect::<HashSet<_>>();
        let unique_channels = snapshot.channels.values().copied().collect::<HashSet<_>>();
        if exact != state.members
            || snapshot.channels.len() != state.members.len()
            || unique_channels.len() != state.members.len()
            || validate_topology(state.root, &state.members).is_err()
            || state.publications.len() != state.members.len()
            || state
                .publications
                .values()
                .any(|publication| publication.snapshot != snapshot.digest)
        {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Conflict);
        }
        state.capture = Some(CaptureState {
            snapshot: ResourceSnapshot {
                digest: snapshot.digest,
                channels: snapshot.channels.clone(),
            },
            terminal: HashMap::new(),
        });
        state.phase = Phase::Frozen;
        self.freeze = Some(guard);
        Ok(())
    }

    pub(super) fn publish(&mut self) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        if state.phase != Phase::Frozen || self.published {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Conflict);
        }
        state.phase = Phase::Published;
        self.published = true;
        Ok(())
    }

    pub(super) fn terminal(&mut self, event: CaptureEvent) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        if state.phase != Phase::Published || event.epoch != state.epoch || event.close != self.close {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Stale);
        }
        let publication = state.publications.get(&event.process).copied();
        let capture = state.capture.as_mut().ok_or(AdmissionError::Poisoned)?;
        if capture.snapshot.channels.get(&event.process) != Some(&event.channel)
            || capture.terminal.contains_key(&event.process)
            || publication
                .is_none_or(|publication| publication.task != event.task || publication.resource != event.resource)
        {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Unauthorized);
        }
        // EOF and GROUP_ABORT are authenticated terminal failures. They are
        // consumed exactly once, then poison the entire generation atomically.
        capture.terminal.insert(event.process, event.outcome);
        if event.outcome != CaptureOutcome::Commit {
            state.phase = Phase::Poisoned;
            self.authority.changed.notify_all();
            drop(state);
            return match self.abort_resources() {
                Ok(()) => Err(AdmissionError::Conflict),
                Err(error) => Err(error),
            };
        }
        Ok(())
    }

    pub(super) fn commit(mut self) -> Result<(), AdmissionError> {
        let mut state = self.authority.lock()?;
        let capture = state.capture.as_ref().ok_or(AdmissionError::Poisoned)?;
        if state.phase != Phase::Published
            || capture.terminal.len() != state.members.len()
            || capture
                .terminal
                .values()
                .any(|outcome| *outcome != CaptureOutcome::Commit)
        {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Conflict);
        }
        self.freeze.as_mut().ok_or(AdmissionError::Poisoned)?.release()?;
        self.freeze.take();
        let outcome = self.storage.as_mut().ok_or(AdmissionError::Poisoned)?.commit();
        match outcome {
            CommitOutcome::Published => {
                state.phase = Phase::Committed;
                self.settled = true;
                self.storage.take();
                Ok(())
            }
            CommitOutcome::PublishedNotDurable => {
                state.phase = Phase::Committed;
                self.settled = true;
                self.storage.take();
                Err(AdmissionError::NotDurable)
            }
            CommitOutcome::DefinitelyNotPublished => {
                self.storage.as_mut().ok_or(AdmissionError::Poisoned)?.rollback()?;
                self.storage.take();
                state.capture = None;
                state.phase = Phase::Open;
                self.settled = true;
                Err(AdmissionError::Conflict)
            }
            CommitOutcome::PublicationUnknown => {
                state.phase = Phase::Poisoned;
                state.ambiguous_storage = self
                    .storage
                    .take()
                    .map(|storage| Box::new(storage) as Box<dyn StorageGuard>);
                self.settled = true;
                Err(AdmissionError::Poisoned)
            }
        }
    }
}

impl<D, S: StorageGuard, F: FreezeGuard<D>> Drop for CloseAdmission<'_, D, S, F> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let released = self.freeze.as_mut().is_none_or(|freeze| freeze.release().is_ok());
        let rolled_back = self.storage.as_mut().is_some_and(|storage| storage.rollback().is_ok());
        let Ok(mut state) = self.authority.state.lock() else {
            return;
        };
        state.capture = None;
        state.phase = if !self.published && released && rolled_back {
            Phase::Open
        } else {
            Phase::Poisoned
        };
        self.authority.changed.notify_all();
    }
}

impl<D: Copy + Eq + Hash> Authority<D> {
    pub(super) fn reconcile_storage(&self) -> Result<CommitOutcome, AdmissionError> {
        let mut storage = {
            let mut state = self.lock()?;
            if state.phase != Phase::Poisoned {
                return Err(AdmissionError::Closed);
            }
            state.ambiguous_storage.take().ok_or(AdmissionError::Poisoned)?
        };
        let outcome = storage.reconcile();
        let mut state = self.lock()?;
        match outcome {
            CommitOutcome::Published | CommitOutcome::PublishedNotDurable => {
                state.phase = Phase::Committed;
            }
            CommitOutcome::DefinitelyNotPublished => {
                storage.rollback()?;
                state.capture = None;
                state.phase = Phase::Open;
            }
            CommitOutcome::PublicationUnknown => {
                state.ambiguous_storage = Some(storage);
                return Err(AdmissionError::Poisoned);
            }
        }
        Ok(outcome)
    }
}
