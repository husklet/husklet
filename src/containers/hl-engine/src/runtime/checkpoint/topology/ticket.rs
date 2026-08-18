use super::super::authority::PrepareId;
use super::restore::RestoreAdmission;
use super::{
    close::{CloseAdmission, Reaper},
    model::{
        AdmissionError, CheckpointGeneration, ChildRole, CloseId, Epoch, LifecycleRole, LineageId, MemberOrdinal,
        OfdId, OfdNamespace, ParentRole, ProcessIdentity, Publication, SavedProcessIdentity, TicketId,
        validate_saved_topology,
    },
};
use std::{
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::{Condvar, Mutex, MutexGuard},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Event<C> {
    pub(super) epoch: Epoch,
    pub(super) close: CloseId,
    pub(super) ticket: TicketId,
    pub(super) role: C,
}

pub(crate) struct ForkAdmission {
    pub(super) event: Event<ParentRole>,
    pub(super) child: Event<ChildRole>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TerminalEvent {
    pub(super) admission: Event<ParentRole>,
    pub(super) task: PrepareId,
    pub(super) resource: PrepareId,
    pub(super) lifecycle: LifecycleRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LifecycleEvent {
    pub(super) epoch: Epoch,
    pub(super) close: CloseId,
    pub(super) process: ProcessIdentity,
    pub(super) role: LifecycleRole,
}

pub(super) struct TicketState<D> {
    parent: ProcessIdentity,
    expected: Option<SavedProcessIdentity>,
    pub(super) parent_role: ParentRole,
    pub(super) child_role: ChildRole,
    pub(super) parent_report: Option<ProcessIdentity>,
    pub(super) child_report: Option<ProcessIdentity>,
    pub(super) started: Option<ProcessIdentity>,
    child_ready: bool,
    publication: Option<Publication<D>>,
    released: bool,
    consumed: bool,
    terminal: bool,
    restore: bool,
    member: MemberOrdinal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    Open,
    Closing,
    Frozen,
    Published,
    Committed,
    Restoring,
    Poisoned,
}

pub(super) struct State<D> {
    pub(super) epoch: Epoch,
    pub(super) close: CloseId,
    pub(super) phase: Phase,
    pub(super) root: ProcessIdentity,
    pub(super) lineage: LineageId,
    pub(super) generation: CheckpointGeneration,
    pub(super) member_ordinals: HashMap<ProcessIdentity, MemberOrdinal>,
    pub(super) next_member: MemberOrdinal,
    pub(super) ofd_next: HashMap<MemberOrdinal, std::num::NonZeroU64>,
    pub(super) members: HashSet<ProcessIdentity>,
    pub(super) publications: HashMap<ProcessIdentity, Publication<D>>,
    pub(super) lifecycles: HashMap<ProcessIdentity, LifecycleRole>,
    pub(super) tickets: HashMap<TicketId, TicketState<D>>,
    pub(super) expected_restore: HashSet<SavedProcessIdentity>,
    pub(super) reserved_restore: HashSet<SavedProcessIdentity>,
    pub(super) restored: HashMap<SavedProcessIdentity, ProcessIdentity>,
    pub(super) reservation_members: Option<HashSet<MemberOrdinal>>,
    pub(super) restore_live: HashSet<ProcessIdentity>,
    pub(super) expected_snapshot: Option<super::model::ResourceSnapshot<D, SavedProcessIdentity>>,
    pub(super) capture: Option<super::close::CaptureState<D>>,
    pub(super) cleanup: Option<super::restore::PendingCleanup>,
    pub(super) ambiguous_storage: Option<Box<dyn super::StorageGuard>>,
}

impl<D> State<D> {
    pub(super) fn restore_processes(&self) -> HashSet<ProcessIdentity> {
        let mut exact = self.restore_live.clone();
        exact.extend(
            self.tickets
                .values()
                .filter(|ticket| ticket.restore)
                .filter_map(|ticket| ticket.child_report.or(ticket.parent_report).or(ticket.started)),
        );
        exact
    }
}

pub(crate) struct Authority<D> {
    pub(super) state: Mutex<State<D>>,
    pub(super) changed: Condvar,
}

impl<D: Copy + Eq + Hash> Authority<D> {
    pub(super) fn new(
        epoch: Epoch,
        close: CloseId,
        root: ProcessIdentity,
        root_publication: Publication<D>,
        root_lifecycle: LifecycleRole,
        lineage: LineageId,
        generation: CheckpointGeneration,
    ) -> Result<Self, AdmissionError> {
        if root.parent.is_some()
            || root_lifecycle.0 == root_publication.task
            || root_lifecycle.0 == root_publication.resource
        {
            return Err(AdmissionError::Conflict);
        }
        Ok(Self {
            state: Mutex::new(State {
                epoch,
                close,
                phase: Phase::Open,
                root,
                lineage,
                generation,
                member_ordinals: HashMap::from([(root, MemberOrdinal::new(1)?)]),
                next_member: MemberOrdinal::new(2)?,
                ofd_next: HashMap::from([(MemberOrdinal::new(1)?, std::num::NonZeroU64::MIN)]),
                members: HashSet::from([root]),
                publications: HashMap::from([(root, root_publication)]),
                lifecycles: HashMap::from([(root, root_lifecycle)]),
                tickets: HashMap::new(),
                expected_restore: HashSet::new(),
                reserved_restore: HashSet::new(),
                restored: HashMap::new(),
                reservation_members: None,
                restore_live: HashSet::new(),
                expected_snapshot: None,
                capture: None,
                cleanup: None,
                ambiguous_storage: None,
            }),
            changed: Condvar::new(),
        })
    }

    pub(super) fn lock(&self) -> Result<MutexGuard<'_, State<D>>, AdmissionError> {
        match self.state.lock() {
            Ok(state) => Ok(state),
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.phase = Phase::Poisoned;
                self.state.clear_poison();
                self.changed.notify_all();
                Err(AdmissionError::Poisoned)
            }
        }
    }

    fn context<C: Copy + Eq>(state: &State<D>, event: Event<C>, role: C) -> Result<(), AdmissionError> {
        if event.epoch != state.epoch || event.close != state.close {
            return Err(AdmissionError::Stale);
        }
        (event.role == role).then_some(()).ok_or(AdmissionError::Unauthorized)
    }

    pub(super) fn poison(state: &mut State<D>, error: AdmissionError) -> AdmissionError {
        state.phase = Phase::Poisoned;
        error
    }

    fn insert(
        state: &mut State<D>,
        ticket: TicketId,
        parent_role: ParentRole,
        child_role: ChildRole,
        parent: ProcessIdentity,
        expected: Option<SavedProcessIdentity>,
        restore: bool,
    ) -> Result<ForkAdmission, AdmissionError> {
        let restore_root = restore && expected.is_some_and(|child| child.parent.is_none());
        if parent_role.0 == child_role.0
            || state.tickets.contains_key(&ticket)
            || (!state.members.contains(&parent) && !restore_root)
        {
            return Err(Self::poison(state, AdmissionError::Conflict));
        }
        let member = if let Some(expected) = expected {
            expected.member
        } else {
            let member = state.next_member;
            state.next_member = member.next()?;
            member
        };
        state.tickets.insert(
            ticket,
            TicketState {
                parent,
                expected,
                parent_role,
                child_role,
                parent_report: None,
                child_report: None,
                started: None,
                child_ready: false,
                publication: None,
                released: false,
                consumed: false,
                terminal: false,
                restore,
                member,
            },
        );
        Ok(ForkAdmission {
            event: Event {
                epoch: state.epoch,
                close: state.close,
                ticket,
                role: parent_role,
            },
            child: Event {
                epoch: state.epoch,
                close: state.close,
                ticket,
                role: child_role,
            },
        })
    }

    pub(super) fn reserve_fork(
        &self,
        ticket: TicketId,
        parent_role: ParentRole,
        child_role: ChildRole,
        parent: ProcessIdentity,
    ) -> Result<ForkAdmission, AdmissionError> {
        let mut state = self.lock()?;
        if state.phase != Phase::Open {
            return Err(if state.phase == Phase::Poisoned {
                AdmissionError::Poisoned
            } else {
                AdmissionError::Closed
            });
        }
        Self::insert(&mut state, ticket, parent_role, child_role, parent, None, false)
    }

    fn update<C: Copy + Eq>(
        &self,
        event: Event<C>,
        role: C,
        operation: impl FnOnce(&mut TicketState<D>) -> Result<(), AdmissionError>,
    ) -> Result<(), AdmissionError> {
        let mut state = self.lock()?;
        if state.phase == Phase::Poisoned {
            return Err(AdmissionError::Poisoned);
        }
        Self::context(&state, event, role)?;
        let result = state
            .tickets
            .get_mut(&event.ticket)
            .ok_or(AdmissionError::Stale)
            .and_then(operation);
        if matches!(result, Err(AdmissionError::Conflict | AdmissionError::Unauthorized)) {
            state.phase = Phase::Poisoned;
            self.changed.notify_all();
        }
        result
    }

    pub(super) fn parent_report(&self, event: Event<ParentRole>, child: ProcessIdentity) -> Result<(), AdmissionError> {
        self.update(event, event.role, |ticket| {
            let root_edge = ticket.restore && ticket.expected.is_some_and(|expected| expected.parent.is_none());
            if ticket.parent_role != event.role
                || (!root_edge && child.parent != Some(ticket.parent.key))
                || ticket.parent_report.is_some()
                || ticket.started != Some(child)
            {
                return Err(AdmissionError::Unauthorized);
            }
            if ticket.child_report.is_some_and(|reported| reported != child) {
                return Err(AdmissionError::Conflict);
            }
            ticket.parent_report = Some(child);
            Ok(())
        })
    }

    pub(super) fn child_report(&self, event: Event<ChildRole>, child: ProcessIdentity) -> Result<(), AdmissionError> {
        self.update(event, event.role, |ticket| {
            if ticket.child_role != event.role || ticket.child_report.is_some() || ticket.started != Some(child) {
                return Err(AdmissionError::Unauthorized);
            }
            if ticket.parent_report.is_some_and(|reported| reported != child) {
                return Err(AdmissionError::Conflict);
            }
            ticket.child_report = Some(child);
            Ok(())
        })
    }

    pub(super) fn process_started(
        &self,
        event: Event<ChildRole>,
        child: ProcessIdentity,
    ) -> Result<(), AdmissionError> {
        self.update(event, event.role, |ticket| {
            let root_edge = ticket.restore && ticket.expected.is_some_and(|expected| expected.parent.is_none());
            if ticket.child_role != event.role
                || ticket.started.is_some()
                || (!root_edge && child.parent != Some(ticket.parent.key))
            {
                return Err(AdmissionError::Unauthorized);
            }
            ticket.started = Some(child);
            Ok(())
        })
    }

    pub(super) fn child_ready(&self, event: Event<ChildRole>) -> Result<(), AdmissionError> {
        self.update(event, event.role, |ticket| {
            if ticket.child_role != event.role || ticket.child_report.is_none() || ticket.child_ready {
                return Err(AdmissionError::Unauthorized);
            }
            ticket.child_ready = true;
            Ok(())
        })
    }

    pub(super) fn published(
        &self,
        event: Event<ParentRole>,
        publication: Publication<D>,
    ) -> Result<(), AdmissionError> {
        let mut state = self.lock()?;
        if state.phase == Phase::Poisoned {
            return Err(AdmissionError::Poisoned);
        }
        Self::context(&state, event, event.role)?;
        let restore_digest = state.expected_snapshot.as_ref().map(|snapshot| snapshot.digest);
        let ticket = state.tickets.get_mut(&event.ticket).ok_or(AdmissionError::Stale)?;
        if ticket.parent_role != event.role || ticket.parent_report.is_none() || ticket.publication.is_some() {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Unauthorized);
        }
        if ticket.restore && restore_digest != Some(publication.snapshot) {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Conflict);
        }
        ticket.publication = Some(publication);
        Ok(())
    }

    pub(super) fn release(&self, event: Event<ParentRole>) -> Result<(), AdmissionError> {
        self.update(event, event.role, |ticket| {
            if ticket.parent_role != event.role
                || ticket.parent_report.is_none()
                || ticket.parent_report != ticket.child_report
                || !ticket.child_ready
                || ticket.publication.is_none()
                || ticket.released
            {
                return Err(AdmissionError::Unauthorized);
            }
            ticket.released = true;
            Ok(())
        })
    }

    pub(super) fn consume(&self, event: Event<ChildRole>) -> Result<(), AdmissionError> {
        self.update(event, event.role, |ticket| {
            if ticket.child_role != event.role || !ticket.released || ticket.consumed {
                return Err(AdmissionError::Unauthorized);
            }
            ticket.consumed = true;
            Ok(())
        })
    }

    pub(super) fn terminal(&self, terminal: TerminalEvent) -> Result<ProcessIdentity, AdmissionError> {
        let event = terminal.admission;
        let mut state = self.lock()?;
        if state.phase == Phase::Poisoned {
            return Err(AdmissionError::Poisoned);
        }
        Self::context(&state, event, event.role)?;
        let valid = state.tickets.get(&event.ticket).is_some_and(|ticket| {
            ticket.parent_role == event.role
                && ticket.consumed
                && !ticket.terminal
                && ticket.publication.is_some_and(|publication| {
                    publication.task == terminal.task && publication.resource == terminal.resource
                })
                && ticket.parent_role.0 != terminal.lifecycle.0
                && ticket.child_role.0 != terminal.lifecycle.0
        });
        if !valid {
            return Err(Self::poison(&mut state, AdmissionError::Unauthorized));
        }
        let mut ticket = state.tickets.remove(&event.ticket).expect("validated ticket");
        ticket.terminal = true;
        let child = ticket.child_report.expect("consumed ticket has child");
        if !state.members.insert(child) {
            return Err(Self::poison(&mut state, AdmissionError::Conflict));
        }
        if ticket.restore {
            let expected = ticket.expected.expect("restore ticket has saved identity");
            if !state.expected_restore.remove(&expected) || state.restored.insert(expected, child).is_some() {
                return Err(Self::poison(&mut state, AdmissionError::Conflict));
            }
            state.reserved_restore.remove(&expected);
            if expected.parent.is_none() {
                state.root = child;
            }
        }
        state.restore_live.insert(child);
        state
            .publications
            .insert(child, ticket.publication.expect("terminal publication"));
        state.lifecycles.insert(child, terminal.lifecycle);
        if state.member_ordinals.insert(child, ticket.member).is_some() {
            return Err(Self::poison(&mut state, AdmissionError::Conflict));
        }
        state.ofd_next.entry(ticket.member).or_insert(std::num::NonZeroU64::MIN);
        self.changed.notify_all();
        Ok(child)
    }

    pub(super) fn begin_close<S: super::StorageGuard + 'static, F: super::FreezeGuard<D>>(
        &self,
        close: CloseId,
        storage: S,
    ) -> Result<CloseAdmission<'_, D, S, F>, AdmissionError> {
        CloseAdmission::begin(self, close, storage)
    }

    pub(super) fn cancel_fork(
        &self,
        event: Event<ChildRole>,
        reaper: &mut dyn Reaper,
    ) -> Result<HashSet<ProcessIdentity>, AdmissionError> {
        let mut state = self.lock()?;
        if state.phase != Phase::Open || event.epoch != state.epoch || event.close != state.close {
            return Err(AdmissionError::Stale);
        }
        let ticket = state.tickets.get(&event.ticket).ok_or(AdmissionError::Stale)?;
        if ticket.child_role != event.role {
            return Err(AdmissionError::Unauthorized);
        }
        let exact = ticket.started.into_iter().collect::<HashSet<_>>();
        if reaper.kill_and_reap(&exact)? != exact {
            state.phase = Phase::Poisoned;
            return Err(AdmissionError::Poisoned);
        }
        state.tickets.remove(&event.ticket);
        self.changed.notify_all();
        Ok(exact)
    }

    pub(super) fn exec(&self, event: LifecycleEvent) -> Result<(), AdmissionError> {
        let state = self.lock()?;
        if state.phase != Phase::Open || event.epoch != state.epoch || event.close != state.close {
            return Err(AdmissionError::Stale);
        }
        (state.lifecycles.get(&event.process) == Some(&event.role))
            .then_some(())
            .ok_or(AdmissionError::Unauthorized)
    }

    pub(super) fn allocate_ofd(&self, event: LifecycleEvent) -> Result<OfdId, AdmissionError> {
        let mut state = self.lock()?;
        if state.phase != Phase::Open
            || event.epoch != state.epoch
            || event.close != state.close
            || state.lifecycles.get(&event.process) != Some(&event.role)
        {
            return Err(AdmissionError::Unauthorized);
        }
        let member = *state
            .member_ordinals
            .get(&event.process)
            .ok_or(AdmissionError::Conflict)?;
        let sequence = *state.ofd_next.get(&member).ok_or(AdmissionError::Conflict)?;
        let next = sequence
            .get()
            .checked_add(1)
            .and_then(std::num::NonZeroU64::new)
            .ok_or_else(|| {
                state.phase = Phase::Poisoned;
                AdmissionError::Poisoned
            })?;
        state.ofd_next.insert(member, next);
        Ok(OfdId {
            generation: state.generation,
            member,
            sequence,
        })
    }

    pub(super) fn ofd_namespace(&self) -> Result<OfdNamespace, AdmissionError> {
        let state = self.lock()?;
        Ok(OfdNamespace {
            lineage: state.lineage,
            generation: state.generation,
            next_member: state.next_member,
            next: state.ofd_next.clone(),
        })
    }

    pub(super) fn exit(&self, event: LifecycleEvent) -> Result<(), AdmissionError> {
        let mut state = self.lock()?;
        if state.phase != Phase::Open || event.epoch != state.epoch || event.close != state.close {
            return Err(AdmissionError::Stale);
        }
        if state.lifecycles.get(&event.process) != Some(&event.role) {
            return Err(AdmissionError::Unauthorized);
        }
        state.lifecycles.remove(&event.process);
        state.publications.remove(&event.process);
        state.members.remove(&event.process);
        state.member_ordinals.remove(&event.process);
        Ok(())
    }

    pub(super) fn begin_restore<R: Reaper + 'static>(
        &self,
        close: CloseId,
        expected: HashSet<SavedProcessIdentity>,
        snapshot: super::model::ResourceSnapshot<D, SavedProcessIdentity>,
        ofd: OfdNamespace,
        reaper: R,
    ) -> Result<RestoreAdmission<'_, D>, AdmissionError> {
        let mut state = self.lock()?;
        validate_saved_topology(&expected)?;
        if snapshot.channels.keys().copied().collect::<HashSet<_>>() != expected {
            return Err(AdmissionError::Conflict);
        }
        if snapshot.channels.values().copied().collect::<HashSet<_>>().len() != expected.len() {
            return Err(AdmissionError::Conflict);
        }
        if state.phase != Phase::Open || close != state.close || !state.tickets.is_empty() || state.cleanup.is_some() {
            return Err(AdmissionError::Unauthorized);
        }
        let active_members = expected.iter().map(|identity| identity.member).collect::<HashSet<_>>();
        if ofd.lineage != state.lineage
            || ofd.generation != state.generation
            || !active_members.is_subset(&ofd.next.keys().copied().collect())
        {
            return Err(AdmissionError::Stale);
        }
        let highest_member = expected
            .iter()
            .map(|identity| identity.member)
            .max()
            .ok_or(AdmissionError::Conflict)?;
        if ofd.next_member <= highest_member || ofd.next_member < state.next_member {
            return Err(AdmissionError::Stale);
        }
        if state
            .ofd_next
            .iter()
            .any(|(member, high_water)| ofd.next.get(member).is_some_and(|restored| restored < high_water))
        {
            return Err(AdmissionError::Stale);
        }
        state.phase = Phase::Restoring;
        state.expected_restore = expected.clone();
        state.expected_snapshot = Some(snapshot);
        state.reserved_restore.clear();
        state.restore_live.clear();
        state.restored.clear();
        state.reservation_members = None;
        state.members.clear();
        state.publications.clear();
        state.lifecycles.clear();
        state.member_ordinals.clear();
        state.ofd_next = ofd.next;
        state.next_member = ofd.next_member;
        Ok(RestoreAdmission {
            authority: self,
            close,
            reaper: Some(Box::new(reaper)),
            settled: false,
        })
    }

    pub(super) fn reserve_restore(
        &self,
        ticket: TicketId,
        parent_role: ParentRole,
        child_role: ChildRole,
        parent: ProcessIdentity,
        expected: SavedProcessIdentity,
    ) -> Result<ForkAdmission, AdmissionError> {
        let mut state = self.lock()?;
        if state.phase != Phase::Restoring
            || !state.expected_restore.contains(&expected)
            || state
                .reservation_members
                .as_ref()
                .is_none_or(|members| !members.contains(&expected.member))
            || !state.reserved_restore.insert(expected)
        {
            return Err(AdmissionError::Closed);
        }
        if let Some(saved_parent) = expected.parent {
            let logical_parent = state
                .expected_restore
                .iter()
                .chain(state.restored.keys())
                .find(|identity| identity.key == saved_parent)
                .copied()
                .ok_or(AdmissionError::Conflict)?;
            if state.restored.get(&logical_parent) != Some(&parent) {
                return Err(Self::poison(&mut state, AdmissionError::Unauthorized));
            }
        }
        Self::insert(
            &mut state,
            ticket,
            parent_role,
            child_role,
            parent,
            Some(expected),
            true,
        )
    }
}
