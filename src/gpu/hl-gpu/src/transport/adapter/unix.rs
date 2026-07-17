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

/// Hard ceiling on a single frame's (or handshake's) declared payload length, enforced BEFORE the
/// receive buffer is allocated. The header carries a raw untrusted `u32` length (up to 4 GiB); without
/// this cap a hostile/buggy peer could make the reader `vec![0u8; 4 GiB]` per frame — a pure
/// memory-exhaustion DoS — before a single body byte is read.
///
/// This is a RUNTIME transport cap, not a wire-format change: no bytes are added to any frame, and every
/// legitimate frame stays byte-identical. It is set comfortably above the largest legitimate frame: the
/// negotiated [`Capabilities::max_frame_bytes`](crate::protocol::model::capability::Capabilities) default
/// is 256 MiB (`256 << 20`, browser-class) and the runtime validation pass rejects any decoded frame above
/// that negotiated ceiling, so this 512 MiB transport cap sits well above anything the negotiated limits
/// would accept — it only refuses the pathological hundreds-of-MB/GB preallocation. Exceeding it yields a
/// typed `InvalidData` IO error (a "FrameTooLarge"/protocol rejection) instead of a giant allocation.
pub const MAX_FRAME_BYTES: u32 = 512 << 20; // 512 MiB

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

/// The outcome of reading one frame off the connection, distinguishing the three cases the serve loop must
/// treat differently: a complete frame, a clean end-of-stream, and an over-cap frame that must be REJECTED
/// (NACK) without killing the connection.
///
/// The over-cap case is the crucial one for connection robustness: a single frame whose declared length
/// exceeds [`MAX_FRAME_BYTES`] must NOT be allowed to tear down the persistent connection (which drops the
/// host's warm per-connection caches AND every subsequent frame — the guest sees `Broken pipe`). Instead the
/// header is surfaced here with its (still-unconsumed) payload so the serve loop can drain those bytes to
/// keep the stream in sync and reply with a NACK, then keep serving.
pub enum FrameOutcome {
    /// A complete frame (header + fully-read payload).
    Frame(Frame),
    /// Clean EOF exactly at a frame boundary: the peer closed the connection.
    Eof,
    /// The header declared a payload above [`MAX_FRAME_BYTES`]; the payload has NOT been read (so no giant
    /// allocation happened). The caller must drain `header.len` bytes with [`drain_payload`] to resync the
    /// stream, then reject the frame. `header.len` carries the declared length to drain.
    TooLarge(SubmitHeader),
}

/// Read the fixed 16-byte submit header off `stream`, distinguishing a CLEAN end-of-stream (no bytes at a
/// frame boundary) from a TRUNCATED header (some header bytes then EOF). Returns `Ok(None)` on clean EOF.
fn read_header(stream: &UnixStream) -> io::Result<Option<SubmitHeader>> {
    let mut s = stream;
    let mut hdr = [0u8; SubmitHeader::SIZE];
    let mut filled = 0usize;
    while filled < hdr.len() {
        match s.read(&mut hdr[filled..]) {
            Ok(0) => {
                if filled == 0 {
                    return Ok(None);
                }
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated submit header (EOF mid-header)",
                ));
            }
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(Some(SubmitHeader::from_bytes(&hdr)))
}

/// Read one submit frame off the connection, reporting a [`FrameOutcome`]. Unlike [`read_frame`], an over-cap
/// declared length is NOT an error here: it is surfaced as [`FrameOutcome::TooLarge`] so the serve loop can
/// drain + NACK it and keep the connection alive rather than closing on one bad frame. The payload for an
/// over-cap frame is left UNREAD (no allocation), and the caller MUST drain it (via [`drain_payload`]) before
/// reading the next frame.
pub fn read_frame_outcome(stream: &UnixStream) -> io::Result<FrameOutcome> {
    let mut s = stream;
    let header = match read_header(stream)? {
        Some(h) => h,
        None => return Ok(FrameOutcome::Eof),
    };
    // Cap the declared payload length BEFORE allocating so an untrusted `u32` length cannot force a
    // multi-GB preallocation (a memory-exhaustion DoS) ahead of reading any body bytes. Report it as a
    // recoverable TooLarge rather than reading the body.
    if header.len > MAX_FRAME_BYTES {
        // The detector: an over-cap declared length (e.g. a 4.3 GiB GTK frame) surfaces here before any
        // body byte is read. Counted as a nack by the serve loop that drains it.
        hl_log::hl_warn!(
            hl_log::tag::TRANSPORT,
            "frame too large len={} cap={}",
            header.len,
            MAX_FRAME_BYTES
        );
        return Ok(FrameOutcome::TooLarge(header));
    }
    let mut payload = vec![0u8; header.len as usize];
    s.read_exact(&mut payload)?;
    hl_log::hl_count!(hl_log::tag::TRANSPORT, "frames");
    hl_log::hl_add!(hl_log::tag::TRANSPORT, "frame_bytes", header.len as u64);
    Ok(FrameOutcome::Frame(Frame { header, payload }))
}

