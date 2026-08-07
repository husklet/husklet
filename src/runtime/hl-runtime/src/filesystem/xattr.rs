use hl_linux::{Errno, FilesystemAbi, FilesystemTarget, GuestMarshaller, GuestMemory, LinuxResult, XattrPlan};

use super::errno::FileErrno;
use crate::{ResolvedPathLease, RuntimeFilesystemSyscalls, RuntimePathHost, RuntimeXattrMutation};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn path_xattr(&self, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let plan = match self.xattr_plan(&abi, name, arguments) {
            Ok(plan) => plan,
            Err(error) => {
                return LinuxResult::Error(FileErrno::marshal(error));
            }
        };
        let target = Self::xattr_target(&plan);
        let node = match self.xattr_node(host.as_ref(), target) {
            Ok(node) => node,
            Err(error) => return LinuxResult::Error(error),
        };
        match plan {
            XattrPlan::Set { name, value, flags, .. } => {
                Self::commit_xattr(node.as_ref(), RuntimeXattrMutation::Set { name, value, flags })
            }
            XattrPlan::Remove { name, .. } => Self::commit_xattr(node.as_ref(), RuntimeXattrMutation::Remove { name }),
            XattrPlan::Get { name, output, size, .. } => {
                let bytes = match node.xattr_get(&name) {
                    Ok(bytes) => bytes,
                    Err(error) => return LinuxResult::Error(error),
                };
                self.commit_xattr_output(&abi, output, size, bytes)
            }
            XattrPlan::List { output, size, .. } => {
                let bytes = match node.xattr_list() {
                    Ok(bytes) => bytes,
                    Err(error) => return LinuxResult::Error(error),
                };
                self.commit_xattr_output(&abi, output, size, bytes)
            }
        }
    }

    fn xattr_plan(
        &self,
        abi: &FilesystemAbi<'_, M>,
        name: &str,
        arguments: [u64; 6],
    ) -> Result<XattrPlan, hl_linux::FilesystemMarshalError> {
        let descriptor = name.starts_with('f');
        let nofollow = name.starts_with('l');
        let target = if descriptor {
            FilesystemTarget::Descriptor(arguments[0] as i32)
        } else {
            FilesystemTarget::Path(abi.path_operand(-100, arguments[0], false, nofollow)?)
        };
        let size_argument = if name.contains("listxattr") {
            arguments[2]
        } else {
            arguments[3]
        };
        let size = usize::try_from(size_argument).map_err(|_| hl_linux::FilesystemMarshalError::Invalid)?;
        if name.contains("setxattr") {
            abi.xattr_set(target, arguments[1], arguments[2], size, arguments[4] as u32)
        } else if name.contains("getxattr") {
            abi.xattr_get(target, arguments[1], arguments[2], size)
        } else if name.contains("listxattr") {
            FilesystemAbi::<M>::xattr_list(target, arguments[1], size)
        } else {
            abi.xattr_remove(target, arguments[1])
        }
    }

    fn xattr_target(plan: &XattrPlan) -> &FilesystemTarget {
        match plan {
            XattrPlan::Set { target, .. }
            | XattrPlan::Get { target, .. }
            | XattrPlan::List { target, .. }
            | XattrPlan::Remove { target, .. } => target,
        }
    }

    fn xattr_node(
        &self,
        host: &dyn RuntimePathHost,
        target: &FilesystemTarget,
    ) -> Result<Box<dyn ResolvedPathLease>, Errno> {
        match target {
            FilesystemTarget::Descriptor(descriptor) => {
                let lease = self.descriptors.pin(*descriptor).map_err(FileErrno::descriptor)?;
                host.descriptor_node(lease)
                    .map_err(super::super::path_host::RuntimePathError::errno)
            }
            FilesystemTarget::Path(operand) => {
                let base = self.path_base(host, operand)?;
                host.resolve(&base, operand)
                    .map_err(super::super::path_host::RuntimePathError::errno)
            }
        }
    }

    fn commit_xattr(node: &dyn ResolvedPathLease, mutation: RuntimeXattrMutation) -> LinuxResult {
        let mut prepared = match node.prepare_xattr(mutation) {
            Ok(prepared) => prepared,
            Err(error) => return LinuxResult::Error(error),
        };
        match prepared.commit() {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => {
                prepared.rollback();
                LinuxResult::Error(error)
            }
        }
    }

    fn commit_xattr_output(
        &self,
        abi: &FilesystemAbi<'_, M>,
        output: u64,
        capacity: usize,
        bytes: Vec<u8>,
    ) -> LinuxResult {
        let staged = match abi.stage_xattr_output(output, capacity, bytes) {
            Ok(staged) => staged,
            Err(error) => {
                return LinuxResult::Error(FileErrno::marshal(error));
            }
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(length) => LinuxResult::Value(length as u64),
            Err(error) => LinuxResult::Error(FileErrno::marshal(error)),
        }
    }
}

#[cfg(test)]
#[path = "xattr_test.rs"]
mod tests;
