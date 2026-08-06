#![allow(unsafe_code)]

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::net::UnixDatagram;
use std::os::unix::net::UnixStream;

pub(crate) struct InheritedStream;
pub(crate) struct InheritedFile;
pub(crate) struct PinnedRoot;

impl InheritedStream {
    /// Adopts one descriptor inherited by the hidden authority child.
    pub(crate) fn adopt(descriptor: i32) -> Result<UnixStream, ()> {
        if descriptor < 0 {
            return Err(());
        }
        // SAFETY: the hidden child receives this descriptor through one explicit
        // spawn inheritance action and transfers it exactly once to this owner.
        Ok(unsafe { UnixStream::from_raw_fd(descriptor) })
    }

    pub(crate) fn wait_closed(stream: &UnixStream) -> Result<(), ()> {
        let mut event = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLHUP | libc::POLLERR,
            revents: 0,
        };
        loop {
            // SAFETY: poll receives one valid pollfd for the duration of this call.
            let result = unsafe { libc::poll(&raw mut event, 1, -1) };
            if result > 0 && event.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(());
            }
            if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(());
        }
    }
}

pub(crate) struct InheritedDatagram;
pub(crate) struct InheritedListener;

impl InheritedDatagram {
    pub(crate) fn adopt(descriptor: i32) -> Result<UnixDatagram, ()> {
        if descriptor < 0 {
            return Err(());
        }
        // SAFETY: the inherited descriptor is transferred exactly once into this owner.
        Ok(unsafe { UnixDatagram::from_raw_fd(descriptor) })
    }
}

