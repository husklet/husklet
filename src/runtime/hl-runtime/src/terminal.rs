use std::sync::Arc;

use hl_descriptor::OperationActor;
use hl_task::{PendingTarget, ProcessGroupId, SignalInfo, SignalNumber, TaskRegistry};
use hl_terminal::{ForegroundGroup, PairId, Signal, SignalSink};

/// Delivers line-discipline signals through task-owned process-group state.
pub struct TerminalSignals {
    tasks: Arc<TaskRegistry>,
}

impl TerminalSignals {
    #[must_use]
    pub fn new(tasks: Arc<TaskRegistry>) -> Self {
        Self { tasks }
    }
}

impl SignalSink for TerminalSignals {
    fn publish(
        &self,
        actor: Option<OperationActor>,
        _terminal: PairId,
        foreground: Option<ForegroundGroup>,
        signal: Signal,
    ) {
        let Some(actor) = actor else { return };
        let Some(process) = hl_task::ProcessId::from_wire(actor.process, actor.process_generation) else {
            return;
        };
        let snapshot = self.tasks.snapshot();
        let Some(source) = snapshot.processes.iter().find(|entry| entry.id == process) else {
            return;
        };
        let group = match foreground {
            Some(group) => {
                let Some(slot) = group.number.checked_sub(1) else {
                    return;
                };
                let Some(group) = ProcessGroupId::from_wire(slot, group.generation) else {
                    return;
                };
                group
            }
            None => source.process_group,
        };
        let number = match signal {
            Signal::Interrupt => 2,
            Signal::Quit => 3,
            Signal::Suspend => 20,
        };
        let Ok(number) = SignalNumber::new(number) else { return };
        for process in snapshot
            .processes
            .iter()
            .filter(|process| process.process_group == group)
        {
            let _ = self
                .tasks
                .enqueue_signal(PendingTarget::Process(process.id), SignalInfo::bare(number));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig};

    #[test]
    fn rejects_stale_foreground() {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        let group = tasks.process_group_id(process).unwrap();
        let (_, generation) = group.wire_parts();
        let (process_slot, process_generation) = process.wire_parts();
        let (thread_slot, thread_generation) = thread.wire_parts();
        TerminalSignals::new(Arc::clone(&tasks)).publish(
            Some(OperationActor {
                process: process_slot,
                process_generation,
                thread: thread_slot,
                thread_generation,
            }),
            PairId {
                index: 1,
                generation: 1,
            },
            Some(ForegroundGroup {
                number: group.number(),
                generation: generation.wrapping_add(1).max(1),
            }),
            Signal::Interrupt,
        );
        assert!(tasks.snapshot().processes[0].signals.pending.is_empty());
    }
}
