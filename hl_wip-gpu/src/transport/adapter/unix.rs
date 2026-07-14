//! The concrete Unix-socket mechanism: connect/read/write of frames + acks + the handshake, out-of-band
//! `SCM_RIGHTS` fd transfer, the render-node `HL_IOCTL_GPU_ALLOC` allocation, and the futex/eventfd
//! doorbell. This is the one place technology (Unix sockets, ioctl, futex) is named — everything above it
//! speaks in [`Frame`]/[`SubmitHeader`]/[`Capabilities`] values.
//!
//! Ported from `hl-shim`'s `transport.rs`. The submit-header/ack framing is byte-identical to the shipped
//! `gl_shim.c` `exec_stream`; the handshake body is the protocol codec's own bytes.

use std::io::{self, Read, Write};
use std::os::unix::io::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;

use crate::protocol::model::capability::Capabilities;
use crate::transport::model::frame::Frame;
use crate::transport::model::handshake::{decode_handshake, encode_handshake};
use crate::transport::model::header::SubmitHeader;
use crate::transport::model::readback::{ReadbackRequest, READBACK_MAGIC, READBACK_OK};

// ---------------------------------------------------------------------------------------------------
// framed submit IO (byte-identical to gl_shim.c's exec_stream)
// ---------------------------------------------------------------------------------------------------

/// `write(2)` all of `buf`, retrying short writes and `EINTR` (the `write_full` of `gl_shim.c`).
pub fn write_full(stream: &UnixStream, buf: &[u8]) -> io::Result<()> {
    let mut s = stream;
    s.write_all(buf)
}

/// Write one submit frame (`[16-byte header][payload]`) over the connection.
pub fn write_frame(stream: &UnixStream, header: &SubmitHeader, payload: &[u8]) -> io::Result<()> {
    let mut s = stream;
    s.write_all(&header.to_bytes())?;
    s.write_all(payload)?;
    Ok(())
}

