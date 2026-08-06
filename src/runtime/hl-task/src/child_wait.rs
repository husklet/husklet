//! Child selection, wait options, and the prepared-wait reservation.
use crate::{CpuUsage, ExitStatus, ProcessGroupId, ProcessId, SignalNumber, TaskError};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitSelector {
    Any,
    Process(ProcessId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildClass {
    Standard,
    Clone,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildClassSelector {
    Standard,
    Clone,
    All,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildSelector {
    Any,
    Process(ProcessId),
    ProcessGroup(ProcessGroupId),
    SameProcessGroup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildWaitOptions {
    pub no_hang: bool,
    pub report_stopped: bool,
    pub report_continued: bool,
    pub keep_waitable: bool,
    pub class: ChildClassSelector,
}

impl Default for ChildWaitOptions {
    fn default() -> Self {
        Self {
            no_hang: false,
            report_stopped: false,
            report_continued: false,
            keep_waitable: false,
            class: ChildClassSelector::Standard,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildEventKind {
    Exited(ExitStatus),
    Stopped(SignalNumber),
    Continued,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChildEvent {
    pub parent: ProcessId,
    pub child: ProcessId,
    pub process_group: ProcessGroupId,
    pub class: ChildClass,
    pub kind: ChildEventKind,
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChildWaitResult {
    Event(ChildEvent),
    NoChange,
    WouldBlock,
}

#[must_use = "prepared wait selection must be committed or aborted"]
pub struct PreparedWaitSelection<'registry> {
    pub(crate) registry: &'registry crate::TaskRegistry,
    pub(crate) parent: ProcessId,
    pub(crate) event: ChildEvent,
    pub(crate) keep_waitable: bool,
    pub(crate) sequence: u64,
    pub(crate) finished: bool,
}

impl PreparedWaitSelection<'_> {
    #[must_use]
    pub const fn event(&self) -> ChildEvent {
        self.event
    }

    pub fn usage(&self) -> Result<CpuUsage, TaskError> {
        self.registry.cpu_usage(self.event.child)
    }

    pub fn commit(mut self) -> Result<ChildEvent, TaskError> {
        let result = self
            .registry
            .commit_wait_selection(self.parent, self.event, self.keep_waitable, self.sequence);
        self.finished = true;
        result
    }
}

impl Drop for PreparedWaitSelection<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.release_wait_reservation(self.sequence);
        }
    }
}

pub enum PreparedChildWait<'registry> {
    Selection(PreparedWaitSelection<'registry>),
    NoChange,
    WouldBlock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitEvent {
    pub parent: ProcessId,
    pub child: ProcessId,
    pub status: ExitStatus,
    pub sequence: u64,
}
