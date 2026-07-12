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
        use rand::{distr::Alphanumeric, Rng};
        use rustix::{
            io::Errno,
            shm::{self, Mode},
        };

        let mut rng = rand::rng();

        // dd patch (offline-vendored smithay 0.7.0) — two macOS-specific fixes so this non-Linux path
        // (which upstream only ever exercised on FreeBSD, and FreeBSD takes the memfd branch above)
        // actually works on the macOS host `dd-compositor` runs on:
        //
        // (1) NAME LENGTH. macOS caps POSIX shared-memory object names at `PSHMNAMLEN` (31) bytes — XNU
        //     `pshm_open` rejects a longer name with `ENAMETOOLONG`. Smithay's callers pass descriptive
        //     names such as `smithay-dmabuffeedback-format-table` (35 bytes); appending "-" + 7 random
        //     chars pushes that to ~43 bytes and every `shm_open` fails, so the `zwp_linux_dmabuf` v4
        //     feedback global cannot stand up. Keep the object name inside the limit: a portable leading
        //     slash + a truncated descriptive prefix + "-" + 7 random chars, capped at 30 bytes.
        //
        // (2) POPULATION. A macOS POSIX shm object is `ftruncate`+`mmap` only — it has size 0 until
        //     `ftruncate` and does NOT support `write()`/`read()` (a bare `write_all` fails with ENXIO,
        //     "Device not configured"). Size it with `ftruncate` and copy the data through a temporary
        //     writable `mmap` (the exact portable trick `dd-display`'s keymap `anon_fd_with` already uses
        //     on macOS), then hand back the read-only re-opened fd.
        //
        // The name is `shm_unlink`ed right after creation, so it is only briefly visible.
        // (`AsRawFd` for the writable fd is already imported at module scope.)
        const PSHM_MAX: usize = 30; // one byte under macOS PSHMNAMLEN (31)
        const SUFFIX_LEN: usize = 1 /* '-' */ + 7 /* random */;
        let base = name.to_bytes();
        let keep = base.len().min(PSHM_MAX.saturating_sub(1 /* leading '/' */ + SUFFIX_LEN));

        // `memfd_create` isn't available. Instead, try `shm_open` with a randomized name, and
        // loop a couple times if it exists.
        let mut n = 0;
        let (shm_name, fd_rdwr) = loop {
            let mut shm_name = Vec::with_capacity(PSHM_MAX);
            shm_name.push(b'/');
            shm_name.extend_from_slice(&base[..keep]);
            shm_name.push(b'-');
            shm_name.extend((0..7).map(|_| rng.sample(Alphanumeric)));
            let fd = shm::open(
                shm_name.as_slice(),
                shm::OFlags::RDWR | shm::OFlags::CREATE | shm::OFlags::EXCL,
                Mode::RWXU,
            );
            if !matches!(fd, Err(Errno::EXIST)) || n > 3 {
                break (shm_name, fd?);
            }
            n += 1;
        };

        // Size the object, then copy `data` in through a writable mmap (macOS shm has no `write()`).
        let raw = fd_rdwr.as_raw_fd();
        if unsafe { libc::ftruncate(raw, data.len() as libc::off_t) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if !data.is_empty() {
            unsafe {
                let ptr = libc::mmap(
                    std::ptr::null_mut(),
                    data.len(),
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    raw,
                    0,
                );
                if ptr == libc::MAP_FAILED {
                    return Err(std::io::Error::last_os_error());
                }
                std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
                libc::munmap(ptr, data.len());
            }
        }

        // Sealing isn't available, so re-open read-only (the view handed to clients), then unlink the
        // name. The object persists via the open fds; the writable `fd_rdwr` is dropped on return.
        let fd_rdonly = shm::open(shm_name.as_slice(), shm::OFlags::RDONLY, Mode::empty())?;
        let file_rdonly = File::from(fd_rdonly);

        // Unlink so another process can't open shm file.
        let _ = shm::unlink(shm_name.as_slice());

        Ok(Self {
            file: file_rdonly,
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
