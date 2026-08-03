//! Guest-local `OPAQUE_FD` carrier for a host synchronization export identity.
//!
//! Vulkan and CUDA run in separate processes in one Linux guest. The descriptor therefore carries a
//! sealed value between those guest processes; it is never sent over the GPU socket. Only the decoded
//! [`SyncExportId`] crosses the existing framed protocol later.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};

use crate::SyncExportId;

const MAGIC: [u8; 8] = *b"HLSYNCFD";
const VERSION: u32 = 1;
const BYTES: usize = 40;
const REQUIRED_SEALS: libc::c_int =
    libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;

/// An owned, close-on-drop Linux descriptor containing one immutable synchronization export identity.
pub struct OpaqueSyncFd {
    fd: OwnedFd,
}

impl OpaqueSyncFd {
    /// Create the Vulkan-export side of a guest-local opaque-fd handle.
    pub fn create(id: SyncExportId) -> io::Result<Self> {
        let name = b"hl-sync\0";
        // SAFETY: `name` is NUL terminated and the returned descriptor is uniquely owned on success.
        let raw = unsafe {
            libc::memfd_create(
                name.as_ptr().cast(),
                libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `memfd_create` returned a fresh descriptor owned by this function.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let mut file = File::from(fd);
        file.write_all(&encode(id))?;
        file.flush()?;
        let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, REQUIRED_SEALS) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd: file.into() })
    }

    /// Validate and take ownership of an application-supplied CUDA descriptor.
    pub fn from_owned(fd: OwnedFd) -> io::Result<Self> {
        let token = Self { fd };
        token.id()?;
        Ok(token)
    }

    /// Duplicate the kernel reference. Each returned value closes exactly its own descriptor on drop.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            fd: self.fd.try_clone()?,
        })
    }

    /// Decode without consuming this reference.
    pub fn id(&self) -> io::Result<SyncExportId> {
        let seals = unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GET_SEALS) };
        if seals < 0 {
            return Err(io::Error::last_os_error());
        }
        if seals & REQUIRED_SEALS != REQUIRED_SEALS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsealed synchronization fd",
            ));
        }
        let mut file = File::from(self.fd.try_clone()?);
        if file.metadata()?.len() != BYTES as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "wrong synchronization token length",
            ));
        }
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = [0u8; BYTES];
        file.read_exact(&mut bytes)?;
        decode(bytes)
    }

    /// CUDA-style successful import: decode, then consume and close the supplied descriptor.
    pub fn consume(self) -> io::Result<SyncExportId> {
        self.id()
    }

    pub fn as_raw_fd(&self) -> libc::c_int {
        self.fd.as_raw_fd()
    }

    /// Transfer the descriptor to an external ABI without closing it.
    pub fn into_raw_fd(self) -> libc::c_int {
        self.fd.into_raw_fd()
    }
}

fn encode(id: SyncExportId) -> [u8; BYTES] {
    let mut bytes = [0u8; BYTES];
    bytes[..8].copy_from_slice(&MAGIC);
    bytes[8..12].copy_from_slice(&VERSION.to_le_bytes());
    bytes[16..24].copy_from_slice(&id.serial().to_le_bytes());
    bytes[24..40].copy_from_slice(&id.authenticity().to_le_bytes());
    bytes
}

fn decode(bytes: [u8; BYTES]) -> io::Result<SyncExportId> {
    if bytes[..8] != MAGIC || bytes[8..12] != VERSION.to_le_bytes() || bytes[12..16] != [0; 4] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid synchronization token header",
        ));
    }
    let serial = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    let authenticity = u128::from_le_bytes(bytes[24..40].try_into().unwrap());
    if serial == 0 || authenticity == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid synchronization identity",
        ));
    }
    Ok(SyncExportId::from_parts(serial, authenticity))
}
