use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use hl_linux::PathOperand;
use hl_runtime::{
    Access, AccessIdentity, DirectoryBaseLease, GuestPathBytes, PreparedPathMutation, ResolveError, ResolveRequest,
    Resolver, RuntimePathError, RuntimePathHost,
};

use super::{HostError, NativePath, overlay_lease::ParentLease, pin};

mod identity;
mod inode;

pub(super) use identity::Identity;
pub(super) use inode::{NativeLink, prepare_inode_link};

#[derive(Debug)]
enum Mutation {
    Directory(PinnedEntry, u32, Option<std::sync::Arc<super::tmpfs::Budget>>),
    Node(PinnedEntry, u32, u64, Option<std::sync::Arc<super::tmpfs::Budget>>),
    Unlink(PinnedEntry, bool, Option<UnlinkCharge>),
    Rename(PinnedEntry, PinnedEntry, u32),
    Link(PinnedInode, PinnedEntry),
    Symlink(CString, PinnedEntry, Option<std::sync::Arc<super::tmpfs::Budget>>),
    Chmod(PinnedInode, u32, bool),
    Chown(
        PinnedInode,
        Option<u32>,
        Option<u32>,
        std::sync::Arc<super::metadata::Registry>,
    ),
    SetTimes(PinnedInode, [hl_linux::TimestampChange; 2], bool),
}

#[derive(Debug)]
struct PinnedEntry {
    parent: ParentLease,
    name: CString,
    path: PathBuf,
}

#[derive(Debug)]
struct PinnedInode {
    descriptor: OwnedFd,
    path: PathBuf,
}

#[derive(Debug)]
struct UnlinkCharge {
    budget: std::sync::Arc<super::tmpfs::Budget>,
    key: super::tmpfs::Key,
    last_link: bool,
}

impl PinnedEntry {
    fn parent_access(&self, identity: &AccessIdentity) -> Result<(), RuntimePathError> {
        let metadata = super::attribute::Descriptor::new(self.parent.as_raw_fd()).metadata()?;
        let access = Access::from_bits(Access::WRITE | Access::EXECUTE).map_err(|_| RuntimePathError::Invalid)?;
        identity
            .check_access(&metadata, access)
            .map_err(|_| RuntimePathError::Access)
    }

    fn charge(&self, budget: std::sync::Arc<super::tmpfs::Budget>) -> Result<UnlinkCharge, RuntimePathError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent and name are retained by this entry and fstatat
        // initializes status without retaining any argument.
        if unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful fstatat initialized the complete value.
        let status = unsafe { status.assume_init() };
        Ok(UnlinkCharge {
            budget,
            key: super::tmpfs::Key::new(status.st_dev, status.st_ino),
            last_link: status.st_nlink == 1,
        })
    }

    fn track_created(&self, budget: &super::tmpfs::Budget, directory: bool) -> Result<(), RuntimePathError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the created entry remains named below the retained parent.
        if unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful fstatat initialized the complete value.
        let status = unsafe { status.assume_init() };
        if budget
            .created(
                super::tmpfs::Key::new(status.st_dev, status.st_ino),
                status.st_size as u64,
            )
            .is_ok()
        {
            return Ok(());
        }
        let flags = if directory { libc::AT_REMOVEDIR } else { 0 };
        // SAFETY: quota failure rolls back exactly the just-created entry.
        if unsafe { libc::unlinkat(self.parent.as_raw_fd(), self.name.as_ptr(), flags) } != 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        Err(RuntimePathError::NoSpace)
    }
}

