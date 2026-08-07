//! `process_vm_readv`/`writev` vector copying for the memory syscall surface.

use hl_linux::{Errno, GuestIovec, GuestMarshaller, GuestMemory, IOV_MAXIMUM, LinuxResult};
use hl_memory::MappingHost;

use crate::RuntimeMemorySyscalls;

impl<H: MappingHost, M: GuestMemory> RuntimeMemorySyscalls<H, M> {
    pub(super) fn process_vector(&self, arguments: [u64; 6], reading: bool) -> LinuxResult {
        if arguments[5] != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let (Ok(local_count), Ok(remote_count)) = (usize::try_from(arguments[2]), usize::try_from(arguments[4])) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if local_count > IOV_MAXIMUM || remote_count > IOV_MAXIMUM {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let local = match marshaller.iovecs(arguments[1], local_count) {
            Ok(plan) => plan.vectors,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let remote = match marshaller.iovecs(arguments[3], remote_count) {
            Ok(plan) => plan.vectors,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let target = arguments[0] as i32;
        if target <= 0 || self.process.is_none_or(|process| target as u32 != process) {
            return LinuxResult::Error(Errno::ESRCH);
        }
        if reading {
            self.copy_vectors(&remote, &local)
        } else {
            self.copy_vectors(&local, &remote)
        }
    }

    fn advance_vector(index: &mut usize, offset: &mut u64, left: u64) {
        if left == 0 {
            *index += 1;
            *offset = 0;
        }
    }

    fn copy_vectors(&self, source: &[GuestIovec], destination: &[GuestIovec]) -> LinuxResult {
        let mut source_index = 0;
        let mut destination_index = 0;
        let mut source_offset = 0_u64;
        let mut destination_offset = 0_u64;
        let mut total = 0_u64;
        // A 64 KiB staging buffer matches the kernel's own vectored-copy chunk.
        #[allow(clippy::large_stack_arrays)]
        let mut buffer = [0_u8; 64 * 1024];
        while source_index < source.len() && destination_index < destination.len() {
            let source_left = source[source_index].length - source_offset;
            let destination_left = destination[destination_index].length - destination_offset;
            let amount = source_left.min(destination_left).min(buffer.len() as u64) as usize;
            if amount == 0 {
                Self::advance_vector(&mut source_index, &mut source_offset, source_left);
                Self::advance_vector(&mut destination_index, &mut destination_offset, destination_left);
                continue;
            }
            let Some(source_address) = source[source_index].base.checked_add(source_offset) else {
                return Self::copy_result(total);
            };
            let read = match self.memory.read(source_address, &mut buffer[..amount]) {
                Ok(value) if value != 0 => value,
                _ => return Self::copy_result(total),
            };
            let Some(destination_address) = destination[destination_index].base.checked_add(destination_offset) else {
                return Self::copy_result(total);
            };
            let written = match self.memory.write(destination_address, &buffer[..read]) {
                Ok(value) if value != 0 => value,
                _ => return Self::copy_result(total),
            };
            total += written as u64;
            source_offset += written as u64;
            destination_offset += written as u64;
            if written != amount {
                return LinuxResult::Value(total);
            }
        }
        LinuxResult::Value(total)
    }

    fn copy_result(total: u64) -> LinuxResult {
        if total == 0 {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(total)
        }
    }
}
