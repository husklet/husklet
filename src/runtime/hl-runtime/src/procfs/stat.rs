use hl_task::{ProcessId, ProcessLifecycle};
use hl_vfs::{ProcfsError, ProcfsStatInput, ProcfsStatState, ProcfsStatView};

use super::TaskProcfs;

/// One coherent sample of process metrics owned outside the task registry.
///
/// Implementations join the scheduler, process clock, memory ledger, loaded
/// image, and controlling-terminal domains. A producer must return an error
/// when any field is unavailable; `/proc/<pid>/stat` must not mix real task
/// identity with invented metric values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatMetrics {
    pub terminal: i32,
    pub flags: u32,
    pub minor_faults: u64,
    pub child_minor_faults: u64,
    pub major_faults: u64,
    pub child_major_faults: u64,
    pub user_ticks: u64,
    pub system_ticks: u64,
    pub child_user_ticks: i64,
    pub child_system_ticks: i64,
    pub priority: i64,
    pub nice: i64,
    pub interval_ticks: i64,
    pub start_ticks: u64,
    pub virtual_bytes: u64,
    pub resident_pages: i64,
    pub resident_limit: u64,
    pub code_start: u64,
    pub code_end: u64,
    pub stack_start: u64,
    pub stack_pointer: u64,
    pub instruction_pointer: u64,
    pub wait_channel: u64,
    pub swapped_pages: u64,
    pub child_swapped_pages: u64,
    pub exit_signal: i32,
    pub processor: i32,
    pub realtime_priority: u32,
    pub policy: u32,
    pub delay_ticks: u64,
    pub guest_ticks: u64,
    pub child_guest_ticks: i64,
    pub data_start: u64,
    pub data_end: u64,
    pub heap_start: u64,
    pub arguments_start: u64,
    pub arguments_end: u64,
    pub environment_start: u64,
    pub environment_end: u64,
}

/// Consumer-owned capability for producing non-task process-stat metrics.
pub trait StatPort: Send + Sync {
    fn sample(&self, process: ProcessId) -> Result<StatMetrics, ProcfsError>;
}

impl TaskProcfs {
    pub(super) fn stat_view(&self, process: ProcessId) -> Result<ProcfsStatView, ProcfsError> {
        let registry = self.tasks.snapshot();
        let process = registry
            .processes
            .iter()
            .find(|candidate| candidate.id == process)
            .ok_or(ProcfsError::NotFound)?;
        let leader = registry.threads.iter().find(|thread| thread.id == process.leader);
        if leader.is_none() && process.lifecycle != ProcessLifecycle::Zombie {
            return Err(ProcfsError::Invalid);
        }
        let session = registry
            .sessions
            .iter()
            .find(|session| session.id == process.session)
            .ok_or(ProcfsError::Invalid)?;
        let metrics = self.stat.as_ref().ok_or(ProcfsError::NotFound)?.sample(process.id)?;
        let pending = process
            .signals
            .pending
            .iter()
            .chain(leader.into_iter().flat_map(|leader| &leader.signals.pending))
            .fold(0_u64, |mask, signal| mask | (1_u64 << (signal.signal.get() - 1)));
        let (ignored, caught) = process.signals.actions.iter().fold(
            (0_u64, 0_u64),
            |(ignored, caught), (signal, action): &(hl_task::SignalNumber, hl_task::SignalAction)| {
                let bit = 1_u64 << (signal.get() - 1);
                match action.disposition {
                    hl_task::SignalDisposition::Default => (ignored, caught),
                    hl_task::SignalDisposition::Ignore => (ignored | bit, caught),
                    hl_task::SignalDisposition::Handler(_) => (ignored, caught | bit),
                }
            },
        );
        let name = process.name.split(|byte| *byte == 0).next().unwrap_or(&[]).to_vec();
        let state = match process.lifecycle {
            ProcessLifecycle::Zombie => ProcfsStatState::Zombie,
            ProcessLifecycle::Stopped => ProcfsStatState::Stopped,
            ProcessLifecycle::Exiting => ProcfsStatState::Dead,
            ProcessLifecycle::Starting | ProcessLifecycle::Running => {
                match leader.ok_or(ProcfsError::Invalid)?.lifecycle {
                    hl_task::ThreadLifecycle::Blocked => ProcfsStatState::Sleeping,
                    hl_task::ThreadLifecycle::Exiting => ProcfsStatState::Dead,
                    hl_task::ThreadLifecycle::Starting | hl_task::ThreadLifecycle::Runnable => ProcfsStatState::Running,
                }
            }
        };
        let group = i32::try_from(process.process_group.number()).map_err(|_| ProcfsError::Invalid)?;
        let session_id = i32::try_from(process.session.number()).map_err(|_| ProcfsError::Invalid)?;
        let foreground_group = session.foreground_group.map_or(Ok(-1), |group| {
            i32::try_from(group.number()).map_err(|_| ProcfsError::Invalid)
        })?;
        let threads = i64::try_from(process.threads.len().max(1)).map_err(|_| ProcfsError::Invalid)?;
        let exit_code = process.exit_status.map_or(Ok(0), |status| {
            i32::try_from(status.wait_status()).map_err(|_| ProcfsError::Invalid)
        })?;
        ProcfsStatView::new(ProcfsStatInput {
            process: process.id.number(),
            name,
            state,
            parent: process.parent.map_or(0, ProcessId::number),
            group,
            session: session_id,
            terminal: metrics.terminal,
            foreground_group,
            flags: metrics.flags,
            minor_faults: metrics.minor_faults,
            child_minor_faults: metrics.child_minor_faults,
            major_faults: metrics.major_faults,
            child_major_faults: metrics.child_major_faults,
            user_ticks: metrics.user_ticks,
            system_ticks: metrics.system_ticks,
            child_user_ticks: metrics.child_user_ticks,
            child_system_ticks: metrics.child_system_ticks,
            priority: metrics.priority,
            nice: metrics.nice,
            threads,
            interval_ticks: metrics.interval_ticks,
            start_ticks: metrics.start_ticks,
            virtual_bytes: metrics.virtual_bytes,
            resident_pages: metrics.resident_pages,
            resident_limit: metrics.resident_limit,
            code_start: metrics.code_start,
            code_end: metrics.code_end,
            stack_start: metrics.stack_start,
            stack_pointer: metrics.stack_pointer,
            instruction_pointer: metrics.instruction_pointer,
            pending_signals: pending,
            blocked_signals: leader.map_or(0, |leader| leader.signals.mask.bits()),
            ignored_signals: ignored,
            caught_signals: caught,
            wait_channel: metrics.wait_channel,
            swapped_pages: metrics.swapped_pages,
            child_swapped_pages: metrics.child_swapped_pages,
            exit_signal: metrics.exit_signal,
            processor: metrics.processor,
            realtime_priority: metrics.realtime_priority,
            policy: metrics.policy,
            delay_ticks: metrics.delay_ticks,
            guest_ticks: metrics.guest_ticks,
            child_guest_ticks: metrics.child_guest_ticks,
            data_start: metrics.data_start,
            data_end: metrics.data_end,
            heap_start: metrics.heap_start,
            arguments_start: metrics.arguments_start,
            arguments_end: metrics.arguments_end,
            environment_start: metrics.environment_start,
            environment_end: metrics.environment_end,
            exit_code,
        })
        .map_err(|_| ProcfsError::Invalid)
    }
}
