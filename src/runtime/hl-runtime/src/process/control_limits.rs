//! Resource limit syscalls for the process syscall surface.

use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult, ProcessAbi};
use hl_task::{Limit, ProcessId, Resource};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
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
        if let Some(staged) = staged
            && let Err(error) = staged.commit(&GuestMarshaller::new(&self.memory, self.architecture))
        {
            return LinuxResult::Error(error.errno());
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
}
