use hl_descriptor::OperationLease;
use hl_linux::{Errno, LinuxResult, RestartKind};
use hl_task::{PendingTarget, SignalDisposition, SignalInfo, SignalNumber};

use super::RuntimeFilesystemSyscalls;

#[derive(Clone, Copy)]
pub(super) enum TerminalAccess {
    Read,
    Write,
    Control,
}

impl<M: hl_linux::GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn terminal_access(
        &self,
        lease: &OperationLease,
        access: TerminalAccess,
    ) -> Option<LinuxResult> {
        let bindings = self.terminals.as_ref()?;
        let terminal = bindings.get(lease.description_identity())?;
        if terminal.endpoint != hl_terminal::Endpoint::Slave {
            return None;
        }
        let (tasks, configured_process) = self.terminal_tasks.as_ref()?;
        let actor = self.actor?;
        let process = hl_task::ProcessId::from_wire(actor.process, actor.process_generation)?;
        let thread = hl_task::ThreadId::from_wire(actor.thread, actor.thread_generation)?;
        if process != *configured_process {
            return Some(LinuxResult::Error(Errno::EIO));
        }
        let snapshot = tasks.snapshot();
        let process_state = snapshot.processes.iter().find(|candidate| candidate.id == process)?;
        let controlling = terminal.controlling_session()?;
        if process_state.session.number() != controlling {
            return None;
        }
        let foreground = terminal.pair.foreground()?;
        if process_state.process_group.number() == foreground.number
            && process_state.process_group.wire_parts().1 == foreground.generation
        {
            return None;
        }
        if matches!(access, TerminalAccess::Write)
            && !terminal.pair.settings().local.contains(hl_terminal::Local::TO_STOP)
        {
            return None;
        }
        let signal = SignalNumber::new(if matches!(access, TerminalAccess::Read) { 21 } else { 22 }).ok()?;
        let group = snapshot
            .process_groups
            .iter()
            .find(|group| group.id == process_state.process_group)?;
        let thread_state = snapshot.threads.iter().find(|candidate| candidate.id == thread)?;
        let action = process_state
            .signals
            .actions
            .iter()
            .find_map(|(number, action)| (*number == signal).then_some(*action))
            .unwrap_or(hl_task::SignalAction::DEFAULT);
        if group.orphaned
            || thread_state.signals.mask.contains(signal)
            || action.disposition == SignalDisposition::Ignore
        {
            return Some(LinuxResult::Error(Errno::EIO));
        }
        for member in &group.members {
            let _ = tasks.enqueue_signal(PendingTarget::Process(*member), SignalInfo::bare(signal));
        }
        Some(LinuxResult::Restart(RestartKind::NoInterrupt))
    }
}