impl Mutation {
    fn denied(&self, host: &NativePath) -> Result<bool, RuntimePathError> {
        let ordinary = host.ordinary()?;
        let denied = |path: &Path| {
            let guest = host.guest_path(path)?;
            let bytes = GuestPathBytes::new(guest.as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
            Ok::<_, RuntimePathError>(ordinary.mounts().denies_write_bytes(
                &bytes,
                ordinary.root_read_only(),
                ordinary.read_only(),
            ))
        };
        match self {
            Self::Rename(from, to, _) => Ok(denied(&from.path)? || denied(&to.path)?),
            Self::Link(_, to) | Self::Symlink(_, to, _) => denied(&to.path),
            Self::Directory(entry, _, _) | Self::Node(entry, _, _, _) | Self::Unlink(entry, _, _) => {
                denied(&entry.path)
            }
            Self::Chmod(inode, _, _) | Self::Chown(inode, _, _, _) | Self::SetTimes(inode, _, _) => denied(&inode.path),
        }
    }

    fn authorize(&mut self, identity: &AccessIdentity) -> Result<(), RuntimePathError> {
        match self {
            Self::Chmod(inode, mode, _) => {
                super::attribute::authorize_chmod(inode.descriptor.as_raw_fd(), mode, identity)
            }
            Self::Chown(inode, user, group, _) => {
                super::attribute::authorize_chown(inode.descriptor.as_raw_fd(), *user, *group, identity)
            }
            Self::SetTimes(inode, times, _) => {
                super::attribute::authorize_times(inode.descriptor.as_raw_fd(), times, identity)
            }
            Self::Rename(from, to, _) => {
                from.parent_access(identity)?;
                to.parent_access(identity)
            }
            Self::Directory(entry, _, _) | Self::Node(entry, _, _, _) | Self::Unlink(entry, _, _) => {
                entry.parent_access(identity)
            }
            Self::Link(_, to) | Self::Symlink(_, to, _) => to.parent_access(identity),
        }
    }
}

pub(super) fn prepare(
    host: &NativePath,
    bases: &[DirectoryBaseLease],
    plan: &hl_linux::FsMutationPlan,
    identity: &AccessIdentity,
) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
    let base = |index| bases.get(index).ok_or(RuntimePathError::Invalid);
    let aliases = host.ordinary()?;
    let alias_target = |index: usize, operand: &PathOperand| {
        aliases
            .name_binding(base(index)?.path(), operand.path.as_bytes())
            .map(|binding| binding.is_some())
    };
    let projected = match plan {
        hl_linux::FsMutationPlan::CreateDirectory { target, .. }
        | hl_linux::FsMutationPlan::CreateNode { target, .. }
        | hl_linux::FsMutationPlan::Unlink { target, .. }
        | hl_linux::FsMutationPlan::Chmod { target, .. }
        | hl_linux::FsMutationPlan::Chown { target, .. }
        | hl_linux::FsMutationPlan::SetTimes { target, .. } => alias_target(0, target)?,
        hl_linux::FsMutationPlan::Rename { from, to, .. } | hl_linux::FsMutationPlan::Link { from, to, .. } => {
            alias_target(0, from)? || alias_target(1, to)?
        }
        hl_linux::FsMutationPlan::Symlink { link, .. } => alias_target(0, link)?,
    };
    if projected {
        return Err(RuntimePathError::ReadOnly);
    }
    let mut action = match plan {
        hl_linux::FsMutationPlan::CreateDirectory { target, mode } => {
            let entry = pin_entry(host, base(0)?, target, true)?;
            let budget = host.ordinary()?.shm_budget(&entry.path);
            Mutation::Directory(entry, *mode, budget)
        }
        hl_linux::FsMutationPlan::CreateNode { target, mode, device } => {
            let entry = pin_entry(host, base(0)?, target, true)?;
            let budget = host.ordinary()?.shm_budget(&entry.path);
            Mutation::Node(entry, *mode, *device, budget)
        }
        hl_linux::FsMutationPlan::Unlink { target, directory } => {
            let entry = pin_entry(host, base(0)?, target, false)?;
            let charge = host
                .ordinary()?
                .shm_budget(&entry.path)
                .map(|budget| entry.charge(budget))
                .transpose()?;
            Mutation::Unlink(entry, *directory, charge)
        }
        hl_linux::FsMutationPlan::Rename {
            from,
            to,
            exchange,
            no_replace,
        } => Mutation::Rename(
            pin_entry(host, base(0)?, from, false)?,
            pin_entry(host, base(1)?, to, !*exchange)?,
            if *exchange {
                2
            } else if *no_replace {
                1
            } else {
                0
            },
        ),
        hl_linux::FsMutationPlan::Link { from, to, follow } => {
            let source_base = base(0)?;
            let target = pin_entry(host, base(1)?, to, true)?;
            let source_device = host.resolve(source_base, from)?.metadata()?.identity.device;
            let target_device = super::attribute::Descriptor::new(target.parent.as_raw_fd())
                .metadata()?
                .identity
                .device;
            if source_device != target_device {
                return Err(RuntimePathError::CrossDevice);
            }
            Mutation::Link(pin_inode(host, source_base, from, !*follow)?, target)
        }
        hl_linux::FsMutationPlan::Symlink { target, link } => {
            let entry = pin_entry(host, base(0)?, link, true)?;
            let budget = host.ordinary()?.shm_budget(&entry.path);
            Mutation::Symlink(
                CString::new(target.as_slice()).map_err(|_| RuntimePathError::Invalid)?,
                entry,
                budget,
            )
        }
        hl_linux::FsMutationPlan::Chmod { target, mode } => Mutation::Chmod(
            pin_inode(host, base(0)?, target, target.nofollow)?,
            *mode,
            target.nofollow,
        ),
        hl_linux::FsMutationPlan::Chown { target, user, group } => Mutation::Chown(
            pin_inode(host, base(0)?, target, target.nofollow)?,
            *user,
            *group,
            std::sync::Arc::clone(&host.ownership),
        ),
        hl_linux::FsMutationPlan::SetTimes { target, times } => Mutation::SetTimes(
            pin_inode(host, base(0)?, target, target.nofollow)?,
            *times,
            target.nofollow,
        ),
    };
    if action.denied(host)? {
        return Err(RuntimePathError::ReadOnly);
    }
    action.authorize(identity)?;
    Ok(Box::new(PendingMutation {
        action,
        watches: std::sync::Arc::clone(&host.watches),
    }))
}

