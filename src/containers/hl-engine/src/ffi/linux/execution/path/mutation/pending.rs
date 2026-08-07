//! Deferred host mutation commit and publication.

use std::os::fd::AsRawFd;

use hl_runtime::{PreparedPathMutation, RuntimePathError};

use super::super::HostError;
use super::super::overlay_lease::ParentLease;
use super::{Mutation, PinnedEntry, PinnedInode};

#[derive(Debug)]
pub(super) struct PendingMutation {
    pub(super) action: Mutation,
    pub(super) watches: std::sync::Arc<super::super::watch::Hub>,
}

impl PreparedPathMutation for PendingMutation {
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        let result = match &self.action {
            Mutation::Directory(entry, mode, budget) => {
                // SAFETY: the owned parent descriptor and terminated name stay
                // live through mkdirat, which retains neither argument.
                let result = unsafe { libc::mkdirat(entry.parent.as_raw_fd(), entry.name.as_ptr(), *mode) };
                Self::namespace_result(result, &entry.parent)?;
                budget
                    .as_ref()
                    .map_or(Ok(()), |budget| entry.track_created(budget, true))
            }
            Mutation::Node(entry, mode, device, budget) => {
                // SAFETY: the owned parent descriptor and terminated name stay live;
                // mknodat retains neither argument.
                let result = unsafe {
                    libc::mknodat(
                        entry.parent.as_raw_fd(),
                        entry.name.as_ptr(),
                        *mode,
                        *device as libc::dev_t,
                    )
                };
                Self::namespace_result(result, &entry.parent)?;
                budget
                    .as_ref()
                    .map_or(Ok(()), |budget| entry.track_created(budget, false))
            }
            Mutation::Unlink(entry, directory, charge) => {
                let flags = if *directory { libc::AT_REMOVEDIR } else { 0 };
                // SAFETY: the owned parent descriptor and terminated name stay
                // live through unlinkat, which retains neither argument.
                let result = unsafe { libc::unlinkat(entry.parent.as_raw_fd(), entry.name.as_ptr(), flags) };
                Self::namespace_result(result, &entry.parent)?;
                if let Some(charge) = charge {
                    charge.budget.unlink(charge.key, charge.last_link);
                }
                Ok(())
            }
            Mutation::Rename(from, to, flags) => {
                Self::rename(from, to, *flags)?;
                Self::publish_namespace(&from.parent);
                Ok(())
            }
            Mutation::Link(from, to) => {
                Self::link(from, to)?;
                Self::publish_namespace(&to.parent);
                Ok(())
            }
            Mutation::Symlink(target, link, budget) => {
                // SAFETY: target and link names are terminated and the pinned
                // parent remains owned for this non-retaining symlinkat call.
                let result = unsafe { libc::symlinkat(target.as_ptr(), link.parent.as_raw_fd(), link.name.as_ptr()) };
                Self::namespace_result(result, &link.parent)?;
                budget
                    .as_ref()
                    .map_or(Ok(()), |budget| link.track_created(budget, false))
            }
            Mutation::Chmod(inode, mode, nofollow) => Self::chmod(inode, *mode, *nofollow),
            Mutation::Chown(inode, user, group, ownership) => Self::chown(inode, *user, *group, ownership),
            Mutation::SetTimes(inode, times, nofollow) => Self::set_times(inode, *times, *nofollow),
        };
        if result.is_ok() {
            self.publish();
        }
        result
    }

    fn rollback(self: Box<Self>) {}
}

impl PendingMutation {
    /// Publishes immediately after the host makes a namespace change visible.
    /// Later quota accounting can fail (and its rollback can fail too), so the
    /// outer transaction result is not an adequate invalidation boundary.
    pub(super) fn publish_namespace(parent: &ParentLease) {
        parent.publish();
    }

    pub(super) fn namespace_result(result: i32, parent: &ParentLease) -> Result<(), RuntimePathError> {
        Self::result(result)?;
        Self::publish_namespace(parent);
        Ok(())
    }

