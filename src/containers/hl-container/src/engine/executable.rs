//! Descriptor-pinned executable resolution inside one directory rootfs.
//!
//! This is deliberately not wired into `Spec` yet. It proves that container
//! launch can reuse the runtime VFS walk instead of host-following guest links.

use hl_vfs::{
    GuestName, GuestPathBytes, MountNamespace, MountSourceId, NodeHandle, NodeKind, ResolveConstraints, ResolveError,
    ResolveHostError, ResolveRequest, Resolver, VfsHost,
};
use rustix::fs::{FileType, Mode, OFlags};
use std::{
    collections::BTreeMap,
    os::fd::OwnedFd,
    path::Path,
    sync::{Arc, Mutex},
};

pub(super) struct RootfsExecutable {
    host: Host,
    mounts: MountNamespace,
}

impl RootfsExecutable {
    pub(super) fn open(root: &Path, program: &[u8], working_directory: &[u8]) -> Result<OwnedFd, ResolveError> {
        let host = Host::open(root).map_err(ResolveError::Host)?;
        let resolver = Self {
            host,
            mounts: MountNamespace::new(),
        };
        let path = GuestPathBytes::new(program).map_err(ResolveError::Path)?;
        let base = GuestPathBytes::new(working_directory).map_err(ResolveError::Path)?;
        let walker = Resolver::new(resolver.host.clone(), &resolver.mounts);
        let resolved = walker.resolve_with(
            ResolveRequest {
                path: &path,
                base: &base,
                nofollow_final: false,
                no_symlinks: false,
                allow_missing_final: false,
            },
            ResolveConstraints {
                in_root: true,
                ..ResolveConstraints::default()
            },
        )?;
        let parent = resolved.duplicate_parent().map_err(ResolveError::Host)?;
        let name = resolved
            .final_name()
            .ok_or(ResolveError::Host(ResolveHostError::PermissionDenied))?;
        let executable = rustix::fs::openat(
            &parent,
            name.as_bytes(),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|error| ResolveError::Host(map_error(error)))?;
        let metadata = rustix::fs::fstat(&executable).map_err(|error| ResolveError::Host(map_error(error)))?;
        if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile || metadata.st_mode & 0o111 == 0 {
            return Err(ResolveError::Host(ResolveHostError::PermissionDenied));
        }
        Ok(executable)
    }
}

#[derive(Clone)]
struct Host {
    root: Arc<OwnedFd>,
    pins: Arc<Mutex<Pins>>,
}

struct Pins {
    next: u64,
    descriptors: BTreeMap<u64, OwnedFd>,
}

impl Host {
    fn open(root: &Path) -> Result<Self, ResolveHostError> {
        let root = rustix::fs::open(root, OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
            .map_err(map_error)?;
        Ok(Self {
            root: Arc::new(root),
            pins: Arc::new(Mutex::new(Pins {
                next: 1,
                descriptors: BTreeMap::new(),
            })),
        })
    }

    fn pin(&self, descriptor: OwnedFd) -> Result<NodeHandle, ResolveHostError> {
        let mut pins = self.pins.lock().map_err(|_| ResolveHostError::Io)?;
        let handle = pins.next;
        pins.next = pins.next.checked_add(1).ok_or(ResolveHostError::ResourceLimit)?;
        pins.descriptors.insert(handle, descriptor);
        Ok(NodeHandle::from_raw(handle))
    }

    fn with_pin<T>(
        &self,
        handle: NodeHandle,
        apply: impl FnOnce(&OwnedFd) -> Result<T, ResolveHostError>,
    ) -> Result<T, ResolveHostError> {
        let pins = self.pins.lock().map_err(|_| ResolveHostError::Io)?;
        apply(pins.descriptors.get(&handle.raw()).ok_or(ResolveHostError::Io)?)
    }
}

impl VfsHost for Host {
    type ParentLease = OwnedFd;

    fn pin_root(&self) -> Result<NodeHandle, ResolveHostError> {
        self.pin(rustix::io::dup(&self.root).map_err(map_error)?)
    }

    fn pin_mount(&self, _: MountSourceId) -> Result<NodeHandle, ResolveHostError> {
        Err(ResolveHostError::NotFound)
    }

    fn inspect_child(
        &self,
        directory: NodeHandle,
        component: &GuestName,
    ) -> Result<(NodeHandle, NodeKind), ResolveHostError> {
        self.with_pin(directory, |directory| {
            let descriptor = rustix::fs::openat(
                directory,
                component.as_bytes(),
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_error)?;
            let metadata = rustix::fs::fstat(&descriptor).map_err(map_error)?;
            let kind = match FileType::from_raw_mode(metadata.st_mode) {
                FileType::Directory => NodeKind::Directory,
                FileType::RegularFile => NodeKind::File,
                FileType::Symlink => NodeKind::Symlink,
                _ => NodeKind::Other,
            };
            Ok((descriptor, kind))
        })
        .and_then(|(descriptor, kind)| self.pin(descriptor).map(|handle| (handle, kind)))
    }

    fn read_link(&self, link: NodeHandle, output: &mut [u8]) -> Result<usize, ResolveHostError> {
        self.with_pin(link, |link| {
            let target = rustix::fs::readlinkat(link, "", Vec::new()).map_err(map_error)?;
            let bytes = target.to_bytes();
            if bytes.len() > output.len() {
                return Err(ResolveHostError::ResourceLimit);
            }
            output[..bytes.len()].copy_from_slice(bytes);
            Ok(bytes.len())
        })
    }

    fn duplicate_parent(&self, parent: NodeHandle) -> Result<Self::ParentLease, ResolveHostError> {
        self.with_pin(parent, |parent| rustix::io::dup(parent).map_err(map_error))
    }

    fn close(&self, node: NodeHandle) {
        if let Ok(mut pins) = self.pins.lock() {
            pins.descriptors.remove(&node.raw());
        }
    }
}

fn map_error(error: rustix::io::Errno) -> ResolveHostError {
    match error {
        rustix::io::Errno::NOENT => ResolveHostError::NotFound,
        rustix::io::Errno::NOTDIR => ResolveHostError::NotDirectory,
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => ResolveHostError::PermissionDenied,
        rustix::io::Errno::NOMEM | rustix::io::Errno::MFILE | rustix::io::Errno::NFILE => {
            ResolveHostError::ResourceLimit
        }
        _ => ResolveHostError::Io,
    }
}

#[cfg(test)]
mod tests {
    use super::RootfsExecutable;
    use hl_vfs::{ResolveError, ResolveHostError};
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn executable(path: &Path) {
        std::fs::write(path, b"guest").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    use std::path::Path;

    #[test]
    fn absolute_symlink_is_rebased_inside_the_rootfs() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        executable(&root.path().join("bin/busybox"));
        symlink("/bin/busybox", root.path().join("bin/true")).unwrap();
        RootfsExecutable::open(root.path(), b"/bin/true", b"/").unwrap();
    }

    #[test]
    fn absolute_symlink_cannot_select_the_host_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        symlink("/bin/sh", root.path().join("bin/tool")).unwrap();
        assert_eq!(
            RootfsExecutable::open(root.path(), b"/bin/tool", b"/").unwrap_err(),
            ResolveError::Host(ResolveHostError::NotFound)
        );
    }

    #[test]
    fn symlink_loop_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        symlink("two", root.path().join("bin/one")).unwrap();
        symlink("one", root.path().join("bin/two")).unwrap();
        assert_eq!(
            RootfsExecutable::open(root.path(), b"/bin/one", b"/").unwrap_err(),
            ResolveError::SymlinkLoop
        );
    }
}