fn pin_inode(
    host: &NativePath,
    base: &DirectoryBaseLease,
    operand: &PathOperand,
    nofollow: bool,
) -> Result<PinnedInode, RuntimePathError> {
    let ordinary = host.ordinary()?;
    let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
    let base_path = GuestPathBytes::new(base.path().as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
    let resolved = resolver
        .resolve_with(
            ResolveRequest {
                path: &operand.path,
                base: &base_path,
                nofollow_final: nofollow,
                no_symlinks: false,
                allow_missing_final: false,
            },
            base.resolve_constraints(),
        )
        .map_err(super::resolution::Policy::runtime_error)?;
    let parent = resolved
        .duplicate_parent()
        .map_err(|error| super::resolution::Policy::runtime_error(ResolveError::Host(error)))?;
    let name = CString::new(resolved.final_name().ok_or(RuntimePathError::Invalid)?.as_bytes())
        .map_err(|_| RuntimePathError::Invalid)?;
    let path = pin::Host::path(&parent, &name)?;
    #[cfg(target_os = "linux")]
    let flags = libc::O_PATH | libc::O_CLOEXEC | if nofollow { libc::O_NOFOLLOW } else { 0 };
    #[cfg(target_os = "macos")]
    let flags = libc::O_RDONLY | libc::O_CLOEXEC | if nofollow { libc::O_SYMLINK } else { 0 };
    // SAFETY: the pinned parent and terminated name remain live; success creates
    // one descriptor whose ownership is transferred immediately below.
    let descriptor = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(HostError::map(std::io::Error::last_os_error()));
    }
    // SAFETY: successful openat returned one descriptor not owned elsewhere.
    Ok(PinnedInode {
        descriptor: unsafe { OwnedFd::from_raw_fd(descriptor) },
        path,
    })
}

