use hl_linux::{Errno, GuestMemory, LinuxResult};
use hl_task::{CapabilitySets, NamespaceKind, TaskError};

use crate::RuntimeProcessSyscalls;

const CLONE_NEWTIME: u64 = 0x0000_0080;
const CLONE_VM: u64 = 0x0000_0100;
const CLONE_FS: u64 = 0x0000_0200;
const CLONE_FILES: u64 = 0x0000_0400;
const CLONE_SIGHAND: u64 = 0x0000_0800;
const CLONE_THREAD: u64 = 0x0001_0000;
const CLONE_NEWNS: u64 = 0x0002_0000;
const CLONE_SYSVSEM: u64 = 0x0004_0000;
const CLONE_NEWCGROUP: u64 = 0x0200_0000;
const CLONE_NEWUTS: u64 = 0x0400_0000;
const CLONE_NEWIPC: u64 = 0x0800_0000;
const CLONE_NEWUSER: u64 = 0x1000_0000;
const CLONE_NEWPID: u64 = 0x2000_0000;
const CLONE_NEWNET: u64 = 0x4000_0000;

const KNOWN_FLAGS: u64 = CLONE_NEWTIME
    | CLONE_VM
    | CLONE_FS
    | CLONE_FILES
    | CLONE_SIGHAND
    | CLONE_THREAD
    | CLONE_NEWNS
    | CLONE_SYSVSEM
    | CLONE_NEWCGROUP
    | CLONE_NEWUTS
    | CLONE_NEWIPC
    | CLONE_NEWUSER
    | CLONE_NEWPID
    | CLONE_NEWNET;
const NAMESPACE_FLAGS: u64 = CLONE_NEWTIME
    | CLONE_NEWNS
    | CLONE_NEWCGROUP
    | CLONE_NEWUTS
    | CLONE_NEWIPC
    | CLONE_NEWUSER
    | CLONE_NEWPID
    | CLONE_NEWNET;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    #[must_use]
    pub fn with_namespace_handles(
        mut self,
        descriptors: std::sync::Arc<hl_descriptor::DescriptorTable>,
        handles: std::sync::Arc<crate::NamespaceHandleRegistry>,
    ) -> Self {
        self.descriptors = Some(descriptors);
        self.namespace_handles = Some(handles);
        self
    }

    pub(crate) fn unshare(&self, flags: u64) -> LinuxResult {
        if flags & !KNOWN_FLAGS != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if flags == 0 {
            return LinuxResult::Value(0);
        }
        if flags == CLONE_FILES {
            return LinuxResult::Value(0);
        }
        if flags != CLONE_NEWUTS {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let credentials = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        if !credentials.has_capability(CapabilitySets::SYS_ADMIN) {
            return LinuxResult::Error(Errno::EPERM);
        }
        match self.tasks.unshare_namespace(self.process, NamespaceKind::Uts) {
            Ok(_) => LinuxResult::Value(0),
            Err(TaskError::InvalidLifecycle) => LinuxResult::Error(Errno::EINVAL),
            Err(_) => LinuxResult::Error(Errno::ENOMEM),
        }
    }

    pub(crate) fn setns(&self, descriptor: i32, namespace_type: u64) -> LinuxResult {
        if descriptor < 0 {
            return LinuxResult::Error(Errno::EBADF);
        }
        let Some(descriptors) = &self.descriptors else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let lease = match descriptors.pin(descriptor) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EBADF),
        };
        if namespace_type != 0 && (namespace_type & !NAMESPACE_FLAGS != 0 || !namespace_type.is_power_of_two()) {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let Some(handles) = &self.namespace_handles else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let identifier = match handles.identifier(lease.description_identity()) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        if namespace_type != 0 && namespace_type != identifier.kind.clone_flag() {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if identifier.kind != NamespaceKind::Uts {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let credentials = match self.snapshot() {
            Ok(value) => value.credentials,
            Err(error) => return LinuxResult::Error(error),
        };
        if !credentials.has_capability(CapabilitySets::SYS_ADMIN) {
            return LinuxResult::Error(Errno::EPERM);
        }
        match self.tasks.join_namespace(self.process, identifier) {
            Ok(()) => LinuxResult::Value(0),
            Err(TaskError::InvalidLifecycle) => LinuxResult::Error(Errno::EINVAL),
            Err(_) => LinuxResult::Error(Errno::ENOMEM),
        }
    }
}
