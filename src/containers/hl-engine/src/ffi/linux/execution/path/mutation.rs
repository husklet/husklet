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
mod pending;

pub(super) use identity::Identity;
pub(super) use inode::{NativeLink, prepare_inode_link};
use pending::PendingMutation;

#[derive(Debug)]
enum Mutation {
    Directory(PinnedEntry, u32, Option<std::sync::Arc<super::tmpfs::Budget>>),
    Node(PinnedEntry, u32, u64, Option<std::sync::Arc<super::tmpfs::Budget>>),
    Unlink(PinnedEntry, bool, Option<UnlinkCharge>, Option<(u64, u64)>),
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
    /// Set when the inode came from a name binding, whose host file lies outside
    /// the rootfs and so is described by the binding flag rather than the mount
    /// table, exactly as an `open` of the same name is.
    bound: bool,
}

#[derive(Debug)]
struct UnlinkCharge {
    budget: std::sync::Arc<super::tmpfs::Budget>,
    key: super::tmpfs::Key,
    last_link: bool,
}

impl PinnedEntry {
    /// Authorization reads the parent the guest can see, not the writable one:
    /// a lower-only parent has no upper copy until a mutation materializes it,
    /// and the copy inherits the mode being checked here anyway.
    fn parent_access(
        &self,
        ownership: &super::metadata::Registry,
        identity: &AccessIdentity,
    ) -> Result<(), RuntimePathError> {
        let selected = self.parent.selected().as_raw_fd();
        // The path is read back from the descriptor rather than derived from the entry, because the
        // visible layer for the leaf need not be the layer this parent descriptor came from.
        let metadata = super::attribute::Descriptor::new(selected)
            .projected(ownership, || pin::Host::descriptor_path(selected).ok())?;
        let access = Access::from_bits(Access::WRITE | Access::EXECUTE).map_err(|_| RuntimePathError::Invalid)?;
        identity
            .check_access(&metadata, access)
            .map_err(|_| RuntimePathError::Access)
    }

    /// Linux sticky policy: in a `01777` directory a name may be removed or renamed only by the
    /// owner of the entry, the owner of the directory, or `CAP_FOWNER`. Both operands are guest
    /// owners, so this can only deny once the registry is projected and the bypass is off.
    fn sticky_access(
        &self,
        ownership: &super::metadata::Registry,
        identity: &AccessIdentity,
    ) -> Result<(), RuntimePathError> {
        if identity.capabilities.owner_override {
            return Ok(());
        }
        let selected = self.parent.selected().as_raw_fd();
        let directory = super::attribute::Descriptor::new(selected)
            .projected(ownership, || pin::Host::descriptor_path(selected).ok())?;
        if directory.permissions.bits() & hl_runtime::Permissions::STICKY == 0 || identity.user == directory.user {
            return Ok(());
        }
        // A name that cannot be statted is left to the operation's own error, not turned into EPERM.
        let Ok(mut entry) = super::metadata::HostMetadata::anchored(&self.parent, &self.name) else {
            return Ok(());
        };
        ownership.project_at(Some(&self.path), &mut entry);
        if identity.user == entry.user {
            Ok(())
        } else {
            Err(RuntimePathError::OperationNotPermitted)
        }
    }

