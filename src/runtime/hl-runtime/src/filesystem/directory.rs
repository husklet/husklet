use hl_linux::{Errno, FilesystemAbi, FilesystemTarget, GuestAccess, GuestMemory, LinuxResult};

use crate::RuntimeFilesystemSyscalls;

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn chroot(&self, pointer: u64) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let operand = match FilesystemAbi::new(&self.memory, self.architecture).stat(pointer) {
            Ok(FilesystemTarget::Path(operand)) => operand,
            Ok(_) => return LinuxResult::Error(Errno::EINVAL),
            Err(error) => return LinuxResult::Error(super::FileErrno::marshal(error)),
        };
        let base = match self.path_base(host.as_ref(), &operand) {
            Ok(base) => base,
            Err(error) => return LinuxResult::Error(error),
        };
        match host.directory_path(&base, &operand) {
            Ok(path) => {
                self.fs_context.replace_root(path);
                LinuxResult::Value(0)
            }
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(super) fn chdir(&self, pointer: u64) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let operand = match FilesystemAbi::new(&self.memory, self.architecture).stat(pointer) {
            Ok(FilesystemTarget::Path(operand)) => operand,
            Ok(_) => return LinuxResult::Error(Errno::EINVAL),
            Err(error) => return LinuxResult::Error(super::FileErrno::marshal(error)),
        };
        let base = match self.path_base(host.as_ref(), &operand) {
            Ok(base) => base,
            Err(error) => return LinuxResult::Error(error),
        };
        match host.directory_path(&base, &operand) {
            Ok(path) => {
                self.working.replace(self.fs_context.guest_path(&path));
                LinuxResult::Value(0)
            }
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(super) fn fchdir(&self, descriptor: i32) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(super::FileErrno::descriptor(error)),
        };
        match host.descriptor_base(lease) {
            Ok(base) => {
                self.working.replace(self.fs_context.guest_path(base.path()));
                LinuxResult::Value(0)
            }
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(super) fn getcwd(&self, output: u64, size: u64) -> LinuxResult {
        let snapshot = self.working.snapshot();
        if snapshot.deleted {
            return LinuxResult::Error(Errno::ENOENT);
        }
        let mut bytes = snapshot.path.as_str().as_bytes().to_vec();
        bytes.push(0);
        let length = match usize::try_from(size) {
            Ok(length) => length,
            Err(_) => return LinuxResult::Error(Errno::EFAULT),
        };
        if length < bytes.len() {
            return LinuxResult::Error(Errno::ERANGE);
        }
        match self.memory.probe(output, bytes.len(), GuestAccess::Write) {
            Ok(probed) if probed == bytes.len() => {}
            _ => return LinuxResult::Error(Errno::EFAULT),
        }
        match self.memory.write(output, &bytes) {
            Ok(written) if written == bytes.len() => LinuxResult::Value(bytes.len() as u64),
            _ => LinuxResult::Error(Errno::EFAULT),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use hl_descriptor::DescriptorTable;
    use hl_linux::{GuestArchitecture, GuestFault};
    use hl_vfs::GuestPath;

    use super::*;

    struct Memory(Mutex<Vec<u8>>);

    impl GuestMemory for Memory {
        fn probe(&self, address: u64, length: usize, _: GuestAccess) -> Result<usize, GuestFault> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .len()
                .saturating_sub(address as usize)
                .min(length))
        }

        fn read(&self, _: u64, _: &mut [u8]) -> Result<usize, GuestFault> {
            Ok(0)
        }

        fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
            let mut bytes = self.0.lock().unwrap();
            let start = address as usize;
            let count = input.len().min(bytes.len().saturating_sub(start));
            bytes[start..start + count].copy_from_slice(&input[..count]);
            Ok(count)
        }
    }

    fn fixture() -> RuntimeFilesystemSyscalls<Memory> {
        let working = Arc::new(crate::WorkingDirectory::root());
        working.replace(GuestPath::new("/work").unwrap());
        RuntimeFilesystemSyscalls::new(
            Arc::new(DescriptorTable::new(4).unwrap()),
            Memory(Mutex::new(vec![0xaa; 16])),
            GuestArchitecture::Aarch64,
        )
        .with_working_directory(working)
    }

    #[test]
    fn exact_copyout() {
        let fixture = fixture();
        assert_eq!(fixture.getcwd(4, 6), LinuxResult::Value(6));
        assert_eq!(&fixture.memory.0.lock().unwrap()[4..10], b"/work\0");
        assert_eq!(fixture.getcwd(0, 5), LinuxResult::Error(Errno::ERANGE));
        assert_eq!(fixture.memory.0.lock().unwrap()[0], 0xaa);
        assert_eq!(fixture.getcwd(15, 6), LinuxResult::Error(Errno::EFAULT));
    }

    #[test]
    fn deleted_is_missing() {
        let fixture = fixture();
        fixture.working.mark_deleted();
        assert_eq!(fixture.getcwd(0, 16), LinuxResult::Error(Errno::ENOENT));
    }
}
