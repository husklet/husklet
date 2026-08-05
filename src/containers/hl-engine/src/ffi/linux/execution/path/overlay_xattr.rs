use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

pub(super) fn copy(source: &File, target: &File) -> io::Result<()> {
    let size = list(source, std::ptr::null_mut(), 0);
    if size < 0 {
        return Ok(());
    }
    let mut names = vec![0_u8; usize::try_from(size).expect("xattr list size fits usize")];
    let count = list(source, names.as_mut_ptr().cast(), names.len());
    if count < 0 {
        return Err(io::Error::last_os_error());
    }
    names.truncate(usize::try_from(count).expect("xattr list count fits usize"));
    for bytes in names.split_inclusive(|byte| *byte == 0).filter(|bytes| bytes.last() == Some(&0)) {
        let name = CStr::from_bytes_with_nul(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EIO))?;
        let size = get(source, name, std::ptr::null_mut(), 0);
        if size < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut value = vec![0_u8; usize::try_from(size).expect("xattr value size fits usize")];
        let count = get(source, name, value.as_mut_ptr().cast(), value.len());
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        value.truncate(usize::try_from(count).expect("xattr value count fits usize"));
        if set(target, name, &value) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn list(file: &File, output: *mut libc::c_char, size: usize) -> isize {
    // SAFETY: the caller provides either null or writable storage for size.
    unsafe { libc::flistxattr(file.as_raw_fd(), output, size) }
}

#[cfg(target_os = "macos")]
fn list(file: &File, output: *mut libc::c_char, size: usize) -> isize {
    // SAFETY: the caller provides either null or writable storage for size.
    unsafe { libc::flistxattr(file.as_raw_fd(), output, size, 0) }
}

#[cfg(target_os = "linux")]
fn get(file: &File, name: &CStr, output: *mut libc::c_void, size: usize) -> isize {
    // SAFETY: name is terminated and output is null or writable for size.
    unsafe { libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), output, size) }
}

#[cfg(target_os = "macos")]
fn get(file: &File, name: &CStr, output: *mut libc::c_void, size: usize) -> isize {
    // SAFETY: name is terminated and output is null or writable for size.
    unsafe { libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), output, size, 0, 0) }
}

#[cfg(target_os = "linux")]
fn set(file: &File, name: &CStr, value: &[u8]) -> i32 {
    // SAFETY: file and name live and value is readable for its length.
    unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    }
}

#[cfg(target_os = "macos")]
fn set(file: &File, name: &CStr, value: &[u8]) -> i32 {
    // SAFETY: file and name live and value is readable for its length.
    unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    }
}
