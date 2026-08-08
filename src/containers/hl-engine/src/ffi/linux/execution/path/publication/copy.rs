//! Byte and metadata transfer for one overlay copy-up.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

const COPY_BUFFER_SIZE: usize = 64 * 1024;

pub(super) fn copy_content(source: &File, target: &File) -> io::Result<()> {
    let mut offset = 0_i64;
    let mut buffer = vec![0_u8; COPY_BUFFER_SIZE];
    loop {
        // SAFETY: buffer is writable, both descriptors remain owned, and pread
        // does not alter the source open-file-description offset.
        let count = unsafe { libc::pread(source.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len(), offset) };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            return Ok(());
        }
        let count = usize::try_from(count).expect("positive read count fits usize");
        write_all(target, &buffer[..count])?;
        offset = offset
            .checked_add(i64::try_from(count).expect("read count fits offset"))
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EFBIG))?;
    }
}

fn write_all(target: &File, data: &[u8]) -> io::Result<()> {
    let mut written = 0;
    while written < data.len() {
        // SAFETY: the unwritten buffer suffix is readable and target lives.
        let result = unsafe {
            libc::write(
                target.as_raw_fd(),
                data[written..].as_ptr().cast(),
                data.len() - written,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        if result == 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }
        written += usize::try_from(result).expect("positive write count fits usize");
    }
    Ok(())
}

pub(super) fn copy_metadata(source: &File, target: &File) -> io::Result<()> {
    let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat initializes status and retains no descriptor.
    if unsafe { libc::fstat(source.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized status.
    let status = unsafe { status.assume_init() };
    // SAFETY: target is owned and fchmod retains nothing.
    if unsafe { libc::fchmod(target.as_raw_fd(), status.st_mode & 0o7777) } != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(target_os = "linux")]
    let times = [
        libc::timespec {
            tv_sec: status.st_atime,
            tv_nsec: status.st_atime_nsec,
        },
        libc::timespec {
            tv_sec: status.st_mtime,
            tv_nsec: status.st_mtime_nsec,
        },
    ];
    #[cfg(target_os = "macos")]
    let times = [status.st_atimespec, status.st_mtimespec];
    // SAFETY: times has exactly two initialized entries and target is live.
    if unsafe { libc::futimens(target.as_raw_fd(), times.as_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    super::super::overlay_xattr::copy(source, target)
}
