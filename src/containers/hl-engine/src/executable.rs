//! Descriptor-backed authority for one resolved guest executable.
//!
//! This capability is deliberately separate from the serializable runtime plan:
//! the plan describes the guest path, while this value owns the inode selected by
//! the container filesystem resolver. Transfer uses `SCM_RIGHTS`, so an isolated
//! worker receives that same open file rather than reopening a host pathname.

#![allow(unsafe_code)]
#![cfg_attr(not(test), allow(dead_code))]

use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct ExecutableAuthority {
    descriptor: Arc<OwnedFd>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransferError {
    Control,
    Descriptor,
}

impl ExecutableAuthority {
    #[must_use]
    pub fn new(descriptor: OwnedFd) -> Self {
        Self {
            descriptor: Arc::new(descriptor),
        }
    }

    #[must_use]
    pub fn descriptor(&self) -> std::os::fd::BorrowedFd<'_> {
        self.descriptor.as_fd()
    }

    pub(crate) fn send_optional(authority: Option<&Self>, socket: &UnixStream) -> Result<(), TransferError> {
        let byte = [u8::from(authority.is_some())];
        let mut vector = libc::iovec {
            iov_base: byte.as_ptr().cast_mut().cast(),
            iov_len: byte.len(),
        };
        let mut control = ControlBuffer::default();
        // SAFETY: `msghdr` is a C aggregate whose all-zero representation is a
        // valid empty message; the required buffer fields are populated below.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.0.as_mut_ptr().cast();
        message.msg_controllen = control.0.len();
        // SAFETY: the control buffer is aligned and sized for one descriptor;
        // the header and data remain live until sendmsg returns.
        unsafe {
            let header = libc::CMSG_FIRSTHDR(&raw const message);
            if header.is_null() {
                return Err(TransferError::Control);
            }
            let Some(authority) = authority else {
                message.msg_control = std::ptr::null_mut();
                message.msg_controllen = 0;
                return (libc::sendmsg(socket.as_raw_fd(), &raw const message, libc::MSG_NOSIGNAL) == 1)
                    .then_some(())
                    .ok_or(TransferError::Control);
            };
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as _) as usize;
            libc::CMSG_DATA(header)
                .cast::<i32>()
                .write(authority.descriptor.as_raw_fd());
            message.msg_controllen = libc::CMSG_SPACE(size_of::<i32>() as _) as usize;
            (libc::sendmsg(socket.as_raw_fd(), &raw const message, libc::MSG_NOSIGNAL) == 1)
                .then_some(())
                .ok_or(TransferError::Control)
        }
    }

    pub(crate) fn receive_optional(socket: &UnixStream) -> Result<Option<Self>, TransferError> {
        let mut byte = [0_u8];
        let mut vector = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: byte.len(),
        };
        let mut control = ControlBuffer::default();
        // SAFETY: `msghdr` is a C aggregate whose all-zero representation is a
        // valid empty message; the writable buffers are installed below.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut vector;
        message.msg_iovlen = 1;
        message.msg_control = control.0.as_mut_ptr().cast();
        message.msg_controllen = control.0.len();
        // SAFETY: all buffers described by message are writable for this call.
        let received = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut message, libc::MSG_CMSG_CLOEXEC) };
        if received != 1 || message.msg_flags & (libc::MSG_CTRUNC | libc::MSG_TRUNC) != 0 {
            return Err(TransferError::Control);
        }
        if byte[0] == 0 {
            return (message.msg_controllen == 0)
                .then_some(None)
                .ok_or(TransferError::Descriptor);
        }
        if byte[0] != 1 {
            return Err(TransferError::Control);
        }
        // SAFETY: recvmsg initialized ancillary headers within the aligned buffer.
        let header = unsafe { libc::CMSG_FIRSTHDR(&raw const message) };
        // SAFETY: the null check precedes dereferencing `header`; `recvmsg`
        // initialized the header in the live control buffer, and `CMSG_LEN`
        // only computes the required record length.
        let valid_descriptor_header = unsafe {
            !header.is_null()
                && (*header).cmsg_level == libc::SOL_SOCKET
                && (*header).cmsg_type == libc::SCM_RIGHTS
                && (*header).cmsg_len == libc::CMSG_LEN(size_of::<i32>() as _) as usize
        };
        if !valid_descriptor_header {
            return Err(TransferError::Descriptor);
        }
        // SAFETY: the validated SCM_RIGHTS record contains exactly one newly
        // owned descriptor, with close-on-exec requested from recvmsg.
        let descriptor = unsafe { libc::CMSG_DATA(header).cast::<i32>().read() };
        if descriptor < 0 {
            return Err(TransferError::Descriptor);
        }
        // SAFETY: a successful SCM_RIGHTS receive transfers ownership of this
        // new descriptor to the process, and no other `OwnedFd` wraps it here.
        Ok(Some(Self::new(unsafe { OwnedFd::from_raw_fd(descriptor) })))
    }
}

#[repr(C, align(8))]
struct ControlBuffer([u8; 64]);

impl Default for ControlBuffer {
    fn default() -> Self {
        Self([0; 64])
    }
}

#[cfg(test)]
mod tests {
    use super::{ExecutableAuthority, TransferError};
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    #[test]
    fn transfer_preserves_the_open_inode_after_path_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("guest");
        std::fs::write(&path, b"selected").unwrap();
        let selected = std::fs::File::open(&path).unwrap();
        let authority = ExecutableAuthority::new(selected.into());
        let replacement = directory.path().join("replacement");
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let (sender, receiver) = UnixStream::pair().unwrap();

        ExecutableAuthority::send_optional(Some(&authority), &sender).unwrap();
        let received = ExecutableAuthority::receive_optional(&receiver).unwrap().unwrap();
        let mut file = std::fs::File::from(received.descriptor().try_clone_to_owned().unwrap());
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();

        assert_eq!(bytes, b"selected");
    }

    #[test]
    fn received_descriptor_is_close_on_exec() {
        let file = tempfile::tempfile().unwrap();
        let authority = ExecutableAuthority::new(file.into());
        let (sender, receiver) = UnixStream::pair().unwrap();
        ExecutableAuthority::send_optional(Some(&authority), &sender).unwrap();
        let received = ExecutableAuthority::receive_optional(&receiver).unwrap().unwrap();

        // SAFETY: F_GETFD only observes the live descriptor.
        let flags = unsafe { libc::fcntl(received.descriptor().as_raw_fd(), libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }

    #[test]
    fn payload_without_descriptor_is_rejected() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender.write_all(&[1]).unwrap();
        assert_eq!(
            ExecutableAuthority::receive_optional(&receiver).unwrap_err(),
            TransferError::Descriptor
        );
    }

    #[test]
    fn absent_authority_round_trips_without_a_descriptor() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        ExecutableAuthority::send_optional(None, &sender).unwrap();
        assert!(ExecutableAuthority::receive_optional(&receiver).unwrap().is_none());
    }

    #[test]
    fn failed_transfer_retains_the_owned_descriptor() {
        use std::io::Seek as _;

        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"still-owned").unwrap();
        file.rewind().unwrap();
        let authority = ExecutableAuthority::new(file.into());
        let (sender, receiver) = UnixStream::pair().unwrap();
        drop(receiver);

        assert_eq!(
            ExecutableAuthority::send_optional(Some(&authority), &sender),
            Err(TransferError::Control)
        );
        let mut retained = std::fs::File::from(authority.descriptor().try_clone_to_owned().unwrap());
        let mut bytes = Vec::new();
        retained.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"still-owned");
    }
}
