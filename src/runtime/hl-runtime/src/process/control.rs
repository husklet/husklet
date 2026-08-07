use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult, PrctlPlan, ProcessAbi};
use hl_task::{ExitStatus, Limit, ProcessGroupId, ProcessId, Resource};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn personality(&self, requested: u32) -> LinuxResult {
        let previous = if requested == u32::MAX {
            self.tasks.personality(self.process)
        } else {
            self.tasks.set_personality(self.process, requested)
        };
        match previous {
            Ok(value) => LinuxResult::Value(value as u64),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn retire_alarm(&self) {
        if self
            .tasks
            .snapshot()
            .threads
            .iter()
            .any(|thread| thread.process == self.process)
        {
            return;
        }
        let Some(alarms) = &self.alarms else { return };
        alarms.remove(self.process);
    }

    pub(crate) fn set_tid_address(&self, address: u64) -> LinuxResult {
        match self.tasks.set_clear_tid(self.thread, address) {
            Ok(()) => LinuxResult::Value(self.thread.number() as u64),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn cleanup_clear_tid(&self, group: bool) {
        let threads = self
            .tasks
            .snapshot()
            .threads
            .into_iter()
            .filter(|thread| thread.process == self.process && (group || thread.id == self.thread))
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        let clear_tid = threads
            .into_iter()
            .filter_map(|thread| {
                self.tasks
                    .take_clear_tid(thread)
                    .ok()
                    .flatten()
                    .map(|address| (thread, address))
            })
            .collect::<Vec<_>>();
        self.clear_tid_values(&clear_tid);
    }

    fn clear_tid_values(&self, clear_tid: &[(hl_task::ThreadId, u64)]) {
        for &(thread, address) in clear_tid {
            if self.memory.write(address, &0_u32.to_le_bytes()) != Ok(4) {
                continue;
            }
            if let Some(futex) = self.futex.as_ref() {
                futex.clear_tid_wake(self.process, thread, address);
            }
        }
    }

    pub(crate) fn exit_group(&self, runtime: &crate::ExitRuntime, status: ExitStatus) -> LinuxResult {
        let snapshot = self.tasks.snapshot();
        let clear_tid = snapshot
            .threads
            .iter()
            .filter(|thread| thread.process == self.process)
            .filter_map(|thread| thread.clear_tid.map(|address| (thread.id, address)))
            .collect::<Vec<_>>();
        let threads = snapshot
            .threads
            .into_iter()
            .filter(|thread| thread.process == self.process)
            .map(|thread| thread.id)
            .collect::<Vec<_>>();
        let Ok(published) = runtime.exit_once(self.process, &threads, status) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if published {
            if let Some(alarms) = &self.alarms {
                alarms.remove(self.process);
            }
            // These effects are irreversible. Never expose them when a
            // reversible exit participant rejects the transaction.
            self.cleanup_pi_threads(&threads);
            self.clear_tid_values(&clear_tid);
        }
        LinuxResult::Value(0)
    }

    fn cleanup_pi_threads(&self, threads: &[hl_task::ThreadId]) {
        let Some(futex) = self.futex.as_ref() else {
            return;
        };
        for thread in threads {
            futex.owner_exit(*thread);
        }
    }

    pub(crate) fn cleanup_pi_owners(&self, group: bool) {
        let Some(futex) = self.futex.as_ref() else {
            return;
        };
        for thread in self.tasks.snapshot().threads {
            if thread.process == self.process && (group || thread.id == self.thread) {
                futex.owner_exit(thread.id);
            }
        }
    }

    pub(crate) fn cleanup_robust_lists(&self, group: bool) -> Result<(), Errno> {
        let obligations: Vec<_> = self
            .tasks
            .snapshot()
            .threads
            .into_iter()
            .filter(|thread| thread.process == self.process && (group || thread.id == self.thread))
            .filter_map(|thread| thread.robust_list.map(|value| (thread.id, value)))
            .collect();
        if obligations.is_empty() {
            return Ok(());
        }
        let cleanup = self.robust_exit.as_ref().ok_or(Errno::ENOSYS)?;
        for (thread, registration) in obligations {
            let current = self.tasks.take_robust_exit(thread).map_err(|_| Errno::ESRCH)?;
            if current != Some(registration) {
                continue;
            }
            // Linux exit is irreversible. Corrupt guest traversal is best-effort.
            let _ = cleanup.cleanup(self.process, thread, registration);
        }
        Ok(())
    }

    pub(crate) fn getrlimit(&self, resource: u32, address: u64) -> LinuxResult {
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let resource = match abi.resource(resource) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let limit = match self.limit(resource) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let staged = match abi.stage_limit(address, limit) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(crate) fn setrlimit(&self, resource: u32, address: u64) -> LinuxResult {
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let resource = match abi.resource(resource) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let limit = match abi.limit(address) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.change_limit(resource, limit)
    }

    pub(crate) fn prlimit(&self, arguments: [u64; 6]) -> LinuxResult {
        let target = match self.limit_target(arguments[0] as u32) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let abi = ProcessAbi::new(&self.memory, self.architecture);
        let resource = match abi.resource(arguments[1] as u32) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let previous = match self.limit_for(target, resource) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let staged = if arguments[3] == 0 {
            None
        } else {
            Some(abi.defer_limit(arguments[3], previous))
        };
        if arguments[2] != 0 {
            let replacement = match abi.limit(arguments[2]) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if let LinuxResult::Error(error) = self.change_limit_for(target, resource, replacement) {
                return LinuxResult::Error(error);
            }
        }
        if let Some(staged) = staged {
            if let Err(error) = staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
                return LinuxResult::Error(error.errno());
            }
        }
        LinuxResult::Value(0)
    }

    fn limit(&self, resource: Resource) -> Result<Limit, Errno> {
        self.limit_for(self.process, resource)
    }

    fn limit_for(&self, process: ProcessId, resource: Resource) -> Result<Limit, Errno> {
        self.tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|snapshot| snapshot.id == process)
            .and_then(|process| {
                process
                    .limits
                    .into_iter()
                    .find_map(|(kind, limit)| (kind == resource).then_some(limit))
            })
            .ok_or(Errno::EINVAL)
    }

    fn change_limit(&self, resource: Resource, replacement: Limit) -> LinuxResult {
        self.change_limit_for(self.process, resource, replacement)
    }

    fn change_limit_for(&self, process: ProcessId, resource: Resource, replacement: Limit) -> LinuxResult {
        let Some(snapshot) = self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|snapshot| snapshot.id == process)
        else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let current = snapshot
            .limits
            .iter()
            .find_map(|(kind, limit)| (*kind == resource).then_some(*limit));
        let caller = self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|snapshot| snapshot.id == self.process);
        let can_raise = caller.is_some_and(|caller| caller.credentials.capabilities.effective & (1_u64 << 24) != 0);
        if !can_raise && current.is_some_and(|limit| replacement.hard > limit.hard) {
            return LinuxResult::Error(Errno::EPERM);
        }
        match self.tasks.set_limit(process, resource, replacement) {
            Ok(()) => {
                if process == self.process
                    && resource == Resource::OpenFiles
                    && let Some(descriptors) = &self.descriptors
                {
                    descriptors.set_admission_limit(replacement.soft);
                }
                LinuxResult::Value(0)
            }
            Err(_) => LinuxResult::Error(Errno::EINVAL),
        }
    }

    fn limit_target(&self, pid: u32) -> Result<ProcessId, Errno> {
        let snapshot = self.tasks.snapshot();
        let caller = snapshot
            .processes
            .iter()
            .find(|process| process.id == self.process)
            .ok_or(Errno::ESRCH)?;
        let target = if pid == 0 {
            caller
        } else {
            snapshot
                .processes
                .iter()
                .find(|process| process.id.number() == pid)
                .ok_or(Errno::ESRCH)?
        };
        let credentials = &caller.credentials;
        let permitted = credentials.capabilities.effective & (1_u64 << 24) != 0
            || [
                credentials.real_user,
                credentials.effective_user,
                credentials.saved_user,
            ]
            .into_iter()
            .all(|user| user == target.credentials.real_user);
        permitted.then_some(target.id).ok_or(Errno::EPERM)
    }

    pub(crate) fn prctl(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = match ProcessAbi::new(&self.memory, self.architecture).prctl(arguments) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if let Some(result) = self.credential_prctl(plan) {
            return result;
        }
        match plan {
            PrctlPlan::SetParentDeathSignal(signal) => {
                if self.tasks.set_pdeath(self.process, signal).is_err() {
                    return LinuxResult::Error(Errno::ESRCH);
                }
            }
            PrctlPlan::SetDumpable(value) => {
                if self.tasks.set_dumpable(self.process, value).is_err() {
                    return LinuxResult::Error(Errno::ESRCH);
                }
            }
            PrctlPlan::SetName(name) => {
                if self.tasks.set_name(self.thread, name).is_err() {
                    return LinuxResult::Error(Errno::ESRCH);
                }
            }
            PrctlPlan::SetTimerSlack(value) => {
                let value = if value == 0 { 50_000 } else { value };
                if self.tasks.set_timer_slack(self.process, value).is_err() {
                    return LinuxResult::Error(Errno::ESRCH);
                }
            }
            PrctlPlan::SetSubreaper(value) => {
                if self.tasks.set_subreaper(self.process, value).is_err() {
                    return LinuxResult::Error(Errno::ESRCH);
                }
            }
            PrctlPlan::SetThp(value) => {
                if self.tasks.set_thp(self.process, value).is_err() {
                    return LinuxResult::Error(Errno::ESRCH);
                }
            }
            PrctlPlan::SetMcePolicy(value) => {
                if self.tasks.set_mce_policy(self.process, value).is_err() {
                    return LinuxResult::Error(Errno::ESRCH);
                }
            }
            PrctlPlan::GetMcePolicy => return self.control_value(3),
            PrctlPlan::TogglePerfEvents => {}
            PrctlPlan::SetMemoryLayout => {
                let capable = self
                    .snapshot()
                    .map(|process| process.credentials.capabilities.effective & (1_u64 << 24) != 0);
                return match capable {
                    Ok(false) => LinuxResult::Error(Errno::EPERM),
                    Ok(true) => LinuxResult::Error(Errno::EINVAL),
                    Err(error) => LinuxResult::Error(error),
                };
            }
            PrctlPlan::GetTimerSlack => return self.control_value(0),
            PrctlPlan::GetThp => return self.control_value(1),
            PrctlPlan::GetSpeculation(_) => return LinuxResult::Value(0),
            PrctlPlan::SetTiming => {}
            PrctlPlan::GetSeccomp => {
                return self
                    .seccomp
                    .as_ref()
                    .map_or(LinuxResult::Error(Errno::EINVAL), |port| port.mode());
            }
            PrctlPlan::SetSeccompStrict => {
                return self
                    .seccomp
                    .as_ref()
                    .map_or(LinuxResult::Error(Errno::EINVAL), |port| port.strict());
            }
            PrctlPlan::SetSeccompFilter { address } => {
                return self
                    .seccomp
                    .as_ref()
                    .map_or(LinuxResult::Error(Errno::EINVAL), |port| port.filter(address));
            }
            PrctlPlan::GetSubreaper { destination } => {
                let value = self.snapshot().map(|process| process.child_subreaper);
                let value = match value {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(error),
                };
                return self.copy_prctl(destination, &u32::from(value).to_le_bytes());
            }
            PrctlPlan::SetNoNewPrivileges
            | PrctlPlan::GetNoNewPrivileges
            | PrctlPlan::SetKeepCapabilities(_)
            | PrctlPlan::GetKeepCapabilities
            | PrctlPlan::ReadCapability(_)
            | PrctlPlan::DropCapability(_)
            | PrctlPlan::GetSecureBits
            | PrctlPlan::SetSecureBits(_)
            | PrctlPlan::AmbientRead(_)
            | PrctlPlan::AmbientRaise(_)
            | PrctlPlan::AmbientLower(_)
            | PrctlPlan::AmbientClear => unreachable!(),
            PrctlPlan::GetDumpable => return self.control_value(2),
            PrctlPlan::GetParentDeathSignal { destination } => {
                let value = self.snapshot().map(|process| process.parent_death_signal);
                let value = match value {
                    Ok(value) => value,
                    Err(error) => return LinuxResult::Error(error),
                };
                return self.copy_prctl(destination, &value.to_le_bytes());
            }
            PrctlPlan::GetName { destination } => {
                let name = self
                    .tasks
                    .snapshot()
                    .threads
                    .into_iter()
                    .find(|thread| thread.id == self.thread)
                    .map(|thread| thread.name);
                let Some(name) = name else {
                    return LinuxResult::Error(Errno::ESRCH);
                };
                return self.copy_prctl(destination, &name);
            }
        }
        LinuxResult::Value(0)
    }

    fn control_value(&self, field: usize) -> LinuxResult {
        let process = match self.snapshot() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        LinuxResult::Value(match field {
            0 => process.timer_slack,
            1 => u64::from(process.thp_disabled),
            3 => u64::from(process.mce_policy),
            _ => u64::from(process.dumpable),
        })
    }

    fn copy_prctl(&self, destination: u64, bytes: &[u8]) -> LinuxResult {
        let copied = GuestMarshaller::new(&self.memory, self.architecture).copy_to(destination, bytes);
        if copied.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(0)
        }
    }

    pub(crate) fn getpgid(&self, pid: i32) -> LinuxResult {
        let Some(process) = self.resolve_process(pid) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        match self.tasks.process_group_id(process) {
            Ok(group) => LinuxResult::Value(group.number() as u64),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn getsid(&self, pid: i32) -> LinuxResult {
        let Some(process) = self.resolve_process(pid) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        match self.tasks.session_id(process) {
            Ok(session) => LinuxResult::Value(session.number() as u64),
            Err(_) => LinuxResult::Error(Errno::ESRCH),
        }
    }

    pub(crate) fn setpgid(&self, pid: i32, pgid: i32) -> LinuxResult {
        if pgid < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let Some(target) = self.resolve_process(pid) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let destination = if pgid == 0 || pgid as u32 == target.number() {
            self.resolve_group(target.number())
        } else {
            match self.resolve_group(pgid as u32) {
                Some(value) => Some(value),
                None => return LinuxResult::Error(Errno::EPERM),
            }
        };
        match self.tasks.set_process_group(self.process, target, destination) {
            Ok(_) => LinuxResult::Value(0),
            Err(hl_task::TaskError::ProcessExeced) => LinuxResult::Error(Errno::EACCES),
            Err(
                hl_task::TaskError::WrongProcess
                | hl_task::TaskError::InvalidProcess(_)
                | hl_task::TaskError::InvalidLifecycle,
            ) => LinuxResult::Error(Errno::ESRCH),
            Err(_) => LinuxResult::Error(Errno::EPERM),
        }
    }

    pub(crate) fn setsid(&self) -> LinuxResult {
        match self.tasks.create_session(self.process) {
            Ok(session) => LinuxResult::Value(session.number() as u64),
            Err(_) => LinuxResult::Error(Errno::EPERM),
        }
    }

    fn resolve_process(&self, number: i32) -> Option<ProcessId> {
        if number == 0 {
            return Some(self.process);
        }
        let number = u32::try_from(number).ok()?;
        self.tasks
            .snapshot()
            .processes
            .into_iter()
            .find_map(|process| (process.id.number() == number).then_some(process.id))
    }

    fn resolve_group(&self, number: u32) -> Option<ProcessGroupId> {
        self.tasks
            .snapshot()
            .process_groups
            .into_iter()
            .find_map(|group| (group.id.number() == number).then_some(group.id))
    }
}
