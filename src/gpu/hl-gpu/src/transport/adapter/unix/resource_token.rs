//! Guest-local sealed `OPAQUE_FD` carrier for a host resource export identity.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use crate::ExportId;

const MAGIC: [u8; 8] = *b"HLRESFD\0";
const VERSION: u32 = 1;
const BYTES: usize = 24;
const REQUIRED_SEALS: libc::c_int =
    libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;

/// An owned immutable descriptor containing one buffer export identity.
pub struct OpaqueResourceFd {
    fd: OwnedFd,
}

impl OpaqueResourceFd {
    pub fn create(id: ExportId) -> io::Result<Self> {
        if id.0 == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "zero resource export id",
            ));
        }
        let raw = unsafe {
            libc::memfd_create(
                b"hl-resource\0".as_ptr().cast(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut file = File::from(fd);
        let mut bytes = [0u8; BYTES];
        bytes[..8].copy_from_slice(&MAGIC);
        bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
        bytes[16..24].copy_from_slice(&id.0.to_le_bytes());
        file.write_all(&bytes)?;
        file.flush()?;
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd: file.into() })
    }

    pub fn from_owned(fd: OwnedFd) -> io::Result<Self> {
        let token = Self { fd };
        token.id()?;
        Ok(token)
    }

    pub fn id(&self) -> io::Result<ExportId> {
        let seals = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 || seals & REQUIRED_SEALS != REQUIRED_SEALS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsealed resource fd",
            ));
        }
        let mut file = File::from(self.fd.try_clone()?);
        if file.metadata()?.len() != BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wrong resource token length",
            ));
        }
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = [0u8; BYTES];
        file.read_exact(&mut bytes)?;
        if bytes[..8] != MAGIC || bytes[8..12] != VERSION.to_le_bytes() || bytes[12..16] != [0; 4] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid resource token header",
            ));
        }
        let id = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        if id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "zero resource export id",
            ));
        }
        Ok(ExportId(id))
    }

    pub fn consume(self) -> io::Result<ExportId> {
        self.id()
    }
    pub fn as_raw_fd(&self) -> libc::c_int {
        self.fd.as_raw_fd()
    }
    pub fn into_raw_fd(self) -> libc::c_int {
        self.fd.into_raw_fd()
    }
}