/// Read one submit frame off the connection. Returns `Ok(None)` on a clean EOF at a frame boundary (the
/// peer closed the connection), and `Err` on a partial/truncated frame.
pub fn read_frame(stream: &UnixStream) -> io::Result<Option<Frame>> {
    let mut s = stream;
    let mut hdr = [0u8; SubmitHeader::SIZE];
    match s.read_exact(&mut hdr) {
        Ok(()) => {}
        // A clean close exactly at the header boundary is end-of-stream, not an error.
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let header = SubmitHeader::from_bytes(&hdr);
    let mut payload = vec![0u8; header.len as usize];
    s.read_exact(&mut payload)?;
    Ok(Some(Frame { header, payload }))
}

/// Read the host executor's single ack byte answering a submitted frame.
pub fn read_ack(stream: &UnixStream) -> io::Result<u8> {
    let mut s = stream;
    let mut ack = [0u8; 1];
    s.read_exact(&mut ack)?;
    Ok(ack[0])
}

/// Write the host executor's single ack byte for a frame.
pub fn write_ack(stream: &UnixStream, ack: u8) -> io::Result<()> {
    let mut s = stream;
    s.write_all(&[ack])
}

// ---------------------------------------------------------------------------------------------------
// readback IO (device→host buffer readback; additive, disjoint from the submit ack)
// ---------------------------------------------------------------------------------------------------

/// Write a device→host readback REQUEST as a submit frame whose header carries the reserved
/// [`READBACK_MAGIC`] sentinel in `surface_id` (so the server routes it to readback, never to submit) and
/// whose payload is the serialized [`ReadbackRequest`]. Reuses the exact submit-frame writer, keeping every
/// real submit byte-identical.
pub fn write_readback_request(stream: &UnixStream, req: &ReadbackRequest) -> io::Result<()> {
    let payload = req.to_bytes();
    let header = SubmitHeader {
        surface_id: READBACK_MAGIC,
        width: 0,
        height: 0,
        len: payload.len() as u32,
    };
    write_frame(stream, &header, &payload)
}

/// Write the host's readback RESPONSE: a status byte then a `u32` length-prefixed byte payload. On failure
/// `status` is [`READBACK_FAIL`](crate::transport::model::readback::READBACK_FAIL) and `bytes` must be
/// empty. This is deliberately NOT the 1-byte submit ack — only a peer that issued a readback request reads
/// this framing.
pub fn write_readback_response(stream: &UnixStream, status: u8, bytes: &[u8]) -> io::Result<()> {
    let mut out = Vec::with_capacity(1 + 4 + bytes.len());
    out.push(status);
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    write_full(stream, &out)
}

/// Read a host readback RESPONSE written by [`write_readback_response`]. Returns the returned bytes on
/// success; a failure status maps to an `Other` IO error the caller surfaces as a typed
/// [`GpuError`](crate::protocol::model::error::GpuError).
pub fn read_readback_response(stream: &UnixStream) -> io::Result<Vec<u8>> {
    let mut s = stream;
    let mut status = [0u8; 1];
    s.read_exact(&mut status)?;
    let mut len_bytes = [0u8; 4];
    s.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    match status[0] {
        READBACK_OK => Ok(body),
        _ => Err(io::Error::new(io::ErrorKind::Other, "host readback failed")),
    }
}

// ---------------------------------------------------------------------------------------------------
// handshake IO (length-prefixed protocol capability descriptor)
// ---------------------------------------------------------------------------------------------------

/// Write the host's capability advertisement as the connection handshake (`[u32 len][body]`).
pub fn write_handshake(stream: &UnixStream, caps: &Capabilities) -> io::Result<()> {
    write_full(stream, &encode_handshake(caps))
}

/// Read the host's capability advertisement off a freshly-connected socket. The handshake is a
/// length-prefixed frame; we read the `u32` length, then the body, then decode via the protocol codec.
pub fn read_handshake(stream: &UnixStream) -> io::Result<Capabilities> {
    let mut s = stream;
    let mut len_bytes = [0u8; 4];
    s.read_exact(&mut len_bytes)?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    // Reconstruct the full `[len][body]` frame so we reuse `Capabilities::from_handshake` verbatim.
    let mut full = Vec::with_capacity(4 + len);
    full.extend_from_slice(&len_bytes);
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    full.extend_from_slice(&body);
    decode_handshake(&full).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad handshake: {e}")))
}

// ---------------------------------------------------------------------------------------------------
// out-of-band handle transfer: SCM_RIGHTS fd passing
// ---------------------------------------------------------------------------------------------------

/// Send a single file descriptor to the peer over `stream` as an `SCM_RIGHTS` control message (one
/// payload byte carries it). This is the out-of-band handle channel §10 of the overview describes:
/// the fd (dma-buf / shm) is passed here and correlated to a submit by its surface id.
pub fn send_fd(stream: &UnixStream, fd: RawFd) -> io::Result<()> {
    // SAFETY: we build a well-formed `msghdr` with a single SCM_RIGHTS cmsg carrying one fd; the cmsg
    // scratch is 8-byte aligned (a `[u64; _]`) and large enough for `CMSG_SPACE(size_of::<RawFd>())`.
    unsafe {
        let mut byte = [0u8; 1];
        let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
        let mut cmsg_buf = [0u64; 8]; // 64 bytes, 8-byte aligned cmsg scratch
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
        std::ptr::copy_nonoverlapping(
            &fd as *const RawFd as *const u8,
            libc::CMSG_DATA(cmsg),
            std::mem::size_of::<RawFd>(),
        );
        let n = libc::sendmsg(stream.as_raw_fd(), &msg, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Receive a single file descriptor sent by [`send_fd`]. Returns the new fd number in this process (which
/// refers to the same open file description as the sender's fd).
pub fn recv_fd(stream: &UnixStream) -> io::Result<RawFd> {
    // SAFETY: mirror of `send_fd`; the cmsg scratch is 8-byte aligned and sized for one fd.
    unsafe {
        let mut byte = [0u8; 1];
        let mut iov = libc::iovec { iov_base: byte.as_mut_ptr().cast(), iov_len: 1 };
        let mut cmsg_buf = [0u64; 8];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = cmsg_buf.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&cmsg_buf) as _;
        let n = libc::recvmsg(stream.as_raw_fd(), &mut msg, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        if cmsg.is_null()
            || (*cmsg).cmsg_level != libc::SOL_SOCKET
            || (*cmsg).cmsg_type != libc::SCM_RIGHTS
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "no SCM_RIGHTS cmsg received"));
        }
        let mut fd: RawFd = -1;
        std::ptr::copy_nonoverlapping(
            libc::CMSG_DATA(cmsg),
            &mut fd as *mut RawFd as *mut u8,
            std::mem::size_of::<RawFd>(),
        );
        Ok(fd)
    }
}

// ---------------------------------------------------------------------------------------------------
// render-node allocation (guest GPU-memory registration)
// ---------------------------------------------------------------------------------------------------

/// Guest GPU-memory registration via the render node.
pub mod renderd {
    use super::*;
    use crate::transport::model::abi::{GpuAlloc, HL_IOCTL_GPU_ALLOC, RENDER_NODE};
    use std::fs::OpenOptions;

    /// Allocate an engine-backed surface (rung-2 IOSurface/dma-buf) of `width`x`height`. Ports
    /// `gl_shim.c`'s `open("/dev/dri/renderD128"); ioctl(HL_IOCTL_GPU_ALLOC,&g_surf)`.
    ///
    /// The opened render-node fd is intentionally leaked for process lifetime (as in `gl_shim.c`: the
    /// surface's dma-buf fd returned in `GpuAlloc::fd` must outlive it). Returns the filled `GpuAlloc`.
    pub fn alloc(width: u32, height: u32, format: u32) -> io::Result<GpuAlloc> {
        let f = OpenOptions::new().read(true).write(true).open(RENDER_NODE)?;
        let mut a = GpuAlloc { width, height, format, ..Default::default() };
        let rc = unsafe {
            libc::ioctl(f.as_raw_fd(), HL_IOCTL_GPU_ALLOC as _, &mut a as *mut _ as *mut libc::c_void)
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        std::mem::forget(f); // keep the render node open for process lifetime
        Ok(a)
    }
}

// ---------------------------------------------------------------------------------------------------
// completion doorbell (forward seam for the shared-memory command ring)
// ---------------------------------------------------------------------------------------------------

/// A completion doorbell for the future shared-memory command ring (the eventfd/futex wake gfxstream
/// uses). The current socket path blocks on the ack instead; this is the forward seam so a ring-mode
/// transport can signal without re-inventing it.
pub struct Doorbell {
    fd: RawFd,
}

impl Doorbell {
    /// Create a semaphore-mode eventfd (`EFD_CLOEXEC | EFD_SEMAPHORE`).
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_SEMAPHORE) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Doorbell { fd })
    }
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for Doorbell {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// Wake up to `n` waiters parked on the futex word at `addr` (`FUTEX_WAKE`, private). The completion
/// primitive for the shared-ring path; unused by the socket-ack path but part of the transport mechanism
/// so a ring-mode transport shares it.
///
/// # Safety
/// `addr` must point to a live, correctly-aligned `u32` shared with the host.
pub unsafe fn futex_wake(addr: *mut u32, n: i32) -> i64 {
    libc::syscall(libc::SYS_futex, addr, libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG, n) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_writes_and_reads_over_a_socketpair() {
        let (a, b) = UnixStream::pair().unwrap();
        let caps = Capabilities::full("adapter-host");
        write_handshake(&a, &caps).unwrap();
        assert_eq!(read_handshake(&b).unwrap(), caps);
    }

    #[test]
    fn scm_rights_transfers_a_working_fd() {
        // Send the read end of a pipe over the socket; the received fd must read the byte written to the
        // pipe's write end — proving it refers to the same open file description.
        let (a, b) = UnixStream::pair().unwrap();
        let mut fds = [0 as RawFd; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (read_end, write_end) = (fds[0], fds[1]);

        send_fd(&a, read_end).unwrap();
        let got = recv_fd(&b).unwrap();
        assert!(got >= 0);

        let payload = *b"Z";
        assert_eq!(unsafe { libc::write(write_end, payload.as_ptr().cast(), 1) }, 1);
        let mut buf = [0u8; 1];
        assert_eq!(unsafe { libc::read(got, buf.as_mut_ptr().cast(), 1) }, 1);
        assert_eq!(buf, payload);

        unsafe {
            libc::close(read_end);
            libc::close(write_end);
            libc::close(got);
        }
    }

    #[test]
    fn doorbell_opens_and_closes() {
        let d = Doorbell::new().unwrap();
        assert!(d.raw_fd() >= 0);
    }
}
