use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd};

use hl_descriptor::LinkableInode;
use hl_linux::PathOperand;
use hl_runtime::{
    DirectoryBaseLease, GuestPathBytes, PreparedPathMutation, ResolveError, ResolveRequest, Resolver, RuntimePathError,
};

use super::super::{HostError, NativePath, overlay_lease::ParentLease, pin};

#[derive(Debug)]
pub(in crate::ffi::linux::execution::path) struct NativeLink {
    file: std::fs::File,
    anonymous_exclusive: bool,
    #[cfg(target_os = "linux")]
    anonymous_name: pin::AnonymousSlot,
}

impl NativeLink {
    pub(in crate::ffi::linux::execution::path) const fn new(
        file: std::fs::File,
        anonymous_exclusive: bool,
        #[cfg(target_os = "linux")] anonymous_name: pin::AnonymousSlot,
    ) -> Self {
        Self {
            file,
            anonymous_exclusive,
            #[cfg(target_os = "linux")]
            anonymous_name,
        }
    }

    pub(in crate::ffi::linux::execution::path) fn acquire(
        source: hl_descriptor::OperationLease,
    ) -> Result<std::fs::File, RuntimePathError> {
        let capability = source
            .object()
            .linkable_inode()
            .map_err(|_| RuntimePathError::BadDescriptor)?
            .ok_or(RuntimePathError::BadDescriptor)?;
        let native = capability
            .as_any()
            .downcast_ref::<Self>()
            .ok_or(RuntimePathError::BadDescriptor)?;
        native.file.try_clone().map_err(HostError::map)
    }
}

impl LinkableInode for NativeLink {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

pub(in crate::ffi::linux::execution::path) fn prepare_inode_link(
    host: &NativePath,
    source: hl_descriptor::OperationLease,
    base: &DirectoryBaseLease,
    target: &PathOperand,
    identity: &hl_runtime::AccessIdentity,
) -> Result<Box<dyn PreparedPathMutation>, RuntimePathError> {
    let capability = source
        .object()
        .linkable_inode()
        .map_err(|_| RuntimePathError::BadDescriptor)?
        .ok_or(RuntimePathError::BadDescriptor)?;
    let _ = identity;
    let native = capability
        .as_any()
        .downcast_ref::<NativeLink>()
        .ok_or(RuntimePathError::CrossDevice)?;
    let source = native.file.try_clone().map_err(HostError::map)?;
    let ordinary = host.ordinary()?;
    let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
    let base_path = GuestPathBytes::new(base.path().as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
    let resolved = resolver
        .resolve_with(
            ResolveRequest {
                path: &target.path,
                base: &base_path,
                nofollow_final: true,
                no_symlinks: false,
                allow_missing_final: true,
            },
            base.resolve_constraints(),
        )
        .map_err(super::super::resolution::Policy::runtime_error)?;
    let parent = resolved
        .duplicate_parent()
        .map_err(|error| super::super::resolution::Policy::runtime_error(ResolveError::Host(error)))?;
    let name = CString::new(resolved.final_name().ok_or(RuntimePathError::Invalid)?.as_bytes())
        .map_err(|_| RuntimePathError::Invalid)?;
    let path = pin::Host::path(&parent, &name).or_else(|error| {
        if error == RuntimePathError::NotFound {
            pin::Host::mutation_path(&parent, &name)
        } else {
            Err(error)
        }
    })?;
    let guest =
        GuestPathBytes::new(host.guest_path(&path)?.as_str().as_bytes()).map_err(|_| RuntimePathError::Invalid)?;
    if ordinary
        .mounts()
        .denies_write_bytes(&guest, ordinary.root_read_only(), ordinary.read_only())
    {
        return Err(RuntimePathError::ReadOnly);
    }
    Ok(Box::new(PendingInodeLink {
        source,
        parent,
        name,
        anonymous_exclusive: native.anonymous_exclusive,
        #[cfg(target_os = "linux")]
        anonymous_name: native.anonymous_name.clone(),
    }))
}

#[derive(Debug)]
struct PendingInodeLink {
    source: std::fs::File,
    parent: ParentLease,
    name: CString,
    anonymous_exclusive: bool,
    #[cfg(target_os = "linux")]
    anonymous_name: pin::AnonymousSlot,
}

impl PreparedPathMutation for PendingInodeLink {
    fn commit(&mut self) -> Result<(), RuntimePathError> {
        if self.anonymous_exclusive {
            return Err(RuntimePathError::NotFound);
        }
        #[cfg(target_os = "linux")]
        {
            let parent = self.parent.mutation()?;
            if let Some(result) = self.anonymous_name.materialize(parent.as_raw_fd(), &self.name) {
                if result.is_ok() {
                    self.parent.publish();
                }
                return result;
            }
        }
        Self::publish(&self.source, &self.parent, &self.name)
    }

    fn rollback(self: Box<Self>) {}
}

impl PendingInodeLink {
    #[cfg(target_os = "linux")]
    fn publish(source: &std::fs::File, parent: &ParentLease, name: &CString) -> Result<(), RuntimePathError> {
        // SAFETY: both descriptors and the terminated target name remain live
        // for the non-retaining atomic linkat operation. The empty source name
        // selects the descriptor-owned inode through AT_EMPTY_PATH.
        let result = unsafe {
            libc::linkat(
                source.as_raw_fd(),
                c"".as_ptr(),
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        if result == 0 {
            parent.publish();
            return Ok(());
        }
        Self::copy_publish(source, parent, name)
    }

    #[cfg(target_os = "linux")]
    fn copy_publish(source: &std::fs::File, parent: &ParentLease, name: &CString) -> Result<(), RuntimePathError> {
        // A constrained authority may lack the host capability required by
        // linkat(AT_EMPTY_PATH), even though the guest was admitted to publish
        // this inode. Match the retained engine's capability-independent
        // fallback: create the name exclusively and copy through offset I/O so
        // the source open-file-description cursor remains unchanged.
        // SAFETY: `parent` holds its directory fd open and `name` is a live
        // NUL-terminated CString; openat retains no pointer past return.
        let descriptor = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o600,
            )
        };
        if descriptor < 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        // The exclusive target name is visible before copying can fail.
        parent.publish();
        // SAFETY: openat returned one new owned descriptor.
        let target = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let result = Self::copy_contents(source, &target);
        drop(target);
        if let Err(error) = result {
            // SAFETY: the parent and name remain live; the exclusive create
            // proves this transaction owns the partial target.
            if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
                parent.publish();
            }
            return Err(error);
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn copy_contents(source: &std::fs::File, target: &std::fs::File) -> Result<(), RuntimePathError> {
        use std::os::unix::fs::FileExt;

        let mut buffer = [0_u8; 65_536];
        let mut offset = 0_u64;
        loop {
            let count = source.read_at(&mut buffer, offset).map_err(HostError::map)?;
            if count == 0 {
                return Ok(());
            }
            let mut written = 0;
            while written < count {
                let count = target
                    .write_at(&buffer[written..count], offset + written as u64)
                    .map_err(HostError::map)?;
                if count == 0 {
                    return Err(RuntimePathError::Io);
                }
                written += count;
            }
            offset = offset.checked_add(count as u64).ok_or(RuntimePathError::TooLarge)?;
        }
    }

    #[cfg(target_os = "macos")]
    fn publish(_: &std::fs::File, _: &ParentLease, _: &CString) -> Result<(), RuntimePathError> {
        Err(RuntimePathError::Unsupported)
    }
}
