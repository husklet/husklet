use std::sync::Arc;

use hl_descriptor::{DescriptorFlags, OpenFileDescription, StatusFlags};
use hl_ipc::Pipe;
use hl_linux::{Errno, GuestMemory, LinuxResult};

use crate::{filesystem::errno::FileErrno, filesystem::syscalls::RuntimeFilesystemSyscalls};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn pipe2(&self, output: u64, flags: u32) -> LinuxResult {
        const NONBLOCK: u32 = 0o00004000;
        const CLOEXEC: u32 = 0o02000000;
        const NOTIFICATION: u32 = 0o40000000;
        let direct = match self.architecture {
            hl_linux::GuestArchitecture::Aarch64 => 0x1_0000,
            hl_linux::GuestArchitecture::X86_64 => StatusFlags::DIRECT,
        };
        if flags & !(NONBLOCK | direct | CLOEXEC | NOTIFICATION) != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if flags & NOTIFICATION != 0 {
            return LinuxResult::Error(Errno::ENOSYS);
        }
        let pipe = Arc::new(if flags & direct != 0 {
            Pipe::new_packet(flags & NONBLOCK != 0)
        } else {
            Pipe::new(flags & NONBLOCK != 0)
        });
        let registered = self.pipe_registry.as_ref().map(|registry| registry.open(pipe.clone()));
        let descriptions = registered.as_ref().map_or_else(
            || {
                [
                    pipe.reader.clone() as Arc<dyn OpenFileDescription>,
                    pipe.writer.clone() as Arc<dyn OpenFileDescription>,
                ]
            },
            crate::IpcOpenPipe::descriptions,
        );
        let shared_status = flags & NONBLOCK | u32::from(flags & direct != 0) * StatusFlags::DIRECT;
        let local = DescriptorFlags::from_bits(u32::from(flags & CLOEXEC != 0) * DescriptorFlags::CLOSE_ON_EXEC);
        let objects: Vec<_> = vec![
            (descriptions[0].clone(), StatusFlags::from_bits(shared_status), local),
            (
                descriptions[1].clone(),
                StatusFlags::from_bits(shared_status | 1),
                local,
            ),
        ];
        let prepared = match self.descriptors.prepare_open_batch(0, objects) {
            Ok(prepared) => prepared,
            Err(error) => {
                return LinuxResult::Error(FileErrno::descriptor(error));
            }
        };
        let numbers = prepared.numbers();
        let identities = prepared.description_identities();
        let publication = match registered
            .as_ref()
            .map(|pipe| pipe.prepare([identities[0].identity, identities[1].identity]))
        {
            Some(Ok(publication)) => Some(publication),
            Some(Err(_)) => return LinuxResult::Error(Errno::ENFILE),
            None => None,
        };
        let mut encoded = [0_u8; 8];
        encoded[..4].copy_from_slice(&numbers[0].to_le_bytes());
        encoded[4..].copy_from_slice(&numbers[1].to_le_bytes());
        match self.memory.write(output, &encoded) {
            Ok(8) => {
                if let Some(publication) = publication {
                    publication.publish();
                }
                let published = prepared.publish_all();
                debug_assert_eq!(published, numbers);
                LinuxResult::Value(0)
            }
            Ok(_) | Err(_) => LinuxResult::Error(Errno::EFAULT),
        }
    }

    pub(super) fn pipe_capacity(&self, descriptor: i32) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        match lease.pipe_capacity() {
            Ok(capacity) => LinuxResult::Value(capacity as u64),
            Err(hl_descriptor::ObjectError::NotSupported) => LinuxResult::Error(Errno::EBADF),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }

    pub(super) fn set_pipe_capacity(&self, descriptor: i32, requested: u64) -> LinuxResult {
        if (requested as u32 as i32) < 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let requested = requested as u32 as usize;
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        match lease.set_pipe_capacity(requested) {
            Ok(capacity) => LinuxResult::Value(capacity as u64),
            Err(hl_descriptor::ObjectError::NotSupported) => LinuxResult::Error(Errno::EBADF),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }
}
