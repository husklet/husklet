use hl_descriptor::DescriptorFlags;
use hl_linux::{Errno, FilesystemAbi, GuestMemory, LinuxResult, RestartKind};

use crate::path_host::OpenDescriptorState;

use super::{FileErrno, RuntimeFilesystemSyscalls};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn open_may_block(&self, arguments: [u64; 6], extended: bool) -> bool {
        let Some(host) = &self.path_host else {
            return false;
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let plan = if extended {
            abi.openat2(
                arguments[0] as i32,
                arguments[1],
                arguments[2],
                match usize::try_from(arguments[3]) {
                    Ok(size) => size,
                    Err(_) => return false,
                },
            )
        } else {
            abi.openat(arguments[0] as i32, arguments[1], arguments[2], arguments[3] as u32)
        };
        let Ok(plan) = plan else {
            return false;
        };
        if plan.resolve.beneath && plan.operand.path.is_absolute() {
            return false;
        }
        if plan.intent.bits() & hl_vfs::OpenIntent::NOFOLLOW == 0
            && super::proc::DescriptorLink::resolve(plan.operand.path.as_bytes()).is_some()
        {
            return false;
        }
        let base = if plan.operand.path.is_absolute() {
            host.root_base()
        } else if plan.operand.directory.raw() == (-100_i64) as u64 {
            let snapshot = self.working.snapshot();
            if snapshot.deleted {
                return false;
            }
            host.working_base(snapshot.path)
        } else {
            let Ok(lease) = self.descriptors.pin(plan.operand.directory.raw() as i32) else {
                return false;
            };
            host.descriptor_base(lease)
        };
        let Ok(base) = base else {
            return false;
        };
        if plan.intent.bits() & hl_vfs::OpenIntent::NOFOLLOW == 0
            && super::proc::DescriptorLink::resolve_at(base.path().as_str().as_bytes(), plan.operand.path.as_bytes())
                .is_some()
        {
            return false;
        }
        // Resolution errors are returned synchronously by the real open path;
        // they do not justify consuming a waiter lane.
        host.open_may_block(&base, &plan).unwrap_or(false)
    }

    pub(super) fn openat(&self, arguments: [u64; 6], extended: bool) -> LinuxResult {
        let result = self.openat_result(arguments, extended);
        hl_log::hl_debug!(
            hl_log::tag::FS,
            "openat directory={} path={:#x} flags={:#x} extended={extended} result={:#x}",
            arguments[0] as i32,
            arguments[1],
            arguments[2],
            result.encode(),
        );
        result
    }

    fn openat_result(&self, arguments: [u64; 6], extended: bool) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let plan = if extended {
            abi.openat2(
                arguments[0] as i32,
                arguments[1],
                arguments[2],
                match usize::try_from(arguments[3]) {
                    Ok(size) => size,
                    Err(_) => return LinuxResult::Error(Errno::EINVAL),
                },
            )
        } else {
            abi.openat(arguments[0] as i32, arguments[1], arguments[2], arguments[3] as u32)
        };
        let plan = match plan {
            Ok(mut plan) => {
                let creation = plan.intent.bits() & (hl_vfs::OpenIntent::CREATE | hl_vfs::OpenIntent::TEMPORARY) != 0;
                if creation {
                    plan.mode = self.fs_context.apply(plan.mode);
                }
                plan
            }
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        // RESOLVE_BENEATH rejects an absolute pathname before selecting a
        // root, descriptor, projected, or synthetic path adapter. Keeping
        // this Linux invariant at syscall admission prevents special path
        // providers from bypassing the pinned resolver's equivalent check.
        if plan.resolve.beneath && plan.operand.path.is_absolute() {
            return LinuxResult::Error(Errno::EXDEV);
        }
        if plan.intent.bits() & hl_vfs::OpenIntent::NOFOLLOW == 0
            && let Some(source) = super::proc::DescriptorLink::resolve(plan.operand.path.as_bytes())
        {
            let flags = DescriptorFlags::from_bits(if plan.close_on_exec {
                DescriptorFlags::CLOSE_ON_EXEC
            } else {
                0
            });
            return match self.descriptors.duplicate(source, 0, flags) {
                Ok(descriptor) => LinuxResult::Value(descriptor as u64),
                Err(hl_descriptor::DescriptorError::BadDescriptor) => LinuxResult::Error(Errno::ENOENT),
                Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
            };
        }
        let base = if plan.operand.path.is_absolute() {
            host.root_base()
        } else if plan.operand.directory.raw() == (-100_i64) as u64 {
            let snapshot = self.working.snapshot();
            if snapshot.deleted {
                return LinuxResult::Error(Errno::ENOENT);
            }
            host.working_base(snapshot.path)
        } else {
            let descriptor = plan.operand.directory.raw() as i32;
            match self.descriptors.pin(descriptor) {
                Ok(lease) => host.descriptor_base(lease),
                Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
            }
        };
        let base = match base {
            Ok(base) => base,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if plan.intent.bits() & hl_vfs::OpenIntent::NOFOLLOW == 0
            && let Some(source) =
                super::proc::DescriptorLink::resolve_at(base.path().as_str().as_bytes(), plan.operand.path.as_bytes())
        {
            let flags = DescriptorFlags::from_bits(if plan.close_on_exec {
                DescriptorFlags::CLOSE_ON_EXEC
            } else {
                0
            });
            return match self.descriptors.duplicate(source, 0, flags) {
                Ok(descriptor) => LinuxResult::Value(descriptor as u64),
                Err(hl_descriptor::DescriptorError::BadDescriptor) => LinuxResult::Error(Errno::ENOENT),
                Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
            };
        }
        let reservation = match self.descriptors.reserve(0) {
            Ok(reservation) => reservation,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let mut opened = match host.prepare_open(&base, &plan) {
            Ok(opened) => opened,
            Err(crate::RuntimePathError::WouldBlock) if !plan.nonblocking => {
                return LinuxResult::Restart(RestartKind::NoInterrupt);
            }
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let state = OpenDescriptorState::from_plan(&plan);
        let install = match self
            .descriptors
            .prepare_reserved(reservation, opened.object(), state.status, state.flags)
        {
            Ok(install) => install,
            Err(error) => {
                opened.rollback();
                return LinuxResult::Error(FileErrno::descriptor(error));
            }
        };
        if let Err(error) = opened.bind(install.description_identity()) {
            opened.rollback();
            drop(install);
            return LinuxResult::Error(error.errno());
        }
        match opened.commit() {
            Ok(()) => {}
            Err(crate::RuntimePathError::WouldBlock) => {
                opened.rollback();
                drop(install);
                return LinuxResult::Restart(RestartKind::NoInterrupt);
            }
            Err(error) => {
                opened.rollback();
                drop(install);
                return LinuxResult::Error(error.errno());
            }
        }
        // O_TRUNC shortens the file without a truncate syscall, so no
        // BackingChange names it and mapped-file lengths must be reread.
        if plan.intent.bits() & hl_vfs::OpenIntent::TRUNCATE != 0
            && let Some(changes) = &self.backing_changes
        {
            changes.lengths_changed();
        }
        LinuxResult::Value(install.publish() as u64)
    }
}
