use std::ffi::CString;
use std::os::fd::AsRawFd;
use std::sync::Arc;

use hl_runtime::{
    Access, GuestPathBytes, PreparedUnixSocketPathBind, PreparedUnixSocketPathUnlink, ResolveConstraints, ResolveError,
    ResolveRequest, Resolver, RuntimePathError, RuntimePathHost, UnixSocketPathPort,
};

use super::{HostError, NativePath, overlay_lease::ParentLease};

pub(in crate::ffi::linux::execution) struct UnixSocketPaths {
    host: Arc<NativePath>,
    namespace: Arc<hl_network::UnixNamespace>,
    context: Arc<hl_runtime::FsContext>,
}

impl UnixSocketPaths {
    pub(in crate::ffi::linux::execution) fn new(
        host: Arc<NativePath>,
        namespace: Arc<hl_network::UnixNamespace>,
        context: Arc<hl_runtime::FsContext>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            namespace,
            context,
        })
    }

    fn entry(&self, pathname: &GuestPathBytes, missing: bool) -> Result<Entry, RuntimePathError> {
        let ordinary = self.host.ordinary()?;
        let context_root = self.context.root();
        let root = GuestPathBytes::new(context_root.as_str().as_bytes()).expect("filesystem root path is valid");
        let resolver = Resolver::new(ordinary.host(), ordinary.mounts());
        let resolved = resolver
            .resolve_with(
                ResolveRequest {
                    path: pathname,
                    base: &root,
                    nofollow_final: true,
                    no_symlinks: false,
                    allow_missing_final: missing,
                },
                ResolveConstraints {
                    in_root: context_root.as_str() != "/",
                    ..ResolveConstraints::default()
                },
            )
            .map_err(super::resolution::Policy::runtime_error)?;
        let parent = resolved
            .duplicate_parent()
            .map_err(|error| super::resolution::Policy::runtime_error(ResolveError::Host(error)))?;
        let name = CString::new(resolved.final_name().ok_or(RuntimePathError::Invalid)?.as_bytes())
            .map_err(|_| RuntimePathError::Invalid)?;
        Ok(Entry { parent, name })
    }
}

#[derive(Debug)]
struct Entry {
    parent: ParentLease,
    name: CString,
}

impl Entry {
    fn inode(&self) -> Result<(u64, u64), RuntimePathError> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        let result = unsafe {
            libc::fstatat(
                self.parent.as_raw_fd(),
                self.name.as_ptr(),
                status.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(HostError::map(std::io::Error::last_os_error()));
        }
        let status = unsafe { status.assume_init() };
        Ok((status.st_dev, status.st_ino))
    }
}

#[derive(Debug)]
struct Bind {
    entry: Entry,
    inode: (u64, u64),
}

impl PreparedUnixSocketPathBind for Bind {
    fn commit(self: Box<Self>) {}
    fn rollback(self: Box<Self>) {
        if self.entry.inode().ok() == Some(self.inode) {
            unsafe {
                libc::unlinkat(self.entry.parent.as_raw_fd(), self.entry.name.as_ptr(), 0);
            }
        }
    }
}

#[derive(Debug)]
struct Unlink {
    namespace: Arc<hl_network::UnixNamespace>,
    pathname: Vec<u8>,
    binding: hl_network::UnixBinding,
}

impl PreparedUnixSocketPathUnlink for Unlink {
    fn committed(self: Box<Self>) {
        self.namespace.unlink_pathname(&self.pathname, self.binding);
    }
}

impl UnixSocketPathPort for UnixSocketPaths {
    fn prepare_bind(&self, pathname: &GuestPathBytes) -> Result<Box<dyn PreparedUnixSocketPathBind>, hl_linux::Errno> {
        let entry = self.entry(pathname, true).map_err(|error| error.errno())?;
        let identity = self.host.access_identity().map_err(|error| error.errno())?;
        let metadata = super::attribute::Descriptor::new(entry.parent.as_raw_fd())
            .metadata()
            .map_err(|error| error.errno())?;
        let access = Access::from_bits(Access::WRITE | Access::EXECUTE).map_err(|_| hl_linux::Errno::EINVAL)?;
        identity
            .check_access(&metadata, access)
            .map_err(|_| hl_linux::Errno::EACCES)?;
        let result = unsafe {
            libc::mknodat(
                entry.parent.as_raw_fd(),
                entry.name.as_ptr(),
                libc::S_IFSOCK | self.context.apply(0o777),
                0,
            )
        };
        if result != 0 {
            return Err(HostError::map(std::io::Error::last_os_error()).errno());
        }
        let inode = match entry.inode() {
            Ok(inode) => inode,
            Err(error) => {
                unsafe {
                    libc::unlinkat(entry.parent.as_raw_fd(), entry.name.as_ptr(), 0);
                }
                return Err(error.errno());
            }
        };
        Ok(Box::new(Bind { entry, inode }))
    }

    fn prepare_unlink(&self, pathname: &GuestPathBytes) -> Option<Box<dyn PreparedUnixSocketPathUnlink>> {
        let binding = self.namespace.pathname_binding(pathname.as_bytes())?;
        Some(Box::new(Unlink {
            namespace: Arc::clone(&self.namespace),
            pathname: pathname.as_bytes().to_vec(),
            binding,
        }))
    }
}
