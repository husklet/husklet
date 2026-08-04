use hl_linux::{Errno, GuestAccess, GuestMarshaller, GuestMemory, LinuxResult};

use super::errno::FileErrno;
use super::syscalls::RuntimeFilesystemSyscalls;
use crate::atomic_read::AtomicReadCopyout;

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn positional_io(
        &self,
        descriptor: i32,
        address: u64,
        length: u64,
        offset: u64,
        reading: bool,
    ) -> LinuxResult {
        if length > i32::MAX as u64 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if Self::access_rejects(&lease, reading) {
            return LinuxResult::Error(Errno::EBADF);
        }
        if lease.object().kind() == hl_descriptor::ObjectKind::Pipe {
            return LinuxResult::Error(Errno::ESPIPE);
        }
        if (offset as i64) < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let mut length = length as usize;
        if reading {
            let context = hl_descriptor::OperationContext {
                actor: self.actor,
                cancellation: None,
            };
            let nonblocking = lease.status().bits() & hl_descriptor::StatusFlags::NONBLOCKING != 0;
            if let Some(result) = AtomicReadCopyout::execute_transactional(
                &lease,
                &marshaller,
                address,
                length,
                Some(offset),
                nonblocking,
                context,
            ) {
                return result;
            }
            match marshaller.probe(address, length, GuestAccess::Write) {
                Ok(value) if value == length => {}
                _ => return LinuxResult::Error(Errno::EFAULT),
            }
            let mut bytes = vec![0; length];
            return match lease.read_at(offset, &mut bytes) {
                Ok(count) => LinuxResult::Value(marshaller.copy_to(address, &bytes[..count.min(length)]).copied as u64),
                Err(error) => LinuxResult::Error(FileErrno::object(error)),
            };
        }
        length = match self.limit_write(&lease, Some(offset), length) {
            Ok(length) => length,
            Err(error) => return error,
        };
        let mut bytes = vec![0; length];
        let progress = marshaller.copy_from(address, &mut bytes);
        if progress.copied == 0 && progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        match lease.write_at(offset, &bytes[..progress.copied]) {
            Ok(count) => LinuxResult::Value(count.min(progress.copied) as u64),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }

    pub(super) fn split_offset(low: u64, high: u64) -> u64 {
        (low & u32::MAX as u64) | ((high & u32::MAX as u64) << 32)
    }

    pub(super) fn positional_vector(
        &self,
        descriptor: i32,
        address: u64,
        raw_count: u64,
        offset: u64,
        reading: bool,
        flags: Option<u64>,
    ) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let shared = flags.is_some() && offset == u64::MAX;
        if matches!(
            lease.object().kind(),
            hl_descriptor::ObjectKind::Pipe | hl_descriptor::ObjectKind::Socket
        ) && !shared
        {
            return LinuxResult::Error(Errno::ESPIPE);
        }
        let Ok(count) = usize::try_from(raw_count) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let access = if reading { GuestAccess::Write } else { GuestAccess::Read };
        let plan = match marshaller.io_vector_records(address, count, access) {
            Ok(plan) => plan,
            Err(error) => {
                if matches!(error, hl_linux::MarshalError::Fault(_)) && Self::access_rejects(&lease, reading) {
                    return LinuxResult::Error(Errno::EBADF);
                }
                return LinuxResult::Error(FileErrno::vector(error));
            }
        };
        let position = if shared {
            None
        } else if (offset as i64) < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        } else {
            Some(offset)
        };
        self.execute_vector(
            &lease,
            &marshaller,
            plan,
            reading,
            position,
            flags.map(|value| value as u32),
        )
    }
}
