use hl_linux::{
    Errno, GuestMarshaller, GuestMemory, LinuxResult, ProcessAbi, ResourceUsage, RestartKind, SignalAbi, WaitKind,
};
use hl_task::{
    ChildClassSelector, ChildEvent, ChildEventKind, ChildSelector, ChildWaitOptions, PreparedChildWait,
    PreparedWaitSelection, ProcessId, SignalDisposition, SignalInfo, SignalNumber, TraceWait,
};

const SA_RESTART: u64 = 0x1000_0000;

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn wait4(&self, pid: i32, status: u64, options: u32, usage: u64) -> LinuxResult {
        let plan = match ProcessAbi::new(&self.memory, self.architecture).wait4(pid, status, options, usage) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let selector = match self.wait_selector(plan.kind) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let wait_options = Self::wait_options(plan.options, false);
        if let Some(result) = self.trace_wait4(pid, status) {
            return result;
        }
        let Ok(prepared) = self.tasks.prepare_wait_child(self.process, selector, wait_options) else {
            return LinuxResult::Error(Errno::ECHILD);
        };
        if let Some(result) = self.finish_prepared_wait(prepared, pid, status, usage) {
            return result;
        }
        if let Some(result) = self.wait_interruption() {
            return result;
        }
        // Blocking is a dispatcher-level syscall restart, not an in-handler retry.
        LinuxResult::Restart(RestartKind::NoInterrupt)
    }

    fn wait_interruption(&self) -> Option<LinuxResult> {
        let prepared = self.tasks.prepare_deliverable_signal(self.thread).ok()??;
        let action = self.tasks.action(self.process, prepared.info().signal).ok()?;
        self.tasks.force_signal_delivery(prepared).ok()?;
        Some(match action.disposition {
            SignalDisposition::Default | SignalDisposition::Ignore => LinuxResult::Restart(RestartKind::NoInterrupt),
            SignalDisposition::Handler(_) if action.flags & SA_RESTART != 0 => {
                LinuxResult::Restart(RestartKind::NoInterrupt)
            }
            SignalDisposition::Handler(_) => LinuxResult::Error(Errno::EINTR),
        })
    }

    fn finish_prepared_wait(
        &self,
        prepared: PreparedChildWait<'_>,
        pid: i32,
        status: u64,
        usage: u64,
    ) -> Option<LinuxResult> {
        match prepared {
            PreparedChildWait::Selection(selection) => Some(
                self.trace_wait4(pid, status)
                    .unwrap_or_else(|| self.finish_wait4(selection, status, usage)),
            ),
            PreparedChildWait::NoChange => Some(LinuxResult::Value(0)),
            PreparedChildWait::WouldBlock => None,
        }
    }

    fn trace_wait4(&self, pid: i32, status: u64) -> Option<LinuxResult> {
        let port = self.ptrace.as_ref()?;
        let tracee = if pid > 0 {
            match self.tasks.process_number(pid as u32) {
                Ok(process) => Some(process),
                Err(_) => return None,
            }
        } else {
            None
        };
        let event = match self.tasks.trace_peek(self.process, tracee) {
            Ok(TraceWait::Event(event)) => event,
            Ok(TraceWait::WouldBlock) | Err(_) => return None,
        };
        if status != 0 {
            let bytes = port.wait_status(event).to_le_bytes();
            if GuestMarshaller::new(&self.memory, self.architecture)
                .copy_to(status, &bytes)
                .fault
                .is_some()
            {
                return Some(LinuxResult::Error(Errno::EFAULT));
            }
        }
        if self.tasks.trace_commit_wait(self.process, event).is_err() {
            return Some(LinuxResult::Error(Errno::ECHILD));
        }
        Some(LinuxResult::Value(event.tracee.number() as u64))
    }

    fn finish_wait4(
        &self,
        selection: PreparedWaitSelection<'_>,
        status_pointer: u64,
        usage_pointer: u64,
    ) -> LinuxResult {
        let event = selection.event();
        let usage = match selection.usage() {
            Ok(value) => Self::wait_usage(value),
            Err(_) => return LinuxResult::Error(Errno::ECHILD),
        };
        let Ok(reaped) = selection.commit() else {
            return LinuxResult::Error(Errno::ECHILD);
        };
        self.release_reaped(reaped.child);
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if usage_pointer != 0 {
            let staged = match ProcessAbi::new(&self.memory, self.architecture).stage_usage(usage_pointer, usage) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if let Err(error) = staged.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        if status_pointer != 0 {
            let staged = match ProcessAbi::new(&self.memory, self.architecture)
                .stage_wait_status(status_pointer, Self::wait_status(event))
            {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if let Err(error) = staged.commit(&marshaller) {
                return LinuxResult::Error(error.errno());
            }
        }
        LinuxResult::Value(reaped.child.number() as u64)
    }

    fn wait_usage(usage: hl_task::CpuUsage) -> ResourceUsage {
        let nanoseconds = usage.total_nanoseconds();
        ResourceUsage {
            user_seconds: i64::try_from(nanoseconds / 1_000_000_000).unwrap_or(i64::MAX),
            user_microseconds: i64::from(((nanoseconds % 1_000_000_000) / 1_000) as u32),
            ..ResourceUsage::default()
        }
    }

    fn release_reaped(&self, child: hl_task::ProcessId) {
        hl_log::hl_info!(
            hl_log::tag::TASK,
            "process reaped parent={} child={}",
            self.process.number(),
            child.number(),
        );
        let retained = self
            .tasks
            .snapshot()
            .processes
            .iter()
            .any(|process| process.id == child);
        if !retained && let Some(reap) = &self.reap {
            reap.remove(child);
        }
    }

    pub(crate) fn waitid(&self, arguments: [u64; 6]) -> LinuxResult {
        let parsed = ProcessAbi::new(&self.memory, self.architecture).waitid(
            arguments[0] as u32,
            arguments[1] as u32,
            arguments[2],
            arguments[3] as u32,
            arguments[4],
        );
        let plan = match parsed {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let selector = match self.wait_selector(plan.kind) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let options = Self::wait_options(plan.options, plan.keep_waitable);
        let Ok(prepared) = self.tasks.prepare_wait_child(self.process, selector, options) else {
            return LinuxResult::Error(Errno::ECHILD);
        };
        match prepared {
            PreparedChildWait::Selection(selection) => {
                return self.finish_waitid(selection, plan.information, plan.usage);
            }
            PreparedChildWait::NoChange => {
                let zero = [0_u8; 128];
                let faulted = GuestMarshaller::new(&self.memory, self.architecture)
                    .copy_to(plan.information, &zero)
                    .fault
                    .is_some();
                return if faulted {
                    LinuxResult::Error(Errno::EFAULT)
                } else {
                    LinuxResult::Value(0)
                };
            }
            PreparedChildWait::WouldBlock => {}
        }
        if let Some(result) = self.wait_interruption() {
            return result;
        }
        // Blocking is a dispatcher-level syscall restart, not an in-handler retry.
        LinuxResult::Restart(RestartKind::NoInterrupt)
    }

    fn finish_waitid(&self, selection: PreparedWaitSelection<'_>, information: u64, usage: u64) -> LinuxResult {
        let event = selection.event();
        let child_usage = match selection.usage() {
            Ok(value) => Self::wait_usage(value),
            Err(_) => return LinuxResult::Error(Errno::ECHILD),
        };
        let signal = SignalNumber::new(17).expect("SIGCHLD");
        let (code, status) = match event.kind {
            ChildEventKind::Exited(hl_task::ExitStatus::Code(status)) => (1, u64::from(status)),
            ChildEventKind::Exited(hl_task::ExitStatus::Signal {
                signal,
                dumped_core: false,
            }) => (2, u64::from(signal)),
            ChildEventKind::Exited(hl_task::ExitStatus::Signal {
                signal,
                dumped_core: true,
            }) => (3, u64::from(signal)),
            ChildEventKind::Stopped(signal) => (5, u64::from(signal.get())),
            ChildEventKind::Continued => (6, 18),
        };
        let info = SignalInfo {
            signal,
            code,
            sender_process: event.child.number(),
            value: status,
            ..SignalInfo::bare(signal)
        };
        let Ok(reaped) = selection.commit() else {
            return LinuxResult::Error(Errno::ECHILD);
        };
        self.release_reaped(reaped.child);
        let staged_info = match SignalAbi::new(&self.memory, self.architecture).stage_info(information, info) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        if staged_info.commit(&marshaller).is_err() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        if usage != 0 {
            let staged_usage = match ProcessAbi::new(&self.memory, self.architecture).stage_usage(usage, child_usage) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if staged_usage.commit(&marshaller).is_err() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        LinuxResult::Value(0)
    }

    fn wait_selector(&self, kind: WaitKind) -> Result<ChildSelector, Errno> {
        Ok(match kind {
            WaitKind::Any => ChildSelector::Any,
            WaitKind::SameProcessGroup => ChildSelector::SameProcessGroup,
            WaitKind::Process(number) => {
                ChildSelector::Process(self.resolve_wait_process(number).ok_or(Errno::ECHILD)?)
            }
            WaitKind::ProcessGroup(number) => ChildSelector::ProcessGroup(
                self.tasks
                    .snapshot()
                    .process_groups
                    .into_iter()
                    .find_map(|group| (group.id.number() == number).then_some(group.id))
                    .ok_or(Errno::ECHILD)?,
            ),
        })
    }

    fn resolve_wait_process(&self, number: u32) -> Option<ProcessId> {
        self.tasks
            .snapshot()
            .processes
            .into_iter()
            .find_map(|process| (process.id.number() == number).then_some(process.id))
    }

    fn wait_options(bits: u32, keep_waitable: bool) -> ChildWaitOptions {
        ChildWaitOptions {
            no_hang: bits & 1 != 0,
            report_stopped: bits & 2 != 0,
            report_continued: bits & 8 != 0,
            keep_waitable,
            class: if bits & 0x4000_0000 != 0 {
                ChildClassSelector::All
            } else if bits & 0x8000_0000 != 0 {
                ChildClassSelector::Clone
            } else {
                ChildClassSelector::Standard
            },
        }
    }

    fn wait_status(event: ChildEvent) -> u32 {
        match event.kind {
            ChildEventKind::Exited(status) => status.wait_status(),
            ChildEventKind::Stopped(signal) => (u32::from(signal.get()) << 8) | 0x7f,
            ChildEventKind::Continued => 0xffff,
        }
    }
}
