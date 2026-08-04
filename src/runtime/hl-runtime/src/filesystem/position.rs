use super::{RuntimeFilesystemSyscalls, errno::FileErrno};
use hl_linux::{Errno, GuestMemory, LinuxResult};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn readahead(&self, descriptor: i32, raw_offset: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if (raw_offset as i64) < 0 || lease.object().kind() != hl_descriptor::ObjectKind::File {
            return LinuxResult::Error(Errno::EINVAL);
        }
        LinuxResult::Value(0)
    }

    pub(super) fn seek(&self, descriptor: i32, raw_offset: u64, whence: u32) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if lease.object().kind() == hl_descriptor::ObjectKind::Pipe {
            return LinuxResult::Error(Errno::ESPIPE);
        }
        let signed = raw_offset as i64;
        let position = match whence {
            0 if signed >= 0 => hl_descriptor::SeekPosition::Start(raw_offset),
            1 => hl_descriptor::SeekPosition::Current(signed),
            2 => hl_descriptor::SeekPosition::End(signed),
            3 if signed >= 0 => hl_descriptor::SeekPosition::Data(raw_offset),
            4 if signed >= 0 => hl_descriptor::SeekPosition::Hole(raw_offset),
            3 | 4 => return LinuxResult::Error(Errno::ENXIO),
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        match lease.seek(position) {
            Ok(offset) => LinuxResult::Value(offset),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }
}