fn pin_entry(
    host: &NativePath,
    base: &DirectoryBaseLease,
    operand: &PathOperand,
    missing: bool,
) -> Result<PinnedEntry, RuntimePathError> {
    let ordinary = host.ordinary()?;
    let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
    let base_path = GuestPathBytes::new(base.path().as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
    let resolved = resolver
        .resolve_with(
            ResolveRequest {
                path: &operand.path,
                base: &base_path,
                nofollow_final: true,
                no_symlinks: false,
                allow_missing_final: missing,
            },
            base.resolve_constraints(),
        )
        .map_err(super::resolution::Policy::runtime_error)?;
    let parent = resolved
        .duplicate_parent()
        .map_err(|error| super::resolution::Policy::runtime_error(ResolveError::Host(error)))?;
    let name = CString::new(resolved.final_name().ok_or(RuntimePathError::Invalid)?.as_bytes())
        .map_err(|_| RuntimePathError::Invalid)?;
    if operand.path.as_bytes().ends_with(b"/") {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: the pinned parent and terminated final name remain live and
        // status is writable; fstatat retains none of them.
        let result = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result == 0 {
            // SAFETY: successful fstatat initialized the complete status.
            if unsafe { status.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFDIR {
                return Err(RuntimePathError::NotDirectory);
            }
        } else {
            let error = std::io::Error::last_os_error();
            if !missing || error.raw_os_error() != Some(libc::ENOENT) {
                return Err(HostError::map(error));
            }
        }
    }
    let path = if missing {
        pin::Host::path(&parent, &name).or_else(|error| {
            if error == RuntimePathError::NotFound {
                pin::Host::mutation_path(&parent, &name)
            } else {
                Err(error)
            }
        })?
    } else {
        pin::Host::path(&parent, &name)?
    };
    Ok(PinnedEntry { parent, name, path })
}

#[derive(Debug)]
struct PendingMutation {
    action: Mutation,
    watches: std::sync::Arc<super::watch::Hub>,
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
    fn publish_namespace(parent: &ParentLease) {
        parent.publish();
    }

    fn namespace_result(result: i32, parent: &ParentLease) -> Result<(), RuntimePathError> {
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
        ownership: &super::metadata::Registry,
    ) -> Result<(), RuntimePathError> {
        let current = super::attribute::Descriptor::new(inode.descriptor.as_raw_fd()).metadata()?;
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
        super::attribute::set_times_fd(inode.descriptor.as_raw_fd(), changes, nofollow)
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
                super::native::syscall::FCHMODAT2,
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

#[cfg(test)]
mod epoch_tests {
    use std::fs::File;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_runtime::GuestPathBytes;

    use super::{ParentLease, PendingMutation};

    fn publication() -> (ParentLease, Arc<AtomicU64>) {
        let epoch = Arc::new(AtomicU64::new(11));
        let directory = File::open(std::env::temp_dir()).unwrap();
        let parent = ParentLease::upper(GuestPathBytes::new(b"/").unwrap(), directory.into())
            .with_epoch(Arc::clone(&epoch));
        (parent, epoch)
    }

    fn assert_namespace_success_publishes() {
        let (parent, epoch) = publication();
        PendingMutation::namespace_result(0, &parent).unwrap();
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn mkdir_success_publishes_namespace_epoch() {
        assert_namespace_success_publishes();
    }

    #[test]
    fn mknod_success_publishes_namespace_epoch() {
        assert_namespace_success_publishes();
    }

    #[test]
    fn unlink_success_publishes_namespace_epoch() {
        assert_namespace_success_publishes();
    }

    #[test]
    fn rmdir_success_publishes_namespace_epoch() {
        assert_namespace_success_publishes();
    }

    #[test]
    fn rename_success_publishes_namespace_epoch() {
        let (parent, epoch) = publication();
        PendingMutation::publish_namespace(&parent);
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn link_success_publishes_namespace_epoch() {
        let (parent, epoch) = publication();
        PendingMutation::publish_namespace(&parent);
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn symlink_success_publishes_namespace_epoch() {
        assert_namespace_success_publishes();
    }

    #[test]
    fn failed_host_mutation_does_not_publish_namespace_epoch() {
        let (parent, epoch) = publication();
        assert!(PendingMutation::namespace_result(-1, &parent).is_err());
        assert_eq!(epoch.load(Ordering::Acquire), 11);
    }

    #[test]
    fn publication_survives_visible_accounting_failure() {
        let (parent, epoch) = publication();
        PendingMutation::namespace_result(0, &parent).unwrap();
        let accounting = Err::<(), _>(hl_runtime::RuntimePathError::NoSpace);
        assert!(accounting.is_err());
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }
}
