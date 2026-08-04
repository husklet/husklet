#![allow(unsafe_code)]

use std::os::fd::AsRawFd;
use std::os::unix::fs::FileExt;

use hl_descriptor::ObjectError;

pub(super) struct Materialization;

impl Materialization {
    const MAXIMUM: u64 = 1024 * 1024 * 1024;

    pub(super) fn copy(
        size: u64,
        mut read: impl FnMut(u64, &mut [u8]) -> Result<usize, ObjectError>,
    ) -> Result<std::fs::File, ObjectError> {
        if size > Self::MAXIMUM {
            return Err(ObjectError::ResourceLimit);
        }
        let file = crate::ffi::linux::abi::Memfd::create(c"hl-projected").map_err(|()| ObjectError::Io)?;
        file.set_len(size).map_err(|_| ObjectError::Io)?;
        let mut offset = 0_u64;
        let mut chunk = vec![0_u8; hl_provider::TreeWire::MAX_DATA];
        while offset < size {
            let limit =
                usize::try_from((size - offset).min(chunk.len() as u64)).map_err(|_| ObjectError::InvalidArgument)?;
            let count = read(offset, &mut chunk[..limit])?;
            if count == 0 {
                return Err(ObjectError::Io);
            }
            Self::write(&file, &chunk[..count], offset)?;
            offset = offset.checked_add(count as u64).ok_or(ObjectError::InvalidArgument)?;
        }
        // SAFETY: fcntl retains no pointer; this private descriptor is fully
        // initialized and not published until immutable seals succeed.
        if unsafe { crate::ffi::linux::abi::fcntl(file.as_raw_fd(), 1033, 15) } != 0 {
            return Err(ObjectError::Io);
        }
        Ok(file)
    }

    fn write(file: &std::fs::File, bytes: &[u8], offset: u64) -> Result<(), ObjectError> {
        let mut written = 0;
        while written < bytes.len() {
            let count = file
                .write_at(&bytes[written..], offset + written as u64)
                .map_err(|_| ObjectError::Io)?;
            if count == 0 {
                return Err(ObjectError::Io);
            }
            written += count;
        }
        Ok(())
    }
}
