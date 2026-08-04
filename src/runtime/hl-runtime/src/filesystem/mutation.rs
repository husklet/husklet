use hl_linux::{Errno, FilesystemAbi, FsMutationPlan, GuestMemory, LinuxResult};

use super::errno::FileErrno;
use crate::{DirectoryBaseLease, RuntimeFilesystemSyscalls, RuntimePathHost};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn path_mutation(&self, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        if name == "fchmod" {
            return self.descriptor_chmod(host.as_ref(), arguments);
        }
        if name == "fchown" {
            return self.descriptor_chown(host.as_ref(), arguments);
        }
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let plan = match name {
            "chmod" => abi.chmodat(-100, arguments[0], arguments[1] as u32, 0),
            "mkdir" => abi.mkdirat(-100, arguments[0], arguments[1] as u32),
            "mkdirat" => abi.mkdirat(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "mknodat" => abi.mknodat(arguments[0] as i32, arguments[1], arguments[2] as u32, arguments[3]),
            "unlink" => abi.unlinkat(-100, arguments[0], 0),
            "rmdir" => abi.unlinkat(-100, arguments[0], 0x200),
            "unlinkat" => abi.unlinkat(arguments[0] as i32, arguments[1], arguments[2] as u32),
            "symlink" => abi.symlinkat(arguments[0], -100, arguments[1]),
            "symlinkat" => abi.symlinkat(arguments[0], arguments[1] as i32, arguments[2]),
            "link" => abi.linkat(-100, arguments[0], -100, arguments[1], 0),
            "linkat" => abi.linkat(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as i32,
                arguments[3],
                arguments[4] as u32,
            ),
            "rename" => abi.renameat2(-100, arguments[0], -100, arguments[1], 0),
            "renameat" | "renameat2" => abi.renameat2(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as i32,
                arguments[3],
                if name == "renameat" { 0 } else { arguments[4] as u32 },
            ),
            "fchmodat" => abi.chmodat(arguments[0] as i32, arguments[1], arguments[2] as u32, 0),
            "fchmodat2" => abi.chmodat(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as u32,
                arguments[3] as u32,
            ),
            "fchownat" => abi.chownat(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as u32,
                arguments[3] as u32,
                arguments[4] as u32,
            ),
            "chown" => abi.chownat(
                -100,
                arguments[0],
                arguments[1] as u32,
                arguments[2] as u32,
                0,
            ),
            "utime" => abi.utime(arguments[0], arguments[1]),
            "utimes" => abi.utimes(-100, arguments[0], arguments[1]),
            "futimesat" => abi.futimesat(arguments[0] as i32, arguments[1], arguments[2]),
            "utimensat" => abi.utimensat(arguments[0] as i32, arguments[1], arguments[2], arguments[3] as u32),
            _ => return LinuxResult::Error(Errno::ENOSYS),
        };
        let plan = match plan {
            Ok(mut plan) => {
                match &mut plan {
                    FsMutationPlan::CreateDirectory { mode, .. } | FsMutationPlan::CreateNode { mode, .. } => {
                        *mode = self.fs_context.apply(*mode);
                    }
                    _ => {}
                }
                plan
            }
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        let prepared_unlink = match (&plan, &self.unix_socket_paths) {
            (FsMutationPlan::Unlink { target, directory: false }, Some(paths)) => {
                paths.prepare_unlink(&target.path)
            }
            _ => None,
        };
        let prepared = if let FsMutationPlan::Chmod { target, mode } = &plan
            && target.allow_empty
            && target.path.as_bytes().is_empty()
        {
            let source = match self.descriptors.pin(target.directory.raw() as i32) {
                Ok(source) => source,
                Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
            };
            let identity = match host.access_identity() {
                Ok(identity) => identity,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            host.prepare_descriptor_chmod(source, *mode, &identity)
        } else if let FsMutationPlan::SetTimes { target, times } = &plan
            && target.allow_empty
            && target.path.as_bytes().is_empty()
        {
            let source = match self.descriptors.pin(target.directory.raw() as i32) {
                Ok(source) => source,
                Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
            };
            let identity = match host.access_identity() {
                Ok(identity) => identity,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            host.prepare_descriptor_times(source, *times, &identity)
        } else if let FsMutationPlan::Link { from, to, follow } = &plan
            && ((from.allow_empty && from.path.as_bytes().is_empty())
                || (*follow && Self::proc_descriptor(from.path.as_bytes()).is_some()))
        {
            let descriptor = Self::proc_descriptor(from.path.as_bytes()).unwrap_or(from.directory.raw() as i32);
            let source = match self.descriptors.pin(descriptor) {
                Ok(source) => source,
                Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
            };
            let target = match self.mutation_base(host.as_ref(), to) {
                Ok(target) => target,
                Err(error) => return LinuxResult::Error(error),
            };
            let identity = match host.access_identity() {
                Ok(identity) => identity,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            if from.allow_empty && from.path.as_bytes().is_empty() && !identity.capabilities.dac_read_search {
                return LinuxResult::Error(hl_linux::Errno::ENOENT);
            }
            host.prepare_inode_link(source, &target, to, &identity)
        } else {
            let bases = match self.mutation_bases(host.as_ref(), &plan) {
                Ok(bases) => bases,
                Err(error) => return LinuxResult::Error(error),
            };
            let identity = match host.access_identity() {
                Ok(identity) => identity,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            host.prepare_mutation(&bases, &plan, &identity)
        };
        let mut transaction = match prepared {
            Ok(transaction) => transaction,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match transaction.commit() {
            Ok(()) => {
                if let Some(unlink) = prepared_unlink {
                    unlink.committed();
                }
                LinuxResult::Value(0)
            }
            Err(error) => {
                transaction.rollback();
                LinuxResult::Error(error.errno())
            }
        }
    }

    fn proc_descriptor(path: &[u8]) -> Option<i32> {
        let digits = path.strip_prefix(b"/proc/self/fd/")?;
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    fn descriptor_chmod(&self, host: &dyn RuntimePathHost, arguments: [u64; 6]) -> LinuxResult {
        let source = match self.descriptors.pin(arguments[0] as i32) {
            Ok(source) => source,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let identity = match host.access_identity() {
            Ok(identity) => identity,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let mut prepared = match host.prepare_descriptor_chmod(source, arguments[1] as u32, &identity) {
            Ok(prepared) => prepared,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        prepared
            .commit()
            .map_or_else(|error| LinuxResult::Error(error.errno()), |_| LinuxResult::Value(0))
    }

    fn descriptor_chown(&self, host: &dyn RuntimePathHost, arguments: [u64; 6]) -> LinuxResult {
        let source = match self.descriptors.pin(arguments[0] as i32) {
            Ok(source) => source,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let identity = match host.access_identity() {
            Ok(identity) => identity,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let optional = |value: u64| (value as u32 != u32::MAX).then_some(value as u32);
        let mut prepared =
            match host.prepare_descriptor_chown(source, optional(arguments[1]), optional(arguments[2]), &identity) {
                Ok(prepared) => prepared,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
        prepared
            .commit()
            .map_or_else(|error| LinuxResult::Error(error.errno()), |_| LinuxResult::Value(0))
    }

    fn mutation_bases(
        &self,
        host: &dyn RuntimePathHost,
        plan: &FsMutationPlan,
    ) -> Result<Vec<DirectoryBaseLease>, Errno> {
        let operands: Vec<_> = match plan {
            FsMutationPlan::CreateDirectory { target, .. }
            | FsMutationPlan::CreateNode { target, .. }
            | FsMutationPlan::Unlink { target, .. }
            | FsMutationPlan::Chmod { target, .. }
            | FsMutationPlan::Chown { target, .. }
            | FsMutationPlan::SetTimes { target, .. } => vec![target],
            FsMutationPlan::Rename { from, to, .. } | FsMutationPlan::Link { from, to, .. } => vec![from, to],
            FsMutationPlan::Symlink { link, .. } => vec![link],
        };
        operands
            .into_iter()
            .map(|operand| self.mutation_base(host, operand))
            .collect()
    }

    fn mutation_base(
        &self,
        host: &dyn RuntimePathHost,
        operand: &hl_linux::PathOperand,
    ) -> Result<DirectoryBaseLease, Errno> {
        if operand.path.is_absolute() {
            return host.root_base().map_err(|error| error.errno());
        }
        if operand.directory.raw() == (-100_i64) as u64 {
            let snapshot = self.working.snapshot();
            if snapshot.deleted {
                return Err(Errno::ENOENT);
            }
            return host.working_base(snapshot.path).map_err(|error| error.errno());
        }
        let lease = self
            .descriptors
            .pin(operand.directory.raw() as i32)
            .map_err(FileErrno::descriptor)?;
        host.descriptor_base(lease).map_err(|error| error.errno())
    }
}