    fn publish(&self) {
        match &self.action {
            Mutation::Unlink(entry, _, _) => {
                self.watches.publish(&entry.path, hl_event::InotifyMask::DELETE_SELF);
                if let (Some(parent), Some(name)) = (entry.path.parent(), entry.path.file_name()) {
                    self.watches
                        .publish_child(parent, name.as_encoded_bytes(), hl_event::InotifyMask::DELETE);
                }
            }
            Mutation::Rename(from, to, _) => self.watches.publish_move(&from.path, &to.path),
            Mutation::Chmod(inode, _, _) | Mutation::Chown(inode, _, _, _) | Mutation::SetTimes(inode, _, _) => {
                self.watches.publish(&inode.path, hl_event::InotifyMask::ATTRIB);
            }
            _ => {}
        }
    }

    fn result(result: i32) -> Result<(), RuntimePathError> {
        if result == 0 {
            Ok(())
        } else {
            Err(HostError::map(std::io::Error::last_os_error()))
        }
    }

    fn chown(
        inode: &PinnedInode,
        user: Option<u32>,
        group: Option<u32>,
        ownership: &super::super::metadata::Registry,
    ) -> Result<(), RuntimePathError> {
        let current = super::super::attribute::Descriptor::new(inode.descriptor.as_raw_fd()).metadata()?;
        ownership.set(
            inode.descriptor.as_raw_fd(),
            user.unwrap_or(current.user),
            group.unwrap_or(current.group),
        )
    }

    fn set_times(
        inode: &PinnedInode,
        changes: [hl_linux::TimestampChange; 2],
        nofollow: bool,
    ) -> Result<(), RuntimePathError> {
        super::super::attribute::set_times_fd(inode.descriptor.as_raw_fd(), changes, nofollow)
    }

    #[cfg(target_os = "linux")]
    fn link(from: &PinnedInode, to: &PinnedEntry) -> Result<(), RuntimePathError> {
        // SAFETY: the source capability, target parent, and terminated names stay
        // live; AT_EMPTY_PATH selects the already-resolved source inode.
        let result = unsafe {
            libc::linkat(
                from.descriptor.as_raw_fd(),
                c"".as_ptr(),
                to.parent.as_raw_fd(),
                to.name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        Self::result(result)
    }

    #[cfg(target_os = "macos")]
    fn link(_: &PinnedInode, _: &PinnedEntry) -> Result<(), RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }

    #[cfg(target_os = "linux")]
    fn chmod(inode: &PinnedInode, mode: u32, nofollow: bool) -> Result<(), RuntimePathError> {
        let flags = libc::AT_EMPTY_PATH | if nofollow { libc::AT_SYMLINK_NOFOLLOW } else { 0 };
        // SAFETY: fchmodat2 observes the retained inode descriptor and empty
        // terminated path only for this call and retains neither.
        let result = unsafe {
            libc::syscall(
                super::super::native::syscall::FCHMODAT2,
                inode.descriptor.as_raw_fd(),
                c"".as_ptr(),
                mode,
                flags,
            )
        };
        Self::result(result as i32)
    }

    #[cfg(target_os = "macos")]
    fn chmod(inode: &PinnedInode, mode: u32, nofollow: bool) -> Result<(), RuntimePathError> {
        if nofollow {
            return Err(RuntimePathError::Unsupported);
        }
        // SAFETY: fchmod observes the retained descriptor and retains no state.
        Self::result(unsafe { libc::fchmod(inode.descriptor.as_raw_fd(), mode) })
    }

    #[cfg(target_os = "linux")]
    fn rename(from: &PinnedEntry, to: &PinnedEntry, flags: u32) -> Result<(), RuntimePathError> {
        // SAFETY: both owned parent descriptors and terminated names remain
        // live through atomic renameat2; syscall retains no pointer or fd.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                from.parent.as_raw_fd(),
                from.name.as_ptr(),
                to.parent.as_raw_fd(),
                to.name.as_ptr(),
                flags,
            )
        };
        Self::result(result as i32)
    }

    #[cfg(target_os = "macos")]
    fn rename(from: &PinnedEntry, to: &PinnedEntry, flags: u32) -> Result<(), RuntimePathError> {
        if flags != 0 {
            return Err(RuntimePathError::Unsupported);
        }
        // SAFETY: both owned parent descriptors and terminated names remain
        // live through renameat; the call retains no pointer or descriptor.
        let result = unsafe {
            libc::renameat(
                from.parent.as_raw_fd(),
                from.name.as_ptr(),
                to.parent.as_raw_fd(),
                to.name.as_ptr(),
            )
        };
        Self::result(result)
    }
}
