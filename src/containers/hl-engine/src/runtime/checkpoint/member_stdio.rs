//! The terminal a restored member reattaches to, and the one message that hands it over.
//!
//! A whole-image restore re-forks every captured process out of one launch, and `checkpoint/image.c`
//! records each member's guest fds 0..2 as `CKF_TTY` so the restore rebinds them to a live terminal. Until
//! this existed the only terminal a restored member could reach was the restoring engine's own bridge --
//! one bridge for a tree of many -- so a host holding a member individually still had no I/O to attach a
//! pane to, and refused to reattach rather than seat a session whose input went nowhere.
//!
//! The producer is necessarily the host, and necessarily BEFORE the restore: a member asks for its
//! terminal from inside its descriptor restore, long before any pane exists to ask on its behalf. So the
//! host creates one pty per sealed member it is about to revive, registers it here under the guest pid the
//! image names that member by, and starts the container. A member with no registration reads no descriptor
//! and keeps the inherited bridge, which is exactly the behaviour that preceded this.
//!
//! A registration is handed out ONCE. The host drops its own slave end with it, so the pty master sees a
//! real end-of-file when the member exits instead of being held open forever by the registry.

#![allow(unsafe_code)]

use std::{
    collections::BTreeMap,
    num::NonZeroI32,
    os::fd::{AsRawFd as _, OwnedFd, RawFd},
    os::unix::net::UnixStream,
    sync::Mutex,
};

/// Every terminal the host pre-created for a member of the restore it is about to start.
#[derive(Default)]
pub(crate) struct MemberTerminals {
    by_guest_pid: Mutex<BTreeMap<i32, OwnedFd>>,
}

impl MemberTerminals {
    /// Registers the terminal one sealed member will reattach to. Must happen before the restore starts:
    /// the member asks for it during its own descriptor restore, and an answer that arrives later is an
    /// answer to nothing.
    pub(crate) fn register(&self, guest_pid: NonZeroI32, terminal: OwnedFd) -> Result<(), &'static str> {
        self.by_guest_pid
            .lock()
            .map_err(|_| "member terminal registry is poisoned")?
            .insert(guest_pid.get(), terminal);
        Ok(())
    }

    /// Takes the terminal registered for one member, if any. Removing rather than cloning is what gives the
    /// pty master an end-of-file when the member exits, and what keeps one terminal answering exactly one
    /// member.
    pub(crate) fn take(&self, guest_pid: NonZeroI32) -> Option<OwnedFd> {
        self.by_guest_pid.lock().ok()?.remove(&guest_pid.get())
    }

    /// Drops every unclaimed registration. A fresh capture retires the tree these terminals were created
    /// for, exactly as it retires the member capabilities beside them.
    pub(crate) fn clear(&self) {
        if let Ok(mut terminals) = self.by_guest_pid.lock() {
            terminals.clear();
        }
    }
}

