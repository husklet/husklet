use hl_linux::{Errno, GuestAccess, GuestMarshaller, GuestMemory, LinuxResult};

use super::errno::FileErrno;
use crate::RuntimeFilesystemSyscalls;
use crate::atomic_read::AtomicReadCopyout;

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    fn interrupted_read(&self, result: LinuxResult) -> LinuxResult {
        if result == LinuxResult::Error(Errno::EINTR) {
            self.pipe_cancellation
                .as_ref()
                .map_or(result, |cancellation| cancellation.interrupted_result())
        } else {
            result
        }
    }

    pub(super) fn read(&self, descriptor: i32, address: u64, raw_length: u64) -> LinuxResult {
        let Ok(length) = usize::try_from(raw_length) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if raw_length > i32::MAX as u64 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if Self::access_rejects(&lease, true) {
            return LinuxResult::Error(Errno::EBADF);
        }
        if lease.object().kind() == hl_descriptor::ObjectKind::Directory {
            return LinuxResult::Error(Errno::EISDIR);
        }
        if length == 0 {
            return LinuxResult::Value(0);
        }
        if let Some(result) = self.terminal_access(&lease, super::job::TerminalAccess::Read) {
            return result;
        }
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let cancellation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        let context = hl_descriptor::OperationContext {
            actor: self.actor,
            cancellation,
        };
        if let Some(result) = AtomicReadCopyout::execute(&lease, &marshaller, address, length, Some(context)) {
            return self.interrupted_read(result);
        }
        let nonblocking = lease.status().bits() & hl_descriptor::StatusFlags::NONBLOCKING != 0;
        if let Some(result) =
            AtomicReadCopyout::execute_transactional(&lease, &marshaller, address, length, None, nonblocking, context)
        {
            return self.interrupted_read(result);
        }
        let available = match marshaller.probe(address, length, GuestAccess::Write) {
            Ok(0) => return Self::failed_read_probe(&lease),
            Ok(available) => available,
            Err(_) => return Self::failed_read_probe(&lease),
        };
        let mut bytes = vec![0; available];
        let result = lease.read_context(&mut bytes, context);
        match result {
            Ok(count) => {
                let count = count.min(available);
                let progress = marshaller.copy_to(address, &bytes[..count]);
                if progress.fault.is_some() && progress.copied == 0 {
                    LinuxResult::Error(Errno::EFAULT)
                } else {
                    LinuxResult::Value(progress.copied as u64)
                }
            }
            Err(error) => self.interrupted_read(LinuxResult::Error(FileErrno::object(error))),
        }
    }

    pub(super) fn failed_read_probe(lease: &hl_descriptor::OperationLease) -> LinuxResult {
        match lease.probe_read(1) {
            Ok(Some(0)) => LinuxResult::Value(0),
            Ok(Some(_)) | Ok(None) => LinuxResult::Error(Errno::EFAULT),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }
}
