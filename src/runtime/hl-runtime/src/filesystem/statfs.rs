use hl_linux::{Errno, FilesystemAbi, FilesystemTarget, GuestMarshaller, GuestMemory, LinuxResult};

use super::errno::FileErrno;
use crate::{FilesystemStats, RuntimeFilesystemSyscalls, RuntimePathHost};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn statfs(&self, arguments: [u64; 6], descriptor: bool) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let stats = if descriptor {
            self.descriptor_stats(host.as_ref(), arguments[0] as i32)
        } else {
            self.path_stats(host.as_ref(), &abi, arguments[0])
        };
        let stats = match stats {
            Ok(stats) => stats,
            Err(error) => return LinuxResult::Error(error),
        };
        let staged = match abi.stage_statfs(arguments[1], stats) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(_) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(FileErrno::marshal(error)),
        }
    }

    fn descriptor_stats(&self, host: &dyn RuntimePathHost, descriptor: i32) -> Result<FilesystemStats, Errno> {
        let lease = self.descriptors.pin(descriptor).map_err(FileErrno::descriptor)?;
        host.descriptor_node(lease)
            .and_then(|node| node.filesystem())
            .map_err(|error| error.errno())
    }

    fn path_stats(
        &self,
        host: &dyn RuntimePathHost,
        abi: &FilesystemAbi<'_, M>,
        pointer: u64,
    ) -> Result<FilesystemStats, Errno> {
        let operand = match abi.stat(pointer).map_err(FileErrno::marshal)? {
            FilesystemTarget::Path(operand) => operand,
            FilesystemTarget::Descriptor(_) => return Err(Errno::EINVAL),
        };
        let base = self.path_base(host, &operand)?;
        host.filesystem(&base, &operand).map_err(|error| error.errno())
    }
}
