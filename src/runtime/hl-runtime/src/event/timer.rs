use hl_descriptor::DescriptorTable;
use hl_event::TimerFd;
use hl_linux::{Errno, EventAbi, GuestArchitecture, GuestMarshaller, GuestMemory, LinuxResult};
use std::sync::Arc;

use super::errno::ErrorMap;
use crate::{OperationRegistry, filesystem::FilesystemErrno};

pub(crate) struct TimerOperations<'a, M: GuestMemory> {
    pub(crate) descriptors: Arc<DescriptorTable>,
    pub(crate) operations: Arc<OperationRegistry>,
    pub(crate) memory: &'a M,
    pub(crate) architecture: GuestArchitecture,
}

impl<M: GuestMemory> TimerOperations<'_, M> {
    pub(crate) fn gettime(&self, descriptor: i32, output: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FilesystemErrno::descriptor(error)),
        };
        let timer: Arc<TimerFd> = match self.operations.timer(lease.description_identity()) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let setting = match timer.get_time() {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        let staged = match EventAbi::new(self.memory, self.architecture).timerfd_gettime_copyout(output, setting) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        match staged.commit(&GuestMarshaller::new(self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(ErrorMap::marshal(error)),
        }
    }

    pub(crate) fn settime(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = EventAbi::new(self.memory, self.architecture);
        let plan = match abi.timerfd_settime(arguments[1] as u32, arguments[2], arguments[3]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
        };
        let lease = match self.descriptors.pin(arguments[0] as i32) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FilesystemErrno::descriptor(error)),
        };
        let timer = match self.operations.timer(lease.description_identity()) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        if let Some(output) = plan.old_value {
            let previous = match timer.get_time() {
                Ok(value) => value,
                Err(_) => return LinuxResult::Error(Errno::EINVAL),
            };
            let staged = match abi.timerfd_gettime_copyout(output, previous) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(ErrorMap::marshal(error)),
            };
            if let Err(error) = staged.commit(&GuestMarshaller::new(self.memory, self.architecture)) {
                return LinuxResult::Error(ErrorMap::marshal(error));
            }
        }
        match timer.set_time(plan.flags, plan.setting) {
            Ok(_) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EINVAL),
        }
    }
}