/// Discard exactly `len` payload bytes from `stream` in bounded chunks, WITHOUT allocating a buffer the size
/// of the (possibly hundreds-of-MB) frame. Used by the serve loop to resync the stream after a
/// [`FrameOutcome::TooLarge`] so the connection survives an over-cap frame. A truncated stream (peer closed
/// mid-payload) surfaces as `UnexpectedEof` — the connection is genuinely gone.
pub fn drain_payload(stream: &UnixStream, len: u32) -> io::Result<()> {
    let mut s = stream;
    let mut remaining = len as u64;
    let mut scratch = [0u8; 64 * 1024];
    while remaining > 0 {
        let want = remaining.min(scratch.len() as u64) as usize;
        match s.read(&mut scratch[..want]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "EOF while draining an over-cap frame payload",
                ));
            }
            Ok(n) => remaining -= n as u64,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Read one submit frame off the connection. Returns `Ok(None)` on a clean EOF at a frame boundary (the
/// peer closed the connection), and `Err` on a partial/truncated frame or an over-cap declared length
/// ([`InvalidData`](io::ErrorKind::InvalidData) "FrameTooLarge"). The over-cap payload is left unread (no
/// draining), so a caller that must keep the connection alive should use [`read_frame_outcome`] +
/// [`drain_payload`] instead of this convenience wrapper.
pub fn read_frame(stream: &UnixStream) -> io::Result<Option<Frame>> {
    match read_frame_outcome(stream)? {
        FrameOutcome::Frame(f) => Ok(Some(f)),
        FrameOutcome::Eof => Ok(None),
        FrameOutcome::TooLarge(header) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "frame payload length {} exceeds cap {MAX_FRAME_BYTES} (FrameTooLarge)",
                header.len
            ),
        )),
    }
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
    let len_u32 = u32::from_le_bytes(len_bytes);
    // Same untrusted-length cap as `read_frame`: refuse an absurd handshake length before allocating its
    // body buffer, so a hostile peer cannot force a multi-GB preallocation at connect time.
    if len_u32 > MAX_FRAME_BYTES {
        hl_log::hl_warn!(
            hl_log::tag::TRANSPORT,
            "handshake too large len={} cap={}",
            len_u32,
            MAX_FRAME_BYTES
        );
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("handshake length {len_u32} exceeds cap {MAX_FRAME_BYTES} (FrameTooLarge)"),
        ));
    }
    let len = len_u32 as usize;
    // Reconstruct the full `[len][body]` frame so we reuse `Capabilities::from_handshake` verbatim.
    let mut full = Vec::with_capacity(4 + len);
    full.extend_from_slice(&len_bytes);
    let mut body = vec![0u8; len];
    s.read_exact(&mut body)?;
    full.extend_from_slice(&body);
    decode_handshake(&full)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad handshake: {e}")))
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
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
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
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
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
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no SCM_RIGHTS cmsg received",
            ));
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
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(RENDER_NODE)?;
        let mut a = GpuAlloc {
            width,
            height,
            format,
            ..Default::default()
        };
        let rc = unsafe {
            libc::ioctl(
                f.as_raw_fd(),
                HL_IOCTL_GPU_ALLOC as _,
                &mut a as *mut _ as *mut libc::c_void,
            )
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
    /// Write end, only on platforms where the doorbell is a self-pipe (no eventfd).
    #[cfg(not(target_os = "linux"))]
    write_fd: RawFd,
}

impl Doorbell {
    /// Create a completion doorbell. On Linux this is a semaphore-mode eventfd
    /// (`EFD_CLOEXEC | EFD_SEMAPHORE`); elsewhere (macOS) a self-pipe, since eventfd/futex are
    /// Linux-only. Portable so the transport crate compiles on the host as well as the guest.
    pub fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_SEMAPHORE) };
            if fd < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Doorbell { fd })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut fds = [0 as RawFd; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Doorbell {
                fd: fds[0],
                write_fd: fds[1],
            })
        }
    }
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for Doorbell {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
        #[cfg(not(target_os = "linux"))]
        unsafe {
            libc::close(self.write_fd)
        };
    }
}

/// Wake up to `n` waiters parked on the futex word at `addr` (`FUTEX_WAKE`, private). The completion
/// primitive for the shared-ring path; unused by the socket-ack path but part of the transport mechanism
/// so a ring-mode transport shares it.
///
/// # Safety
/// `addr` must point to a live, correctly-aligned `u32` shared with the host.
#[cfg(target_os = "linux")]
pub unsafe fn futex_wake(addr: *mut u32, n: i32) -> i64 {
    libc::syscall(
        libc::SYS_futex,
        addr,
        libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
        n,
    ) as i64
}

/// macOS/BSD have no `futex` syscall (the ring-mode path is Linux-only); the socket-ack transport
/// never calls this, so off-Linux it is a no-op returning 0.
#[cfg(not(target_os = "linux"))]
pub unsafe fn futex_wake(_addr: *mut u32, _n: i32) -> i64 {
    0
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
        assert_eq!(
            unsafe { libc::write(write_end, payload.as_ptr().cast(), 1) },
            1
        );
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
