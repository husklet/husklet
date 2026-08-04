use hl_descriptor::{DescriptorFlags, StatusFlags};
use hl_linux::{Errno, GuestMemory, LinuxResult};
use hl_task::PendingTarget;

use crate::{ProcessHandleRegistry, RuntimeProcessSyscalls};

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn pidfd_open(&self, pid: u32, flags: u32) -> LinuxResult {
        const PIDFD_NONBLOCK: u32 = 0x800;
        if flags & !PIDFD_NONBLOCK != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let target = match self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|process| process.id.number() == pid)
        {
            Some(process) => process.id,
            None => return LinuxResult::Error(Errno::ESRCH),
        };
        let (Some(descriptors), Some(handles)) = (&self.descriptors, &self.handles) else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let object = ProcessHandleRegistry::create(target);
        let status = if flags & PIDFD_NONBLOCK != 0 {
            StatusFlags::from_bits(StatusFlags::NONBLOCKING)
        } else {
            StatusFlags::default()
        };
        let install = match descriptors.prepare_open(
            0,
            object.clone(),
            status,
            DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
        ) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EMFILE),
        };
        if handles.register(install.description_identity(), object).is_err() {
            return LinuxResult::Error(Errno::ENFILE);
        }
        LinuxResult::Value(install.publish() as u64)
    }

    pub(crate) fn pidfd_signal(&self, arguments: [u64; 6]) -> LinuxResult {
        if arguments[3] != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let (Some(descriptors), Some(handles)) = (&self.descriptors, &self.handles) else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let lease = match descriptors.pin(arguments[0] as i32) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EBADF),
        };
        let target = match handles.target(lease.description_identity()) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EBADF),
        };
        let snapshot = self.tasks.snapshot();
        let Some(sender) = snapshot.processes.iter().find(|process| process.id == self.process) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let Some(target_process) = snapshot.processes.iter().find(|process| process.id == target) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let information = if arguments[2] == 0 {
            let signal = match Self::signal(arguments[1] as u32) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error),
            };
            signal.map(|signal| Self::info(signal, sender, 0))
        } else {
            let queued = match hl_linux::SignalAbi::new(&self.memory, self.architecture).queued_info(
                target.number() as i32,
                arguments[1] as u32,
                arguments[2],
            ) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if target != self.process && (queued.code >= 0 || queued.code == -6) {
                return LinuxResult::Error(Errno::EPERM);
            }
            queued.info
        };
        if !Self::permitted(sender, target_process) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let Some(information) = information else {
            return LinuxResult::Value(0);
        };
        match self.tasks.enqueue_signal(PendingTarget::Process(target), information) {
            Ok(_) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EAGAIN),
        }
    }

    pub(crate) fn pidfd_getfd(&self, arguments: [u64; 6]) -> LinuxResult {
        if arguments[2] != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let (Some(descriptors), Some(handles)) = (&self.descriptors, &self.handles) else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let pidfd = match descriptors.pin(arguments[0] as i32) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EBADF),
        };
        let target = match handles.target(pidfd.description_identity()) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EBADF),
        };
        let snapshot = self.tasks.snapshot();
        let Some(caller) = snapshot.processes.iter().find(|process| process.id == self.process) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        let Some(target_process) = snapshot.processes.iter().find(|process| process.id == target) else {
            return LinuxResult::Error(Errno::ESRCH);
        };
        if matches!(
            target_process.lifecycle,
            hl_task::ProcessLifecycle::Exiting | hl_task::ProcessLifecycle::Zombie
        ) {
            return LinuxResult::Error(Errno::ESRCH);
        }
        const SYS_PTRACE: u64 = 1_u64 << 19;
        let privileged = caller.credentials.capabilities.permitted & SYS_PTRACE != 0;
        let same_identity = [
            target_process.credentials.real_user,
            target_process.credentials.effective_user,
            target_process.credentials.saved_user,
        ]
        .into_iter()
        .all(|user| user == caller.credentials.real_user)
            && [
                target_process.credentials.real_group,
                target_process.credentials.effective_group,
                target_process.credentials.saved_group,
            ]
            .into_iter()
            .all(|group| group == caller.credentials.real_group);
        if self.process != target && !privileged && (!same_identity || !target_process.dumpable) {
            return LinuxResult::Error(Errno::EPERM);
        }
        let transferred = match handles.export(target, arguments[1] as i32) {
            Ok(value) => value,
            Err(crate::ProcessHandleError::BadDescriptor) => return LinuxResult::Error(Errno::EBADF),
            Err(crate::ProcessHandleError::MissingFiles) => return LinuxResult::Error(Errno::ENOSYS),
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        match descriptors.install_description(0, &transferred, DescriptorFlags::default()) {
            Ok(descriptor) => LinuxResult::Value(descriptor as u64),
            Err(_) => LinuxResult::Error(Errno::EMFILE),
        }
    }
}
