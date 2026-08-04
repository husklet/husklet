use super::{ErrnoMapper, abi as libc};
use crate::native_host::{DirectoryEntry, DirectoryEntryKind, HostError};

pub(super) fn directory_next(
    file: u64,
    cookie: u64,
    name: &mut [u8],
) -> Result<Option<(DirectoryEntry, usize)>, HostError> {
    let descriptor = i32::try_from(file).map_err(|_| HostError::Invalid)?;
    let cookie = i64::try_from(cookie).map_err(|_| HostError::Invalid)?;
    // SAFETY: lseek operates only on the owned directory descriptor and retains nothing.
    if unsafe { libc::lseek(descriptor, cookie, libc::SEEK_SET) } < 0 {
        return Err(ErrnoMapper::current());
    }
    let mut buffer = [0_u8; 4096];
    // SAFETY: buffer is valid for its full length; getdents64 writes bounded records.
    let count = unsafe { libc::syscall(libc::SYS_getdents64, descriptor, buffer.as_mut_ptr(), buffer.len()) };
    if count < 0 {
        return Err(ErrnoMapper::current());
    }
    if count == 0 {
        return Ok(None);
    }
    const HEADER: usize = 19;
    if count < HEADER as i64 {
        return Err(HostError::Failed);
    }
    let inode = u64::from_ne_bytes(buffer[0..8].try_into().map_err(|_| HostError::Failed)?);
    let offset = i64::from_ne_bytes(buffer[8..16].try_into().map_err(|_| HostError::Failed)?);
    let record = usize::from(u16::from_ne_bytes(
        buffer[16..18].try_into().map_err(|_| HostError::Failed)?,
    ));
    if record < HEADER || record > count as usize {
        return Err(HostError::Failed);
    }
    let bytes = &buffer[HEADER..record];
    let length = bytes.iter().position(|byte| *byte == 0).ok_or(HostError::Failed)?;
    if length > name.len() {
        return Err(HostError::Exhausted);
    }
    name[..length].copy_from_slice(&bytes[..length]);
    let kind = match buffer[18] {
        libc::DT_REG => DirectoryEntryKind::File,
        libc::DT_DIR => DirectoryEntryKind::Directory,
        libc::DT_LNK => DirectoryEntryKind::Symlink,
        _ => DirectoryEntryKind::Other,
    };
    Ok(Some((
        DirectoryEntry {
            cookie: u64::try_from(offset).map_err(|_| HostError::Failed)?,
            inode,
            kind,
        },
        length,
    )))
}
