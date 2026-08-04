use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult, UtsName};
use hl_task::{UTS_NAME_MAXIMUM, UtsIdentity};

use crate::RuntimeProcessSyscalls;

impl<M: GuestMemory> RuntimeProcessSyscalls<M> {
    pub(crate) fn uname(&self, destination: u64) -> LinuxResult {
        let identity = match self.tasks.uts_identity(self.process) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        let identity = UtsName::identity(self.architecture, &identity.hostname, &identity.domainname);
        let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_to(destination, identity.bytes());
        if progress.copied == identity.bytes().len() && progress.fault.is_none() {
            LinuxResult::Value(0)
        } else {
            LinuxResult::Error(Errno::EFAULT)
        }
    }

    pub(crate) fn set_uts_name(&self, source: u64, length: usize, domain: bool) -> LinuxResult {
        if !matches!(self.tasks.may_administer_uts(self.process), Ok(true)) {
            return LinuxResult::Error(Errno::EPERM);
        }
        if length > UTS_NAME_MAXIMUM {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let mut bytes = vec![0; length];
        if length != 0 {
            let progress = GuestMarshaller::new(&self.memory, self.architecture).copy_from(source, &mut bytes);
            if progress.copied != length || progress.fault.is_some() {
                return LinuxResult::Error(Errno::EFAULT);
            }
        }
        let mut identity = match self.tasks.uts_identity(self.process) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::ESRCH),
        };
        if domain {
            identity.domainname = bytes;
        } else {
            identity.hostname = bytes;
        }
        let owner = identity.owner();
        match UtsIdentity::owned(identity.hostname, identity.domainname, owner)
            .and_then(|value| self.tasks.replace_uts_identity(self.process, value))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EINVAL),
        }
    }
}