/// Sends `header` with `descriptor` attached over `SCM_RIGHTS`.
///
/// One `sendmsg`, because the rights travel with the message that carries the bytes: a header written
/// separately would arrive without them. The receiver reads this with `recvmsg` for the same reason.
pub(crate) fn send_with_descriptor(socket: &UnixStream, header: &[u8], descriptor: RawFd) -> std::io::Result<()> {
    let mut control = [0_u8; CONTROL_BYTES];
    let mut vector = libc::iovec {
        iov_base: header.as_ptr().cast::<libc::c_void>().cast_mut(),
        iov_len: header.len(),
    };
    // SAFETY: `message` is zeroed C POD; every pointer it is given below borrows storage owned by this
    // frame and outlives the one `sendmsg` call. Nothing here allocates, locks, or unwinds.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
    message.msg_controllen = control.len() as _;
    // SAFETY: `control` is exactly CMSG_SPACE(sizeof(int)) bytes and is described by `message`, so
    // CMSG_FIRSTHDR returns a pointer into it. The descriptor is copied by value into the control buffer;
    // the kernel duplicates it into the peer and this process keeps its own.
    unsafe {
        let control_header = libc::CMSG_FIRSTHDR(&raw const message);
        if control_header.is_null() {
            return Err(std::io::Error::other("checkpoint reply control buffer is unusable"));
        }
        (*control_header).cmsg_level = libc::SOL_SOCKET;
        (*control_header).cmsg_type = libc::SCM_RIGHTS;
        (*control_header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping(
            (&raw const descriptor).cast::<u8>(),
            libc::CMSG_DATA(control_header),
            std::mem::size_of::<RawFd>(),
        );
        message.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;
    }
    loop {
        // SAFETY: `socket` is a live stream socket for the duration of the call, and `message` describes
        // storage owned by this frame.
        let sent = unsafe { libc::sendmsg(socket.as_raw_fd(), &raw const message, 0) };
        if sent < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if sent as usize != header.len() {
            // A short send would strand the peer mid-header with the rights already installed, which is
            // unrecoverable rather than retryable: the reply framing is one message by construction.
            return Err(std::io::Error::other("checkpoint reply was sent short"));
        }
        return Ok(());
    }
}

/// `CMSG_SPACE(sizeof(int))`, stated as a constant because it sizes a stack buffer.
const CONTROL_BYTES: usize = 64;

/// The receiving half of one descriptor-passing reply, as the engine's C side spells it: `recvmsg`,
/// because a `read` would take the header and drop the rights attached to it.
///
/// Test-only. Production has exactly one receiver of these replies and it is the C engine
/// (`hl_ckpt_channel_call_receive_descriptor`); this exists so a Rust test can stand in for it and prove
/// the descriptor really crosses.
#[cfg(test)]
pub(crate) fn receive_with_descriptor(socket: &UnixStream, buffer: &mut [u8]) -> (isize, Option<OwnedFd>) {
    use std::os::fd::FromRawFd as _;

    let mut control = [0_u8; CONTROL_BYTES];
    let mut vector = libc::iovec {
        iov_base: buffer.as_mut_ptr().cast::<libc::c_void>(),
        iov_len: buffer.len(),
    };
    // SAFETY: zeroed C POD, filled in below with pointers into this frame's storage.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut vector;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast::<libc::c_void>();
    message.msg_controllen = control.len() as _;
    // SAFETY: `socket` is live and `message` describes storage owned by this frame.
    let read = unsafe { libc::recvmsg(socket.as_raw_fd(), &raw mut message, 0) };
    // SAFETY: the control buffer was filled by the kernel for exactly this `message`, and any descriptor
    // it names was installed into this process by the kernel with no other owner.
    let descriptor = unsafe {
        let control_header = libc::CMSG_FIRSTHDR(&raw const message);
        (!control_header.is_null()).then(|| {
            let mut raw: RawFd = -1;
            std::ptr::copy_nonoverlapping(
                libc::CMSG_DATA(control_header),
                (&raw mut raw).cast::<u8>(),
                std::mem::size_of::<RawFd>(),
            );
            OwnedFd::from_raw_fd(raw)
        })
    };
    (read, descriptor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd as _;

    /// The registry answers one member and exactly one member. A second read of the same guest pid must
    /// find nothing, because the terminal it named has been handed to the process that asked for it.
    #[test]
    fn a_registered_terminal_is_handed_out_once() {
        let terminals = MemberTerminals::default();
        let (terminal, _peer) = UnixStream::pair().expect("terminal stand-in");
        let guest_pid = NonZeroI32::new(9).expect("guest pid");
        terminals
            .register(guest_pid, OwnedFd::from(terminal))
            .expect("registration");

        assert!(terminals.take(guest_pid).is_some());
        assert!(terminals.take(guest_pid).is_none(), "one terminal answered two members");
    }

    /// An unregistered member reads nothing rather than someone else's terminal.
    #[test]
    fn an_unregistered_member_is_answered_with_no_terminal() {
        let terminals = MemberTerminals::default();
        let (terminal, _peer) = UnixStream::pair().expect("terminal stand-in");
        terminals
            .register(NonZeroI32::new(9).expect("guest pid"), OwnedFd::from(terminal))
            .expect("registration");
        assert!(terminals.take(NonZeroI32::new(10).expect("guest pid")).is_none());
    }

    /// The descriptor has to arrive as a descriptor: the peer must be able to read bytes written to the
    /// original through the copy the kernel installed for it.
    #[test]
    fn a_sent_descriptor_arrives_as_a_working_descriptor() {
        use std::io::{Read as _, Write as _};

        let (server, client) = UnixStream::pair().expect("channel");
        let (carried, mut carried_peer) = UnixStream::pair().expect("carried");
        let header = [7_u8; 32];
        send_with_descriptor(&server, &header, carried.as_fd().as_raw_fd()).expect("descriptor reply");

        let mut received = [0_u8; 32];
        let (read, installed) = receive_with_descriptor(&client, &mut received);
        assert_eq!(read, 32);
        assert_eq!(received, header);
        let mut installed = std::fs::File::from(installed.expect("the reply carried a descriptor"));
        carried_peer.write_all(b"live").expect("write through the peer");
        let mut bytes = [0_u8; 4];
        installed.read_exact(&mut bytes).expect("read through the copy");
        assert_eq!(&bytes, b"live");
    }
}
