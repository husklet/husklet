//! Child-wait selection, reservation, and child-event queueing.

use std::collections::BTreeSet;

use super::super::{State, TaskRegistry};
use crate::{
    ChildClass, ChildClassSelector, ChildEvent, ChildEventKind, ChildSelector, ChildWaitOptions, ChildWaitResult,
    ExitStatus, ProcessId, ProcessLifecycle, SignalDisposition, SignalInfo, SignalNumber, TaskError,
};

impl TaskRegistry {
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

    pub(in crate::registry) fn record_child_transition(
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

    pub(in crate::registry) fn queue_child_signal(
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
