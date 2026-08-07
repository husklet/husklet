use std::collections::BTreeSet;

mod event;

#[cfg(test)]
mod test;

use super::{ProcessGroup, Session, State, TaskRegistry};
use crate::{
    ChildClass, ChildClassSelector, ChildEvent, ChildEventKind, ChildSelector, ChildWaitOptions, ChildWaitResult,
    ExitStatus, ForegroundGroupEvent, PendingTarget, PreparedTerminalTransition, ProcessGroupId, ProcessId,
    ProcessLifecycle, SessionId, SignalDisposition, SignalInfo, SignalNumber, TaskError, TerminalTransition,
    TerminalTransitionEffects,
};

impl TaskRegistry {
    pub(super) fn allocate_session(state: &mut State, leader: ProcessId) -> Result<SessionId, TaskError> {
        let (slot, _) = leader.parts().ok_or(TaskError::InvalidProcess(leader))?;
        let entry = state.sessions.get_mut(slot).ok_or(TaskError::ProcessLimit)?;
        if entry.value.is_some() || entry.generation == u16::MAX {
            return Err(TaskError::ProcessLimit);
        }
        entry.generation += 1;
        Ok(SessionId::new(slot as u32, entry.generation))
    }

    pub(super) fn allocate_process_group(state: &mut State, leader: ProcessId) -> Result<ProcessGroupId, TaskError> {
        let (slot, _) = leader.parts().ok_or(TaskError::InvalidProcess(leader))?;
        let entry = state.process_groups.get_mut(slot).ok_or(TaskError::ProcessLimit)?;
        if entry.value.is_some() || entry.generation == u16::MAX {
            return Err(TaskError::ProcessLimit);
        }
        entry.generation += 1;
        Ok(ProcessGroupId::new(slot as u32, entry.generation))
    }

    pub fn session_id(&self, process: ProcessId) -> Result<SessionId, TaskError> {
        Ok(Self::process(&self.lock(), process)?.session)
    }

    /// Returns the process's controlling-terminal session association. A
    /// nonleader can disassociate without changing the session's terminal.
    pub fn terminal_session(&self, process: ProcessId) -> Result<Option<SessionId>, TaskError> {
        let state = self.lock();
        let process = Self::process(&state, process)?;
        Ok((!process.terminal_detached).then_some(process.session))
    }

