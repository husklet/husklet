use hl_descriptor::{DescriptorFlags, ExactDuplicate};
use hl_linux::{DescriptorIoSyscalls, Errno, LinuxResult, SyscallOperation};

use super::DescriptorPort;

impl DescriptorPort {
    fn result(result: Result<i32, Errno>) -> LinuxResult {
        result
            .map(|descriptor| LinuxResult::Value(descriptor as u64))
            .unwrap_or_else(LinuxResult::Error)
    }

    fn fcntl(&self, descriptor: i32, command: u32, argument: u64) -> LinuxResult {
        if let Err(error) = self.descriptors.snapshot(descriptor) {
            return LinuxResult::Error(error);
        }
        match command {
            0 => Self::result(
                self.descriptors
                    .duplicate(descriptor, argument as i32, DescriptorFlags::default()),
            ),
            1 => self
                .descriptors
                .flags(descriptor)
                .map(|flags| LinuxResult::Value(flags.bits() as u64))
                .unwrap_or_else(LinuxResult::Error),
            2 => self
                .descriptors
                .update_flags(descriptor, DescriptorFlags::from_fcntl(argument as u32))
                .map(|()| LinuxResult::Value(0))
                .unwrap_or_else(LinuxResult::Error),
            3 => self
                .descriptors
                .status(descriptor)
                .map(|status| LinuxResult::Value(status.bits() as u64))
                .unwrap_or_else(LinuxResult::Error),
            4 => self
                .descriptors
                .update_status(descriptor, argument as u32)
                .map(|()| LinuxResult::Value(0))
                .unwrap_or_else(LinuxResult::Error),
            1030 => Self::result(self.descriptors.duplicate(
                descriptor,
                argument as i32,
                DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC),
            )),
            _ => LinuxResult::Error(Errno::EINVAL),
        }
    }
}

impl DescriptorIoSyscalls for DescriptorPort {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match operation.name {
            "close" => self
                .descriptors
                .close(arguments[0] as i32)
                .map(|()| LinuxResult::Value(0))
                .unwrap_or_else(LinuxResult::Error),
            "dup" => Self::result(
                self.descriptors
                    .duplicate(arguments[0] as i32, 0, DescriptorFlags::default()),
            ),
            "dup2" => Self::result(self.descriptors.duplicate_exact(
                arguments[0] as i32,
                arguments[1] as i32,
                ExactDuplicate::Dup2,
            )),
            "dup3" => {
                let flags = arguments[2] as u32;
                if flags & !0o2000000 != 0 {
                    return LinuxResult::Error(Errno::EINVAL);
                }
                let local = DescriptorFlags::from_bits(if flags == 0 { 0 } else { DescriptorFlags::CLOSE_ON_EXEC });
                Self::result(self.descriptors.duplicate_exact(
                    arguments[0] as i32,
                    arguments[1] as i32,
                    ExactDuplicate::Dup3(local),
                ))
            }
            "fcntl" => self.fcntl(arguments[0] as i32, arguments[1] as u32, arguments[2]),
            "lseek" => self.seek(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "read" => self.read(arguments[0], arguments[1], arguments[2]),
            "readv" => self.readv(arguments[0], arguments[1], arguments[2]),
            "write" => self.write(arguments[0], arguments[1], arguments[2]),
            "writev" => self.writev(arguments[0], arguments[1], arguments[2]),
            "getrandom" => self.random(arguments[0], arguments[1], arguments[2]),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}
