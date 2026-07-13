use std::{
    ffi::CStr,
    fs::File,
    io::Write,
    os::unix::io::{AsFd, AsRawFd, BorrowedFd, RawFd},
};

/// A file whose fd cannot be written by other processes
///
/// This mechanism is useful for giving clients access to large amounts of
/// information such as keymaps without them being able to write to the handle.
///
/// On Linux, Android, and FreeBSD, this uses a sealed memfd. On other platforms
/// it creates a POSIX shared memory object with `shm_open`, opens a read-only
/// copy, and unlinks it.
#[derive(Debug)]
pub struct SealedFile {
    file: File,
    size: usize,
}

impl SealedFile {
    /// Create a `[SealedFile]` with the given nul-terminated C string.
    pub fn with_content(name: &CStr, contents: &CStr) -> Result<Self, std::io::Error> {
        Self::with_data(name, contents.to_bytes_with_nul())
    }

    /// Create a `[SealedFile]` with the given binary data.
    #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "android"))]
    pub fn with_data(name: &CStr, data: &[u8]) -> Result<Self, std::io::Error> {
        use rustix::fs::{MemfdFlags, SealFlags};
        use std::io::Seek;

        let fd = rustix::fs::memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;

        let mut file: File = fd.into();
        file.write_all(data)?;
        file.flush()?;

        file.seek(std::io::SeekFrom::Start(0))?;

        rustix::fs::fcntl_add_seals(
            &file,
            SealFlags::SEAL | SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE,
        )?;

        Ok(Self {
            file,
            size: data.len(),
        })
    }

    /// Create a `[SealedFile]` with the given binary data.
    #[cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "android")))]
    pub fn with_data(name: &CStr, data: &[u8]) -> Result<Self, std::io::Error> {
        use std::os::unix::io::FromRawFd;

        // dd patch (offline-vendored smithay 0.7.0) — macOS SealedFile backing.
        //
        // Upstream backs this non-Linux path with a POSIX `shm_open` object (FreeBSD takes the memfd
        // branch above, so macOS is the only consumer). That fd is NOT usable the way the Wayland
        // protocols that carry a SealedFile require: a client maps the payload with
        // `mmap(.., PROT_READ, MAP_PRIVATE, fd, 0)` — the keymap and the `zwp_linux_dmabuf` v4 feedback
        // format-table are both mapped `MAP_PRIVATE` — and **macOS rejects `MAP_PRIVATE` on a POSIX shm
        // descriptor with `EINVAL`**. The `gui_dmabuf_feedback_guest` bridge probe proves it end to end:
        // the SCM_RIGHTS fd, the table size, and the 8-byte Linux dev_t all arrive intact, then the
        // client's `mmap` fails with errno 22, so no real guest can read the advertised format table.
        //
        // A regular file fd DOES support cross-process `MAP_PRIVATE PROT_READ`. Back the sealed file
        // with an unlinked temp file: create it read-write, write the payload (regular files support
        // `write()`, unlike a macOS shm object), then hand clients a re-opened `O_RDONLY` fd and unlink
        // the path. The seal still holds — the fd is read-only so a client cannot write the shared
        // bytes, and its `MAP_PRIVATE` mapping is copy-on-write — while the fd is now actually mappable.
        let _ = name;
        let dir = std::env::var_os("TMPDIR")
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "/tmp".into());
        let mut templ: Vec<u8> =
            format!("{}/smithay-sealed-XXXXXX", dir.trim_end_matches('/')).into_bytes();
        templ.push(0); // NUL terminator that mkstemp overwrites the X's within, in place

        let fd_rdwr = unsafe { libc::mkstemp(templ.as_mut_ptr() as *mut libc::c_char) };
        if fd_rdwr < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // `templ` now holds the concrete NUL-terminated path. Populate through the read-write fd, then
        // close it (the path still exists until we unlink below).
        {
            let mut file_rdwr = unsafe { File::from_raw_fd(fd_rdwr) };
            file_rdwr.write_all(data)?;
            file_rdwr.flush()?;
        }

        // Re-open read-only (the handle handed to clients), then unlink the name. The inode persists via
        // the open fd; the read-only fd cannot be used to write the shared bytes (the seal).
        let path = templ.as_ptr() as *const libc::c_char;
        let fd_rdonly = unsafe { libc::open(path, libc::O_RDONLY) };
        unsafe { libc::unlink(path) };
        if fd_rdonly < 0 {
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self {
            file: unsafe { File::from_raw_fd(fd_rdonly) },
            size: data.len(),
        })
    }

    /// Size of the data contained in the sealed file.
    pub fn size(&self) -> usize {
        self.size
    }
}

impl AsRawFd for SealedFile {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

impl AsFd for SealedFile {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.file.as_fd()
    }
}