    /// Records successful controlling-terminal acquisition for this process.
    pub fn attach_terminal(&self, process: ProcessId, session: SessionId) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        let process = Self::process_mut(&mut state, process)?;
        if process.session != session {
            return Err(TaskError::InvalidSession);
        }
        process.terminal_detached = false;
        Ok(())
    }

    pub fn process_group_id(&self, process: ProcessId) -> Result<ProcessGroupId, TaskError> {
        Ok(Self::process(&self.lock(), process)?.process_group)
    }

    pub fn create_session(&self, process: ProcessId) -> Result<SessionId, TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        let current_group = Self::process(&state, process)?.process_group;
        if Self::process_group(&state, current_group)?.leader == process {
            return Err(TaskError::SessionLeader);
        }
        let session = Self::allocate_session(&mut state, process)?;
        let group = Self::allocate_process_group(&mut state, process)?;
        Self::detach_group_member(&mut state, process)?;
        let mut groups = BTreeSet::new();
        groups.insert(group);
        Self::install_session(
            &mut state,
            session,
            Session {
                leader: process,
                process_groups: groups,
                foreground_group: None,
            },
        )?;
        let mut members = BTreeSet::new();
        members.insert(process);
        Self::install_process_group(
            &mut state,
            group,
            ProcessGroup {
                session,
                leader: process,
                members,
                orphaned: true,
            },
        )?;
        let process_state = Self::process_mut(&mut state, process)?;
        process_state.session = session;
        process_state.process_group = group;
        // setsid always starts a session without a controlling terminal.
        process_state.terminal_detached = true;
        let orphaned = Self::refresh_orphaned_groups(&mut state)?;
        drop(state);
        self.publish_orphaned(orphaned);
        Ok(session)
    }

    pub fn set_process_group(
        &self,
        caller: ProcessId,
        target: ProcessId,
        destination: Option<ProcessGroupId>,
    ) -> Result<ProcessGroupId, TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, caller)?;
        Self::ensure_process_unreserved(&state, target)?;
        Self::validate_group_change(&state, caller, target)?;
        let session = Self::process(&state, target)?.session;
        let group = if let Some(group) = destination {
            if Self::process_group(&state, group)?.session != session {
                return Err(TaskError::InvalidProcessGroup);
            }
            group
        } else {
            let group = Self::allocate_process_group(&mut state, target)?;
            Self::install_process_group(
                &mut state,
                group,
                ProcessGroup {
                    session,
                    leader: target,
                    members: BTreeSet::new(),
                    orphaned: false,
                },
            )?;
            Self::session_mut(&mut state, session)?.process_groups.insert(group);
            group
        };
        if Self::process(&state, target)?.process_group == group {
            return Ok(group);
        }
        Self::detach_group_member(&mut state, target)?;
        Self::process_group_mut(&mut state, group)?.members.insert(target);
        Self::process_mut(&mut state, target)?.process_group = group;
        let orphaned = Self::refresh_orphaned_groups(&mut state)?;
        drop(state);
        self.publish_orphaned(orphaned);
        Ok(group)
    }

    pub fn mark_exec(&self, process: ProcessId) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        let threads: Vec<_> = Self::process(&state, process)?.threads.iter().copied().collect();
        for thread in threads {
            let thread = Self::thread_mut(&mut state, thread)?;
            thread.robust_list = None;
            thread.clear_tid = None;
        }
        let process_state = Self::process_mut(&mut state, process)?;
        if !matches!(
            process_state.lifecycle,
            ProcessLifecycle::Running | ProcessLifecycle::Stopped
        ) {
            return Err(TaskError::InvalidLifecycle);
        }
        process_state.execed = true;
        drop(state);
        if let Err(error) = self.trace_stop(process, crate::TraceStop::Exec)
            && !matches!(error, crate::TraceError::InvalidLink(_))
        {
            return Err(TaskError::InvalidLifecycle);
        }
        Ok(())
    }

    pub fn set_child_class(&self, process: ProcessId, class: ChildClass) -> Result<(), TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, process)?;
        Self::process_mut(&mut state, process)?.child_class = class;
        Ok(())
    }

    pub fn set_foreground_group(
        &self,
        caller: ProcessId,
        group: ProcessGroupId,
    ) -> Result<ForegroundGroupEvent, TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, caller)?;
        let session = Self::process(&state, caller)?.session;
        if Self::process_group(&state, group)?.session != session {
            return Err(TaskError::InvalidProcessGroup);
        }
        Self::session_mut(&mut state, session)?.foreground_group = Some(group);
        Ok(ForegroundGroupEvent::new(session, group))
    }

    /// Validates one controlling-terminal transition and captures the
    /// generation-qualified foreground recipients without publishing signals.
    pub fn prepare_terminal_transition(
        &self,
        caller: ProcessId,
        transition: TerminalTransition,
    ) -> Result<PreparedTerminalTransition<'_>, TaskError> {
        let state = self.lock();
        Self::ensure_process_unreserved(&state, caller)?;
        let session = Self::process(&state, caller)?.session;
        let session_state = Self::session(&state, session)?;
        if transition == TerminalTransition::SessionLeaderExit && session_state.leader != caller {
            return Err(TaskError::InvalidSession);
        }
        if Self::process(&state, caller)?.terminal_detached {
            return Err(TaskError::InvalidSession);
        }
        let session_wide = session_state.leader == caller;
        let foreground = session_state.foreground_group;
        let members: Vec<ProcessId> = foreground
            .map(|group| Self::process_group(&state, group).map(|group| group.members.iter().copied().collect()))
            .transpose()?
            .unwrap_or_default();
        let signals = match transition {
            TerminalTransition::Detach if session_wide => [SignalNumber::new(1).ok(), Some(SignalNumber::CONTINUE)],
            TerminalTransition::SessionLeaderExit => [SignalNumber::new(1).ok(), None],
            TerminalTransition::Detach => [None, None],
        };
        Ok(PreparedTerminalTransition {
            registry: self,
            members,
            caller,
            effects: TerminalTransitionEffects {
                session,
                foreground,
                signals,
                session_wide,
            },
        })
    }

    /// Publishes SIGWINCH to the generation-qualified foreground group stored
    /// by the tty, independent of which process issued TIOCSWINSZ.
    pub fn terminal_window_changed(
        &self,
        session_number: u32,
        foreground: ProcessGroupId,
    ) -> Result<TerminalTransitionEffects, TaskError> {
        let state = self.lock();
        let group = Self::process_group(&state, foreground)?;
        let session = group.session;
        if session.number() != session_number {
            return Err(TaskError::InvalidProcessGroup);
        }
        Self::session(&state, session)?;
        let members = group.members.iter().copied().collect::<Vec<_>>();
        drop(state);
        let signal = SignalNumber::new(28).map_err(|_| TaskError::InvalidLifecycle)?;
        for process in &members {
            let _ = self.enqueue_signal(PendingTarget::Process(*process), SignalInfo::bare(signal));
        }
        Ok(TerminalTransitionEffects {
            session,
            foreground: Some(foreground),
            signals: [Some(signal), None],
            session_wide: false,
        })
    }

    pub fn wait_child(
        &self,
        parent: ProcessId,
        selector: ChildSelector,
        options: ChildWaitOptions,
    ) -> Result<ChildWaitResult, TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, parent)?;
        let eligible = Self::eligible_children(&state, parent, selector, options.class)?;
        if eligible.is_empty() {
            return Err(TaskError::NoChildren);
        }
        let position = state
            .child_events
            .iter()
            .position(|event| eligible.contains(&event.child) && Self::reports_kind(event.kind, options));
        let Some(position) = position else {
            return Ok(if options.no_hang {
                ChildWaitResult::NoChange
            } else {
                ChildWaitResult::WouldBlock
            });
        };
        let event = state.child_events[position];
        if !options.keep_waitable {
            state.child_events.remove(position);
        }
        Ok(ChildWaitResult::Event(event))
    }

    pub fn prepare_wait_child(
        &self,
        parent: ProcessId,
        selector: ChildSelector,
        options: ChildWaitOptions,
    ) -> Result<crate::PreparedChildWait<'_>, TaskError> {
        let mut state = self.lock();
        Self::ensure_process_unreserved(&state, parent)?;
        let eligible = Self::eligible_children(&state, parent, selector, options.class)?;
        if eligible.is_empty() {
            return Err(TaskError::NoChildren);
        }
        let event = state
            .child_events
            .iter()
            .find(|event| {
                !state.wait_reservations.contains(&event.sequence)
                    && eligible.contains(&event.child)
                    && Self::reports_kind(event.kind, options)
            })
            .copied();
        if let Some(event) = event {
            state.wait_reservations.insert(event.sequence);
        }
        Ok(match event {
            Some(event) => crate::PreparedChildWait::Selection(crate::PreparedWaitSelection {
                registry: self,
                parent,
                event,
                keep_waitable: options.keep_waitable,
                sequence: event.sequence,
                finished: false,
            }),
            None if options.no_hang => crate::PreparedChildWait::NoChange,
            None => crate::PreparedChildWait::WouldBlock,
        })
    }

    pub fn wait_observation(&self) -> u64 {
        self.lock().wait_epoch
    }

    pub fn wait_change(&self, observed: u64) {
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.wait_epoch == observed {
            state = self
                .child_ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub fn prepare_wait(
        &self,
        parent: ProcessId,
        selector: ChildSelector,
        options: ChildWaitOptions,
    ) -> Result<crate::PreparedChildWait<'_>, TaskError> {
        let _admission = self.activity.admit();
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        loop {
            Self::ensure_process_unreserved(&state, parent)?;
            let eligible = Self::eligible_children(&state, parent, selector, options.class)?;
            if eligible.is_empty() {
                return Err(TaskError::NoChildren);
            }
            let event = state
                .child_events
                .iter()
                .find(|event| {
                    !state.wait_reservations.contains(&event.sequence)
                        && eligible.contains(&event.child)
                        && Self::reports_kind(event.kind, options)
                })
                .copied();
            if let Some(event) = event {
                state.wait_reservations.insert(event.sequence);
                drop(state);
                return Ok(crate::PreparedChildWait::Selection(crate::PreparedWaitSelection {
                    registry: self,
                    parent,
                    event,
                    keep_waitable: options.keep_waitable,
                    sequence: event.sequence,
                    finished: false,
                }));
            }
            if options.no_hang {
                return Ok(crate::PreparedChildWait::NoChange);
            }
            state = self
                .child_ready
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    pub(crate) fn commit_wait_selection(
        &self,
        parent: ProcessId,
        selected: ChildEvent,
        keep_waitable: bool,
        sequence: u64,
    ) -> Result<ChildEvent, TaskError> {
        let mut state = self.lock();
        if !state.wait_reservations.remove(&sequence) {
            return Err(TaskError::InvalidPlan);
        }
        Self::process(&state, parent)?;
        let position = state
            .child_events
            .iter()
            .position(|event| *event == selected && event.parent == parent)
            .ok_or(TaskError::InvalidPlan)?;
        if keep_waitable {
            return Ok(selected);
        }
        state.child_events.remove(position);
        if let ChildEventKind::Exited(status) = selected.kind {
            let child = selected.child;
            let child_state = Self::process(&state, child)?;
            if child_state.parent != Some(parent)
                || child_state.lifecycle != ProcessLifecycle::Zombie
                || !child_state.children.is_empty()
                || child_state.exit_status != Some(status)
            {
                return Err(TaskError::InvalidPlan);
            }
            let child_usage = child_state
                .cpu_account
                .nanoseconds()
                .saturating_add(child_state.cpu_usage.children_nanoseconds);
            let parent_usage = &mut Self::process_mut(&mut state, parent)?.cpu_usage.children_nanoseconds;
            *parent_usage = parent_usage.saturating_add(child_usage);
            Self::process_mut(&mut state, parent)?.children.remove(&child);
            state
                .waits
                .retain(|event| !(event.parent == parent && event.child == child));
            Self::detach_group_member(&mut state, child)?;
            Self::release_process(&mut state, child)?;
        }
        Ok(selected)
    }

    pub(crate) fn release_wait_reservation(&self, sequence: u64) {
        self.lock().wait_reservations.remove(&sequence);
    }

    pub(super) fn record_child_transition(
        state: &mut State,
        child: ProcessId,
        kind: ChildEventKind,
        max_pending: usize,
    ) -> Result<(), TaskError> {
        let process = Self::process(state, child)?;
        let Some(parent) = process.parent else {
            return Ok(());
        };
        let event = ChildEvent {
            parent,
            child,
            process_group: process.process_group,
            class: process.child_class,
            kind,
            sequence: state.next_wait_sequence,
        };
        state.next_wait_sequence = state.next_wait_sequence.wrapping_add(1).max(1);
        state.wait_epoch = state.wait_epoch.wrapping_add(1).max(1);
        state.child_events.push_back(event);
        Self::queue_child_signal(state, parent, child, kind, max_pending)?;
        Ok(())
    }

    pub(super) fn queue_child_signal(
        state: &mut State,
        parent: ProcessId,
        child: ProcessId,
        kind: ChildEventKind,
        max_pending: usize,
    ) -> Result<(), TaskError> {
        const SA_NOCLDSTOP: u64 = 1;
        let signal = SignalNumber::new(17).map_err(|_| TaskError::InvalidLifecycle)?;
        let action = Self::process(state, parent)?.signals.actions[16];
        if matches!(
            action.disposition,
            SignalDisposition::Default | SignalDisposition::Ignore
        ) {
            return Ok(());
        }
        if action.flags & SA_NOCLDSTOP != 0 && matches!(kind, ChildEventKind::Stopped(_) | ChildEventKind::Continued) {
            return Ok(());
        }
        let child_state = Self::process(state, child)?;
        let (code, status) = match kind {
            ChildEventKind::Exited(ExitStatus::Code(status)) => (1, u64::from(status)),
            ChildEventKind::Exited(ExitStatus::Signal {
                signal,
                dumped_core: false,
            }) => (2, u64::from(signal)),
            ChildEventKind::Exited(ExitStatus::Signal {
                signal,
                dumped_core: true,
            }) => (3, u64::from(signal)),
            ChildEventKind::Stopped(signal) => (5, u64::from(signal.get())),
            ChildEventKind::Continued => (6, 18),
        };
        let info = SignalInfo {
            signal,
            code,
            sender_process: child.number(),
            sender_user: child_state.credentials.real_user,
            value: status,
            ..SignalInfo::bare(signal)
        };
        let _ = Self::process_mut(state, parent)?
            .signals
            .pending
            .enqueue(info, max_pending);
        Ok(())
    }

    pub(super) fn detach_group_member(state: &mut State, process: ProcessId) -> Result<(), TaskError> {
        let group = Self::process(state, process)?.process_group;
        let session = Self::process_group(state, group)?.session;
        Self::process_group_mut(state, group)?.members.remove(&process);
        if !Self::process_group(state, group)?.members.is_empty() {
            return Ok(());
        }
        Self::session_mut(state, session)?.process_groups.remove(&group);
        if Self::session(state, session)?.foreground_group == Some(group) {
            Self::session_mut(state, session)?.foreground_group = None;
        }
        Self::release_process_group(state, group)?;
        if Self::session(state, session)?.process_groups.is_empty() {
            Self::release_session(state, session)?;
        }
        Ok(())
    }

    fn validate_group_change(state: &State, caller: ProcessId, target: ProcessId) -> Result<(), TaskError> {
        let caller_state = Self::process(state, caller)?;
        let target_state = Self::process(state, target)?;
        if !matches!(
            target_state.lifecycle,
            ProcessLifecycle::Running | ProcessLifecycle::Stopped
        ) {
            return Err(TaskError::InvalidLifecycle);
        }
        if caller != target && target_state.parent != Some(caller) {
            return Err(TaskError::WrongProcess);
        }
        if caller != target && target_state.execed {
            return Err(TaskError::ProcessExeced);
        }
        if Self::session(state, target_state.session)?.leader == target {
            return Err(TaskError::SessionLeader);
        }
        if caller_state.session != target_state.session {
            return Err(TaskError::InvalidSession);
        }
        Ok(())
    }

    fn eligible_children(
        state: &State,
        parent: ProcessId,
        selector: ChildSelector,
        class: ChildClassSelector,
    ) -> Result<BTreeSet<ProcessId>, TaskError> {
        let parent_state = Self::process(state, parent)?;
        let parent_group = parent_state.process_group;
        Ok(parent_state
            .children
            .iter()
            .copied()
            .filter(|child| {
                let Ok(child_state) = Self::process(state, *child) else {
                    return false;
                };
                let selected = match selector {
                    ChildSelector::Any => true,
                    ChildSelector::Process(process) => *child == process,
                    ChildSelector::ProcessGroup(group) => child_state.process_group == group,
                    ChildSelector::SameProcessGroup => child_state.process_group == parent_group,
                };
                selected
                    && match class {
                        ChildClassSelector::Standard => child_state.child_class == ChildClass::Standard,
                        ChildClassSelector::Clone => child_state.child_class == ChildClass::Clone,
                        ChildClassSelector::All => true,
                    }
            })
            .collect())
    }

    fn reports_kind(kind: ChildEventKind, options: ChildWaitOptions) -> bool {
        match kind {
            ChildEventKind::Exited(_) => true,
            ChildEventKind::Stopped(_) => options.report_stopped,
            ChildEventKind::Continued => options.report_continued,
        }
    }
}

impl PreparedTerminalTransition<'_> {
    /// Replaces the task-session foreground snapshot with the tty's own
    /// generation-qualified foreground identity.
    pub fn target_foreground(mut self, foreground: Option<ProcessGroupId>) -> Self {
        let state = self.registry.lock();
        let target = foreground.and_then(|identity| {
            let group = TaskRegistry::process_group(&state, identity).ok()?;
            (group.session == self.effects.session).then_some((identity, group))
        });
        self.members = target
            .map(|(_, group)| group.members.iter().copied().collect())
            .unwrap_or_default();
        self.effects.foreground = target.map(|(identity, _)| identity);
        self
    }

    /// Publishes captured signals in Linux order after the terminal mutation.
    #[must_use]
    pub fn commit(self) -> TerminalTransitionEffects {
        let mut state = self.registry.lock();
        if self.effects.session_wide {
            for process in state
                .processes
                .iter_mut()
                .filter_map(|entry| entry.value.as_mut())
                .filter(|process| process.session == self.effects.session)
            {
                process.terminal_detached = true;
            }
        } else if let Ok(process) = TaskRegistry::process_mut(&mut state, self.caller)
            && process.session == self.effects.session
        {
            process.terminal_detached = true;
        }
        drop(state);
        for signal in self.effects.signals.into_iter().flatten() {
            for process in &self.members {
                let _ = self
                    .registry
                    .enqueue_signal(PendingTarget::Process(*process), SignalInfo::bare(signal));
            }
        }
        self.effects
    }
}