    fn charge(&self, budget: std::sync::Arc<super::tmpfs::Budget>) -> Result<UnlinkCharge, RuntimePathError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent and name are retained by this entry and fstatat
        // initializes status without retaining any argument.
        if unsafe {
            libc::fstatat(
                self.parent.selected().as_raw_fd(),
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

    /// The host identity of an entry whose removal frees the inode, so its ownership record can go
    /// with it. `None` whenever another link survives and the record must stay.
    fn freed_identity(&self) -> Option<(u64, u64)> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent and name are retained by this entry and fstatat writes status only.
        if unsafe {
            libc::fstatat(
                self.parent.selected().as_raw_fd(),
                self.name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return None;
        }
        // SAFETY: successful fstatat initialized the complete value.
        let status = unsafe { status.assume_init() };
        (status.st_nlink == 1).then_some((status.st_dev, status.st_ino))
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
            Self::Directory(entry, _, _) | Self::Node(entry, _, _, _) | Self::Unlink(entry, _, _, _) => {
                denied(&entry.path)
            }
            Self::Chmod(inode, _, _) | Self::Chown(inode, _, _, _) | Self::SetTimes(inode, _, _) => {
                if inode.bound {
                    Ok(false)
                } else {
                    denied(&inode.path)
                }
            }
        }
    }

    fn authorize(
        &mut self,
        ownership: &super::metadata::Registry,
        identity: &AccessIdentity,
    ) -> Result<(), RuntimePathError> {
        match self {
            Self::Chmod(inode, mode, _) => super::attribute::authorize_chmod(
                inode.descriptor.as_raw_fd(),
                mode,
                ownership,
                Some(&inode.path),
                identity,
            ),
            Self::Chown(inode, user, group, _) => super::attribute::authorize_chown(
                inode.descriptor.as_raw_fd(),
                *user,
                *group,
                ownership,
                Some(&inode.path),
                identity,
            ),
            Self::SetTimes(inode, times, _) => super::attribute::authorize_times(
                inode.descriptor.as_raw_fd(),
                times,
                ownership,
                Some(&inode.path),
                identity,
            ),
            Self::Rename(from, to, _) => {
                from.parent_access(ownership, identity)?;
                to.parent_access(ownership, identity)?;
                from.sticky_access(ownership, identity)?;
                to.sticky_access(ownership, identity)
            }
            Self::Unlink(entry, _, _, _) => {
                entry.parent_access(ownership, identity)?;
                entry.sticky_access(ownership, identity)
            }
            Self::Directory(entry, _, _) | Self::Node(entry, _, _, _) => entry.parent_access(ownership, identity),
            Self::Link(_, to) | Self::Symlink(_, to, _) => to.parent_access(ownership, identity),
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
    let alias_target =
        |index: usize, operand: &PathOperand| aliases.name_binding(base(index)?.path(), operand.path.as_bytes());
    // A binding's parent is the per-container state directory, not the rootfs, so
    // renaming or unlinking a bound name would damage the runtime's own files
    // whatever the binding's write flag says.
    let namespaced = match plan {
        hl_linux::FsMutationPlan::CreateDirectory { target, .. }
        | hl_linux::FsMutationPlan::CreateNode { target, .. }
        | hl_linux::FsMutationPlan::Unlink { target, .. } => alias_target(0, target)?.is_some(),
        hl_linux::FsMutationPlan::Rename { from, to, .. } | hl_linux::FsMutationPlan::Link { from, to, .. } => {
            alias_target(0, from)?.is_some() || alias_target(1, to)?.is_some()
        }
        hl_linux::FsMutationPlan::Symlink { link, .. } => alias_target(0, link)?.is_some(),
        hl_linux::FsMutationPlan::Chmod { .. }
        | hl_linux::FsMutationPlan::Chown { .. }
        | hl_linux::FsMutationPlan::SetTimes { .. } => false,
    };
    if namespaced {
        return Err(RuntimePathError::ReadOnly);
    }
    // Metadata mutations reach the bound host file itself, so they honour the
    // binding's own flag the way `open` does.
    let bound = match plan {
        hl_linux::FsMutationPlan::Chmod { target, .. }
        | hl_linux::FsMutationPlan::Chown { target, .. }
        | hl_linux::FsMutationPlan::SetTimes { target, .. } => alias_target(0, target)?,
        _ => None,
    };
    if bound.as_ref().is_some_and(|binding| binding.read_only) {
        return Err(RuntimePathError::ReadOnly);
    }
    let metadata_inode = |operand: &PathOperand| match &bound {
        Some(binding) => pin_bound_inode(binding),
        None => pin_inode(host, base(0)?, operand, operand.nofollow),
    };
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
            let freed = entry.freed_identity();
            Mutation::Unlink(entry, *directory, charge, freed)
        }
        hl_linux::FsMutationPlan::Rename {
            from,
            to,
            exchange,
            no_replace,
        } => Mutation::Rename(
            pin_entry(host, base(0)?, from, false)?,
            pin_entry(host, base(1)?, to, !*exchange)?,
            if *exchange { 2 } else { u32::from(*no_replace) },
        ),
        hl_linux::FsMutationPlan::Link { from, to, follow } => {
            let source_base = base(0)?;
            let target = pin_entry(host, base(1)?, to, true)?;
            let source_device = host.resolve(source_base, from)?.metadata()?.identity.device;
            let target_device = super::attribute::Descriptor::new(target.parent.selected().as_raw_fd())
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
        hl_linux::FsMutationPlan::Chmod { target, mode } => {
            Mutation::Chmod(metadata_inode(target)?, *mode, target.nofollow)
        }
        hl_linux::FsMutationPlan::Chown { target, user, group } => Mutation::Chown(
            metadata_inode(target)?,
            *user,
            *group,
            std::sync::Arc::clone(&host.ownership),
        ),
        hl_linux::FsMutationPlan::SetTimes { target, times } => {
            Mutation::SetTimes(metadata_inode(target)?, *times, target.nofollow)
        }
    };
    if action.denied(host)? {
        return Err(RuntimePathError::ReadOnly);
    }
    action.authorize(&host.ownership, identity)?;
    Ok(Box::new(PendingMutation {
        action,
        watches: std::sync::Arc::clone(&host.watches),
        ownership: std::sync::Arc::clone(&host.ownership),
        locks: host.locks.clone(),
        creator: (identity.user, identity.group),
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
    // A walk ending at a directory root (`.`, `..`, a trailing `/.`) names no child, so address the
    // pinned directory itself the way the open path already does rather than rejecting it as EINVAL.
    let name = CString::new(resolved.final_name().map_or(b".".as_slice(), |name| name.as_bytes()))
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
        bound: false,
    })
}

/// Pins the bound host file directly: it lives beside the container's state, not
/// under the rootfs, so no resolver walk can reach it.
fn pin_bound_inode(binding: &super::source::name_bind::NameBinding) -> Result<PinnedInode, RuntimePathError> {
    #[cfg(target_os = "linux")]
    let flags = libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW;
    #[cfg(target_os = "macos")]
    let flags = libc::O_RDONLY | libc::O_CLOEXEC;
    // SAFETY: the binding retains its parent descriptor and terminated leaf;
    // success yields one descriptor owned immediately below.
    let descriptor = unsafe { libc::openat(binding.parent.as_raw_fd(), binding.leaf.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(HostError::map(std::io::Error::last_os_error()));
    }
    // SAFETY: successful openat returned one descriptor not owned elsewhere.
    Ok(PinnedInode {
        descriptor: unsafe { OwnedFd::from_raw_fd(descriptor) },
        path: binding.host.clone(),
        bound: true,
    })
}

/// Rejects a trailing-slash operand whose final component is not a directory.
fn require_trailing_slash_directory(
    parent: &impl AsRawFd,
    name: &CString,
    missing: bool,
) -> Result<(), RuntimePathError> {
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
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if !missing || error.raw_os_error() != Some(libc::ENOENT) {
            return Err(HostError::map(error));
        }
        return Ok(());
    }
    // SAFETY: successful fstatat initialized the complete status.
    if unsafe { status.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(RuntimePathError::NotDirectory);
    }
    Ok(())
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
    // A walk ending at a directory root (`/`, `.`, `..`) names no child. A creation against one
    // still addresses a directory that exists, which Linux reports as EEXIST, not EINVAL; busybox
    // `mkdir -p` walks from `/` and depends on it.
    let Some(final_name) = resolved.final_name() else {
        return Err(if missing {
            RuntimePathError::Exists
        } else {
            RuntimePathError::Invalid
        });
    };
    let name = CString::new(final_name.as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
    if operand.path.as_bytes().ends_with(b"/") {
        require_trailing_slash_directory(&parent, &name, missing)?;
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

#[cfg(test)]
mod epoch_tests {
    use std::fs::File;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use hl_runtime::GuestPathBytes;

    use super::{CString, Mutation, ParentLease, Path, PendingMutation, PinnedEntry, PinnedInode};

    struct Root(std::path::PathBuf, Arc<AtomicU64>);

    impl Root {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn publication() -> (Root, Arc<AtomicU64>) {
        static NEXT: AtomicU64 = AtomicU64::new(1);

        let path = std::env::temp_dir().join(format!(
            "hl-mutation-epoch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).unwrap();
        let epoch = Arc::new(AtomicU64::new(11));
        (Root(path, Arc::clone(&epoch)), epoch)
    }

    /// Drives one real `PendingMutation::commit` against a live temporary
    /// directory so the assertion covers the host syscall, not just `publish`.
    fn commit(action: Mutation) -> Result<(), hl_runtime::RuntimePathError> {
        use hl_runtime::PreparedPathMutation;

        let watches = super::super::watch::Hub::new(b"/").unwrap();
        PendingMutation {
            action,
            watches,
            ownership: std::sync::Arc::new(crate::ffi::linux::execution::path::metadata::Registry::default()),
            locks: None,
            creator: (0, 0),
        }
        .commit()
    }

    /// `ParentLease` owns its descriptor, so each entry pins the root again
    /// against the same shared epoch.
    fn entry(root: &Root, name: &str) -> PinnedEntry {
        let directory = File::open(root.path()).unwrap();
        PinnedEntry {
            parent: ParentLease::upper(GuestPathBytes::new(b"/").unwrap(), directory.into())
                .with_epoch(Arc::clone(&root.1)),
            name: CString::new(name).unwrap(),
            path: root.path().join(name),
        }
    }

    #[test]
    fn mkdir_success_publishes_namespace_epoch() {
        let (root, epoch) = publication();
        commit(Mutation::Directory(entry(&root, "made"), 0o755, None)).unwrap();
        assert!(root.path().join("made").is_dir());
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn symlink_success_publishes_namespace_epoch() {
        let (root, epoch) = publication();
        commit(Mutation::Symlink(
            CString::new("target").unwrap(),
            entry(&root, "link"),
            None,
        ))
        .unwrap();
        assert_eq!(
            std::fs::read_link(root.path().join("link")).unwrap(),
            Path::new("target")
        );
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn unlink_success_publishes_namespace_epoch() {
        let (root, epoch) = publication();
        std::fs::write(root.path().join("gone"), b"x").unwrap();
        commit(Mutation::Unlink(entry(&root, "gone"), false, None, None)).unwrap();
        assert!(!root.path().join("gone").exists());
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn rmdir_success_publishes_namespace_epoch() {
        let (root, epoch) = publication();
        std::fs::create_dir(root.path().join("empty")).unwrap();
        commit(Mutation::Unlink(entry(&root, "empty"), true, None, None)).unwrap();
        assert!(!root.path().join("empty").exists());
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn rename_success_publishes_namespace_epoch() {
        let (root, epoch) = publication();
        std::fs::write(root.path().join("from"), b"x").unwrap();
        commit(Mutation::Rename(entry(&root, "from"), entry(&root, "to"), 0)).unwrap();
        assert!(!root.path().join("from").exists());
        assert!(root.path().join("to").is_file());
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn link_success_publishes_namespace_epoch() {
        let (root, epoch) = publication();
        std::fs::write(root.path().join("source"), b"x").unwrap();
        let source = File::open(root.path().join("source")).unwrap();
        commit(Mutation::Link(
            PinnedInode {
                descriptor: source.into(),
                path: root.path().join("source"),
                bound: false,
            },
            entry(&root, "alias"),
        ))
        .unwrap();
        assert_eq!(
            std::fs::metadata(root.path().join("alias")).unwrap().len(),
            std::fs::metadata(root.path().join("source")).unwrap().len()
        );
        assert_eq!(epoch.load(Ordering::Acquire), 12);
    }

    #[test]
    fn failed_host_mutation_does_not_publish_namespace_epoch() {
        let (root, epoch) = publication();
        std::fs::create_dir(root.path().join("taken")).unwrap();
        // mkdirat over an existing name fails, so nothing became visible.
        assert!(commit(Mutation::Directory(entry(&root, "taken"), 0o755, None)).is_err());
        assert_eq!(epoch.load(Ordering::Acquire), 11);
    }
}