impl InheritedListener {
    pub(crate) fn adopt(descriptor: i32) -> Result<File, ()> {
        if descriptor < 0 {
            return Err(());
        }
        let mut accepting = 0_i32;
        let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
        // SAFETY: output scalar and exact length are valid for this non-retaining query.
        let valid = unsafe {
            libc::getsockopt(
                descriptor,
                libc::SOL_SOCKET,
                libc::SO_ACCEPTCONN,
                (&raw mut accepting).cast(),
                &raw mut length,
            ) == 0
                && accepting == 1
        };
        if !valid {
            // SAFETY: the received right is owned here and rejected exactly once.
            unsafe {
                libc::close(descriptor);
            }
            return Err(());
        }
        // SAFETY: the validated received right is transferred exactly once.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

impl InheritedFile {
    pub(crate) fn adopt(descriptor: i32) -> Result<File, ()> {
        if descriptor < 0 {
            return Err(());
        }
        // SAFETY: the authority child receives this descriptor through one
        // explicit inheritance action and transfers it exactly once here.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

impl PinnedRoot {
    pub(crate) fn open(root: &File, path: &[u8], options: hl_provider::TreeOpen, writable: bool) -> Result<File, i32> {
        use std::os::unix::ffi::OsStrExt;
        if path.is_empty() || path.len() > 4096 || path.contains(&0) || !path.starts_with(b"/") {
            return Err(libc::EINVAL);
        }
        let relative = path.strip_prefix(b"/").unwrap_or(path);
        let relative = if relative.is_empty() { b".".as_slice() } else { relative };
        let name =
            std::ffi::CString::new(std::ffi::OsStr::from_bytes(relative).as_bytes()).map_err(|_| libc::EINVAL)?;
        if !writable && (options.write || options.create || options.truncate || options.append) {
            return Err(libc::EROFS);
        }
        let access = match (options.read, options.write) {
            (true, true) => libc::O_RDWR,
            (false, true) => libc::O_WRONLY,
            _ => libc::O_RDONLY,
        };
        let mut flags = match options.kind {
            hl_provider::TreeKind::File => access | libc::O_CLOEXEC,
            hl_provider::TreeKind::Directory => libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY,
            hl_provider::TreeKind::Link => libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        };
        if options.create {
            flags |= libc::O_CREAT;
        }
        if options.truncate {
            flags |= libc::O_TRUNC;
        }
        if options.append {
            flags |= libc::O_APPEND;
        }
        if options.exclusive {
            flags |= libc::O_EXCL;
        }
        let how = super::OpenHow {
            flags: flags as u64,
            mode: u64::from(options.mode & 0o7777),
            resolve: 0x10 | 0x02 | 0x01,
        };
        // SAFETY: root and name remain live, open_how has the kernel ABI shape,
        // and openat2 copies all inputs without retaining pointers.
        let descriptor = unsafe {
            libc::syscall(
                437_i64,
                root.as_raw_fd(),
                name.as_ptr(),
                &raw const how,
                std::mem::size_of::<super::OpenHow>(),
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        let descriptor = i32::try_from(descriptor).map_err(|_| libc::EIO)?;
        // SAFETY: openat2 returned one new owned descriptor.
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    pub(crate) fn guest(root: &File, file: &File) -> Result<Vec<u8>, i32> {
        let path = |descriptor: i32| -> Result<Vec<u8>, i32> {
            let name = std::ffi::CString::new(format!("/proc/self/fd/{descriptor}")).map_err(|_| libc::EINVAL)?;
            let mut output = vec![0_u8; 4096];
            // SAFETY: name is terminated, output is uniquely writable, and
            // readlink retains neither pointer.
            let count = unsafe { libc::readlink(name.as_ptr(), output.as_mut_ptr().cast(), output.len()) };
            if count < 0 {
                return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
            }
            output.truncate(usize::try_from(count).map_err(|_| libc::EIO)?);
            Ok(output)
        };
        let root = path(root.as_raw_fd())?;
        let node = path(file.as_raw_fd())?;
        let relative = node.strip_prefix(root.as_slice()).ok_or(libc::EACCES)?;
        if !relative.is_empty() && relative[0] != b'/' {
            return Err(libc::EACCES);
        }
        Ok(if relative.is_empty() {
            b"/".to_vec()
        } else {
            relative.to_vec()
        })
    }

    pub(crate) fn read_link(link: &File, maximum: usize) -> Result<Vec<u8>, i32> {
        let name = c"";
        let mut output = vec![0_u8; maximum];
        // SAFETY: the pinned O_PATH handle and bounded output remain live for
        // the call. Linux readlinkat with an empty path reads the link named by
        // the descriptor without re-walking any attacker-controlled ancestors.
        let count = unsafe {
            libc::readlinkat(
                link.as_raw_fd(),
                name.as_ptr(),
                output.as_mut_ptr().cast(),
                output.len(),
            )
        };
        if count < 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        output.truncate(usize::try_from(count).map_err(|_| libc::EIO)?);
        Ok(output)
    }

    pub(crate) fn entries(file: &File, maximum: usize) -> Result<Vec<u8>, i32> {
        let mut output = vec![0_u8; maximum];
        // SAFETY: getdents64 writes at most output.len bytes to the live buffer.
        let count = unsafe {
            libc::syscall(
                libc::SYS_getdents64,
                file.as_raw_fd(),
                output.as_mut_ptr(),
                output.len(),
            )
        };
        if count < 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(libc::EIO));
        }
        output.truncate(usize::try_from(count).map_err(|_| libc::EIO)?);
        Ok(output)
    }
}

#[cfg(test)]
mod test {
    use super::PinnedRoot;

    fn read_options(kind: hl_provider::TreeKind) -> hl_provider::TreeOpen {
        hl_provider::TreeOpen::read(kind)
    }

    #[test]
    fn pinned_root_confines_parent_and_symlink_targets() {
        let base = std::env::temp_dir().join(format!("hl-pinned-root-{}", std::process::id()));
        let root = base.join("root");
        let outside = base.join("outside");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret"), b"host-secret").unwrap();
        std::os::unix::fs::symlink("../outside/secret", root.join("relative")).unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), root.join("absolute")).unwrap();
        std::os::unix::fs::symlink("target", root.join("visible")).unwrap();
        let root_file = std::fs::File::open(&root).unwrap();
        assert!(
            PinnedRoot::open(
                &root_file,
                b"/../outside/secret",
                read_options(hl_provider::TreeKind::File),
                false,
            )
            .is_err()
        );
        assert!(
            PinnedRoot::open(
                &root_file,
                b"/relative",
                read_options(hl_provider::TreeKind::File),
                false,
            )
            .is_err()
        );
        assert!(
            PinnedRoot::open(
                &root_file,
                b"/absolute",
                read_options(hl_provider::TreeKind::File),
                false,
            )
            .is_err()
        );
        let link = PinnedRoot::open(
            &root_file,
            b"/visible",
            read_options(hl_provider::TreeKind::Link),
            false,
        )
        .unwrap();
        assert_eq!(PinnedRoot::read_link(&link, 64).unwrap(), b"target");
        std::fs::remove_dir_all(base).unwrap();
    }
}
