//! Signal delivery adapters owned by process routing.

use std::sync::Arc;

use hl_runtime::{PipeSignalPort, RuntimeProcessSyscalls, SignalBoundaryOutcome, SignalBoundaryPort};

use super::super::process_memory::ProcessMemory;

pub(super) struct PipeSignal {
    pub(super) tasks: Arc<hl_task::TaskRegistry>,
    pub(super) process: hl_task::ProcessId,
}

pub(super) struct FileSizeLimit {
    pub(super) tasks: Arc<hl_task::TaskRegistry>,
    pub(super) process: hl_task::ProcessId,
}

pub(super) struct AsyncSignal {
    pub(super) tasks: Arc<hl_task::TaskRegistry>,
}

pub(super) struct DnotifySignal {
    pub(super) tasks: Arc<hl_task::TaskRegistry>,
    pub(super) process: hl_task::ProcessId,
}

impl hl_runtime::DnotifyPort for DnotifySignal {
    fn arm(
        &self,
        lease: &hl_descriptor::OperationLease,
        mask: u32,
        configured_signal: u8,
    ) -> Result<Box<dyn hl_descriptor::ReadinessSubscription>, hl_runtime::DnotifyError> {
        let file = lease
            .domain_extension()
            .and_then(|extension| extension.downcast_ref::<super::super::path::NativeFile>())
            .ok_or(hl_runtime::DnotifyError::NotDirectory)?;
        let tasks = Arc::clone(&self.tasks);
        let process = self.process;
        let source = lease.signal_source();
        file.subscribe_dnotify(
            mask,
            Arc::new(move || {
                let Some(delivery) = source.notification() else {
                    return;
                };
                let signal = if configured_signal == 0 { 29 } else { configured_signal };
                let Ok(signal) = hl_task::SignalNumber::new(signal) else {
                    return;
                };
                let snapshot = tasks.snapshot();
                let targets = match delivery.owner {
                    hl_descriptor::SignalOwner::Process(0)
                    | hl_descriptor::SignalOwner::Thread(0)
                    | hl_descriptor::SignalOwner::Group(0) => vec![hl_task::PendingTarget::Process(process)],
                    hl_descriptor::SignalOwner::Process(number) => snapshot
                        .processes
                        .iter()
                        .find(|entry| entry.id.number() == number as u32)
                        .map(|entry| vec![hl_task::PendingTarget::Process(entry.id)])
                        .unwrap_or_default(),
                    hl_descriptor::SignalOwner::Thread(number) => snapshot
                        .threads
                        .iter()
                        .find(|entry| entry.id.number() == number as u32)
                        .map(|entry| vec![hl_task::PendingTarget::Thread(entry.id)])
                        .unwrap_or_default(),
                    hl_descriptor::SignalOwner::Group(number) => snapshot
                        .processes
                        .iter()
                        .filter(|entry| entry.process_group.number() == number as u32)
                        .map(|entry| hl_task::PendingTarget::Process(entry.id))
                        .collect(),
                };
                for target in targets {
                    let mut info = hl_task::SignalInfo::bare(signal);
                    info.code = 128;
                    let _ = tasks.enqueue_signal(target, info);
                }
            }),
        )
        .map_err(|error| match error {
            hl_descriptor::ObjectError::InvalidArgument => hl_runtime::DnotifyError::NotDirectory,
            _ => hl_runtime::DnotifyError::Failed,
        })
    }
}

impl hl_runtime::AsyncSignalPort for AsyncSignal {
    fn deliver(&self, source: hl_descriptor::SignalSource) -> Result<(), ()> {
        let Some(delivery) = source.delivery() else {
            return Ok(());
        };
        let signal =
            hl_task::SignalNumber::new(if delivery.signal == 0 { 29 } else { delivery.signal }).map_err(|_| ())?;
        let snapshot = self.tasks.snapshot();
        let targets = match delivery.owner {
            hl_descriptor::SignalOwner::Process(0)
            | hl_descriptor::SignalOwner::Thread(0)
            | hl_descriptor::SignalOwner::Group(0) => Vec::new(),
            hl_descriptor::SignalOwner::Process(number) => snapshot
                .processes
                .iter()
                .find(|process| process.id.number() == number as u32)
                .map(|process| vec![hl_task::PendingTarget::Process(process.id)])
                .unwrap_or_default(),
            hl_descriptor::SignalOwner::Thread(number) => snapshot
                .threads
                .iter()
                .find(|thread| thread.id.number() == number as u32)
                .map(|thread| vec![hl_task::PendingTarget::Thread(thread.id)])
                .unwrap_or_default(),
            hl_descriptor::SignalOwner::Group(number) => snapshot
                .processes
                .iter()
                .filter(|process| process.process_group.number() == number as u32)
                .map(|process| hl_task::PendingTarget::Process(process.id))
                .collect(),
        };
        for target in targets {
            let mut info = hl_task::SignalInfo::bare(signal);
            info.code = -5;
            let _ = self.tasks.enqueue_signal(target, info).map_err(|_| ())?;
        }
        Ok(())
    }
}

impl hl_runtime::FileSizeLimitPort for FileSizeLimit {
    fn soft_limit(&self) -> Result<u64, ()> {
        self.tasks
            .limit(self.process, hl_task::Resource::FileSize)
            .map(|limit| limit.soft)
            .map_err(|_| ())
    }

    fn queue_sigxfsz(&self) -> Result<(), ()> {
        let signal = hl_task::SignalNumber::new(25).map_err(|_| ())?;
        self.tasks
            .enqueue_signal(
                hl_task::PendingTarget::Process(self.process),
                hl_task::SignalInfo::bare(signal),
            )
            .map(|_| ())
            .map_err(|_| ())
    }
}

pub(super) struct SignalBoundary(pub(super) RuntimeProcessSyscalls<ProcessMemory>);

impl SignalBoundaryPort for SignalBoundary {
    fn deliver(&mut self) -> Result<SignalBoundaryOutcome, ()> {
        self.0.deliver_signal_boundary().map_err(|_| ())
    }

    fn restore(&mut self) -> Result<(), ()> {
        self.0.restore_signal_boundary().map_err(|_| ())
    }

    fn resolve_trace(&mut self, signal: Option<u32>) -> Result<(), ()> {
        self.0.resolve_trace_signal(signal).map_err(|_| ())
    }

    fn queue(&mut self, signal: u8, code: i32, address: u64) -> Result<(), ()> {
        self.0.queue_fault(signal, code, address)
    }

    fn terminate(&mut self, signal: u8, dumped_core: bool) -> Result<(), ()> {
        self.0.terminate_signal(signal, dumped_core)
    }

    fn kill(&mut self, scope: hl_linux::SeccompKillScope, signal: u8) -> Result<(), ()> {
        self.0.terminate_seccomp(scope, signal)
    }

    fn seccomp(&mut self, plan: hl_linux::SeccompTrapPlan) -> Result<(), ()> {
        self.0.queue_seccomp(plan)
    }
}

impl PipeSignalPort for PipeSignal {
    fn queue_sigpipe(&self) -> Result<(), ()> {
        let signal = hl_task::SignalNumber::new(13).map_err(|_| ())?;
        self.tasks
            .enqueue_signal(
                hl_task::PendingTarget::Process(self.process),
                hl_task::SignalInfo::bare(signal),
            )
            .map(|_| ())
            .map_err(|_| ())
    }
}
