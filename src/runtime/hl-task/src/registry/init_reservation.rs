use std::collections::BTreeSet;
use std::sync::Arc;

use crate::signal::{SignalProcessState, SignalThreadState};
use crate::{
    ChildClass, ProcessCredentials, ProcessGroupId, ProcessId, ProcessLifecycle, ProcessLimits, SessionId, TaskError,
    ThreadId, ThreadLifecycle,
};

use super::{Process, ProcessGroup, Session, TaskRegistry, Thread, activity};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct InitSlots {
    pub(super) process: ProcessId,
    pub(super) thread: ThreadId,
    pub(super) session: SessionId,
    pub(super) process_group: ProcessGroupId,
}

/// An unpublished reservation for the initial process and its identity graph.
///
/// Dropping the reservation aborts it. Reserved identities consume their
/// generations, but no process, thread, session, group, or namespace mutation
/// becomes guest-visible before [`InitReservation::commit`].
#[must_use = "the initial process reservation must be committed or dropped"]
pub struct InitReservation<'registry> {
    pub(super) registry: &'registry TaskRegistry,
    pub(super) admission: Option<activity::ActivityAdmission>,
    pub(super) slots: InitSlots,
    pub(super) credentials: Option<ProcessCredentials>,
    pub(super) limits: Option<ProcessLimits>,
    pub(super) finished: bool,
}

impl InitReservation<'_> {
    pub fn commit(mut self) -> Result<(ProcessId, ThreadId), TaskError> {
        let credentials = self.credentials.take().ok_or(TaskError::InvalidPlan)?;
        let limits = self.limits.take().ok_or(TaskError::InvalidPlan)?;
        let result = self.registry.commit_init(self.slots, credentials, limits);
        self.finished = result.is_ok();
        if self.finished {
            drop(self.admission.take());
        }
        result
    }
}

impl Drop for InitReservation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.abort_init(self.slots);
        }
        drop(self.admission.take());
    }
}

impl TaskRegistry {
    pub fn create_init(
        &self,
        credentials: ProcessCredentials,
        limits: ProcessLimits,
    ) -> Result<(ProcessId, ThreadId), TaskError> {
        self.begin_create_init(credentials, limits)?.commit()
    }

    pub fn begin_create_init(
        &self,
        credentials: ProcessCredentials,
        limits: ProcessLimits,
    ) -> Result<InitReservation<'_>, TaskError> {
        if credentials.supplementary_groups().len() > self.max_groups {
            return Err(TaskError::GroupLimit);
        }
        let admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.init.is_some() || state.init_reservation.is_some() {
            return Err(TaskError::InvalidLifecycle);
        }
        // Slot zero is held back for the entrypoint init forks, so a container's
        // first guest process is pid 1 the way Linux and Docker present it.
        let (process, thread) = match Self::allocate_leader_from(&mut state, 1) {
            Ok(leader) => leader,
            Err(_) => Self::allocate_leader(&mut state)?,
        };
        let session = Self::allocate_session(&mut state, process)?;
        let process_group = Self::allocate_process_group(&mut state, process)?;
        let slots = InitSlots {
            process,
            thread,
            session,
            process_group,
        };
        state.init_reservation = Some(slots);
        Ok(InitReservation {
            registry: self,
            admission: Some(admission),
            slots,
            credentials: Some(credentials),
            limits: Some(limits),
            finished: false,
        })
    }

    pub(super) fn commit_init(
        &self,
        slots: InitSlots,
        credentials: ProcessCredentials,
        limits: ProcessLimits,
    ) -> Result<(ProcessId, ThreadId), TaskError> {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.init.is_some() || state.init_reservation != Some(slots) {
            return Err(TaskError::InvalidPlan);
        }
        let (process_slot, process_generation) = slots.process.parts().ok_or(TaskError::InvalidPlan)?;
        let (thread_slot, thread_generation) = slots.thread.parts().ok_or(TaskError::InvalidPlan)?;
        let (session_slot, session_generation) = slots.session.parts().ok_or(TaskError::InvalidPlan)?;
        let (group_slot, group_generation) = slots.process_group.parts().ok_or(TaskError::InvalidPlan)?;
        if state
            .processes
            .get(process_slot)
            .is_none_or(|entry| entry.generation != process_generation || entry.value.is_some())
            || state
                .threads
                .get(thread_slot)
                .is_none_or(|entry| entry.generation != thread_generation || entry.value.is_some())
            || state
                .sessions
                .get(session_slot)
                .is_none_or(|entry| entry.generation != session_generation || entry.value.is_some())
            || state
                .process_groups
                .get(group_slot)
                .is_none_or(|entry| entry.generation != group_generation || entry.value.is_some())
        {
            return Err(TaskError::InvalidPlan);
        }
        let initial_user = crate::NamespaceSet::initial().user;
        if !state.user_namespaces.contains_key(&initial_user) {
            return Err(TaskError::InvalidSnapshot);
        }
        let process = slots.process;
        let thread = slots.thread;
        let session = slots.session;
        let process_group = slots.process_group;
        let initial_user_owner = credentials.effective_user;
        Self::install_thread(
            &mut state,
            thread,
            Thread {
                process,
                lifecycle: ThreadLifecycle::Runnable,
                cancellation_pending: false,
                signal_pending: false,
                pending_transaction: None,
                signals: SignalThreadState::new(),
                robust_list: None,
                clear_tid: None,
                name: *b"hl-engine\0\0\0\0\0\0\0",
                affinity: None,
                schedule: crate::SchedulingProfile::OTHER,
            },
        )?;
        let mut threads = BTreeSet::new();
        threads.insert(thread);
        Self::install_process(
            &mut state,
            process,
            Process {
                control_epoch: 0,
                lifecycle: ProcessLifecycle::Running,
                parent: None,
                children: BTreeSet::new(),
                threads,
                leader: thread,
                session,
                process_group,
                terminal_detached: false,
                child_class: ChildClass::Standard,
                execed: false,
                arguments: Vec::new(),
                name: *b"hl-engine\0\0\0\0\0\0\0",
                credentials,
                limits,
                exit_status: None,
                pending_transaction: None,
                signals: SignalProcessState::new(),
                namespaces: crate::NamespaceSet::initial(),
                parent_death_signal: 0,
                child_subreaper: false,
                cpu_usage: crate::CpuUsage::default(),
                cpu_account: Arc::new(crate::CpuAccount::default()),
                dumpable: true,
                oom_score_adj: 0,
                timer_slack: 50_000,
                thp_disabled: false,
                mce_policy: 2,
                personality: 0,
            },
        )?;
        let mut groups = BTreeSet::new();
        groups.insert(process_group);
        Self::install_session(
            &mut state,
            session,
            Session {
                leader: process,
                process_groups: groups,
                foreground_group: Some(process_group),
            },
        )?;
        let mut members = BTreeSet::new();
        members.insert(process);
        Self::install_process_group(
            &mut state,
            process_group,
            ProcessGroup {
                session,
                leader: process,
                members,
                orphaned: true,
            },
        )?;
        state
            .user_namespaces
            .get_mut(&initial_user)
            .expect("initial user namespace validated before publication")
            .owner = initial_user_owner;
        state.init = Some(process);
        state.init_reservation = None;
        Ok((process, thread))
    }

    fn abort_init(&self, slots: InitSlots) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.init_reservation == Some(slots) {
            state.init_reservation = None;
        }
    }
}
