//! The Wayland wire protocol — framing, argument codec, and fd passing over `AF_UNIX`.
//!
//! We hand-roll this rather than pull in Smithay because Smithay's server stack drags in
//! `drm`/`gbm`/`libinput`/`udev`/`rustix`-Linux dependencies that do not build on macOS (where the real
//! `dd-display` runs). The wire format itself is tiny and stable, so a focused codec keeps the CPU-buffer
//! MVP portable AND fully unit-testable on the Linux dev host. See `docs/ideas/RENDERING_PLAN.md`.
//!
//! Wire format (host byte order — guest and host are the same ISA under dd, so native-endian is correct):
//!   header: u32 `object_id`, u32 `(size << 16) | opcode`  (size counts the 8-byte header).
//!   args, each 32-bit aligned: int/uint/fixed = 1 word; object/new_id = 1 word (u32 id);
//!   string = u32 len (incl. NUL) then bytes padded to 4 (len 0 ⇒ null); array = u32 size then bytes
//!   padded; `fd` occupies NO bytes in the data stream — it rides the `SCM_RIGHTS` ancillary queue.

use std::collections::VecDeque;
use std::io;
use std::os::unix::io::RawFd;

/// A decoded (or to-be-encoded) Wayland message: which object, which opcode, and its argument words.
/// Args are kept as raw `u32` words plus side buffers for strings/arrays; `fd` args are pulled from /
/// pushed to the connection's fd queue, never the byte body.
#[derive(Debug, Clone)]
pub struct Message {
    pub object: u32,
    pub opcode: u16,
    pub body: Vec<u8>, // the argument bytes, exactly as they appear after the 8-byte header
}

impl Message {
    pub fn new(object: u32, opcode: u16) -> Message {
        Message {
            object,
            opcode,
            body: Vec::new(),
        }
    }

    // ---- encoders (append to body) ----
    pub fn u32(mut self, v: u32) -> Self {
        self.body.extend_from_slice(&v.to_ne_bytes());
        self
    }
    pub fn i32(self, v: i32) -> Self {
        self.u32(v as u32)
    }
    /// A Wayland string: length (incl. NUL) then UTF-8 bytes + NUL, padded to a 4-byte boundary.
    pub fn string(mut self, s: &str) -> Self {
        let len = s.len() as u32 + 1; // + NUL
        self.body.extend_from_slice(&len.to_ne_bytes());
        self.body.extend_from_slice(s.as_bytes());
        self.body.push(0);
        while self.body.len() % 4 != 0 {
            self.body.push(0);
        }
        self
    }
    /// A wl_array: a size-prefixed opaque byte blob, padded to 4.
    pub fn array(mut self, bytes: &[u8]) -> Self {
        self.body
            .extend_from_slice(&(bytes.len() as u32).to_ne_bytes());
        self.body.extend_from_slice(bytes);
        while self.body.len() % 4 != 0 {
            self.body.push(0);
        }
        self
    }

    /// Serialize into `out` (header + body). Panics never; the size field counts the header.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let size = (8 + self.body.len()) as u32;
        out.extend_from_slice(&self.object.to_ne_bytes());
        out.extend_from_slice(&((size << 16) | self.opcode as u32).to_ne_bytes());
        out.extend_from_slice(&self.body);
    }

    /// A cursor for decoding this message's argument body.
    pub fn reader(&self) -> ArgReader<'_> {
        ArgReader {
            b: &self.body,
            pos: 0,
        }
    }
}

/// Sequential reader over a message body. Bounds-checked; returns 0/"" on underrun rather than panicking,
/// so a malformed client can't crash the compositor.
pub struct ArgReader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> ArgReader<'a> {
    pub fn u32(&mut self) -> u32 {
        if self.pos + 4 > self.b.len() {
            return 0;
        }
        let v = u32::from_ne_bytes(self.b[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }
    pub fn i32(&mut self) -> i32 {
        self.u32() as i32
    }
    pub fn string(&mut self) -> String {
        let len = self.u32() as usize;
        if len == 0 || self.pos + len > self.b.len() {
            return String::new();
        }
        let end = self.pos + len - 1; // drop trailing NUL
        let s = String::from_utf8_lossy(&self.b[self.pos..end]).into_owned();
        let padded = (len + 3) & !3;
        self.pos += padded;
        s
    }
    pub fn array(&mut self) -> Vec<u8> {
        let len = self.u32() as usize;
        if self.pos + len > self.b.len() {
            self.pos = self.b.len();
            return Vec::new();
        }
        let out = self.b[self.pos..self.pos + len].to_vec();
        let padded = (len + 3) & !3;
        self.pos = self.pos.saturating_add(padded).min(self.b.len());
        out
    }
}

/// A buffered Wayland connection: a `SOCK_STREAM` fd carrying message bytes plus an out-of-band queue of
/// received/pending file descriptors (`SCM_RIGHTS`). One per client.
pub struct Conn {
    fd: RawFd,
    rx: Vec<u8>,             // unparsed inbound bytes
    rx_fds: VecDeque<RawFd>, // received fds, consumed in order by `fd`-typed args
    tx: Vec<u8>,             // pending outbound bytes
    tx_fds: Vec<RawFd>,      // fds to send with the next flush
}

impl Conn {
    pub fn new(fd: RawFd) -> Conn {
        Conn {
            fd,
            rx: Vec::new(),
            rx_fds: VecDeque::new(),
            tx: Vec::new(),
            tx_fds: Vec::new(),
        }
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Next received fd (for an `fd`-typed request arg). `None` if the client hasn't sent one yet.
    pub fn take_fd(&mut self) -> Option<RawFd> {
        self.rx_fds.pop_front()
    }

    /// Queue a message for sending on the next `flush`.
    pub fn send(&mut self, m: &Message) {
        m.encode(&mut self.tx);
    }

    /// Attach a file descriptor to the next `flush` (rides `SCM_RIGHTS`). Used by a client to hand its
    /// `wl_shm` pool fd to the compositor alongside the `create_pool` request.
    pub fn queue_fd(&mut self, fd: RawFd) {
        self.tx_fds.push(fd);
    }

    /// Read from the socket into the rx buffer, harvesting any `SCM_RIGHTS` fds. Returns bytes read
    /// (0 ⇒ peer closed). Non-fatal `EAGAIN` maps to `Ok(-1)` so a caller in a poll loop can distinguish
    /// "would block" from EOF.
    pub fn fill(&mut self) -> io::Result<isize> {
        let mut buf = [0u8; 4096];
        let mut cbuf = [0u8; 256];
        let mut iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut _,
            iov_len: buf.len(),
        };
        let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
        mh.msg_iov = &mut iov;
        mh.msg_iovlen = 1;
        mh.msg_control = cbuf.as_mut_ptr() as *mut _;
        mh.msg_controllen = cbuf.len() as _;
        let n = unsafe { libc::recvmsg(self.fd, &mut mh, 0) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EAGAIN) || e.raw_os_error() == Some(libc::EWOULDBLOCK)
            {
                return Ok(-1);
            }
            return Err(e);
        }
        if n == 0 {
            return Ok(0);
        }
        self.rx.extend_from_slice(&buf[..n as usize]);
        // Harvest passed fds.
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&mh);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let data = libc::CMSG_DATA(cmsg) as *const RawFd;
                    let payload = (*cmsg).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                    let count = payload / std::mem::size_of::<RawFd>();
                    for i in 0..count {
                        self.rx_fds.push_back(*data.add(i));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&mh, cmsg);
            }
        }
        Ok(n)
    }

    /// Pop the next complete message from the rx buffer, if one has fully arrived.
    pub fn next_message(&mut self) -> Option<Message> {
        if self.rx.len() < 8 {
            return None;
        }
        let object = u32::from_ne_bytes(self.rx[0..4].try_into().unwrap());
        let word = u32::from_ne_bytes(self.rx[4..8].try_into().unwrap());
        let size = (word >> 16) as usize;
        let opcode = (word & 0xffff) as u16;
        if size < 8 || self.rx.len() < size {
            return None;
        }
        let body = self.rx[8..size].to_vec();
        self.rx.drain(0..size);
        Some(Message {
            object,
            opcode,
            body,
        })
    }

    /// Flush pending outbound bytes (and any queued fds via `SCM_RIGHTS`) to the socket.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.tx.is_empty() && self.tx_fds.is_empty() {
            return Ok(());
        }
        let tx = std::mem::take(&mut self.tx);
        let fds = std::mem::take(&mut self.tx_fds);
        let mut iov = libc::iovec {
            iov_base: tx.as_ptr() as *mut _,
            iov_len: tx.len().max(1),
        };
        let mut mh: libc::msghdr = unsafe { std::mem::zeroed() };
        mh.msg_iov = &mut iov;
        mh.msg_iovlen = 1;
        let mut cbuf = vec![0u8; unsafe { libc::CMSG_SPACE((fds.len() * 4) as u32) } as usize];
        if !fds.is_empty() {
            mh.msg_control = cbuf.as_mut_ptr() as *mut _;
            mh.msg_controllen = cbuf.len() as _;
            unsafe {
                let cmsg = libc::CMSG_FIRSTHDR(&mh);
                (*cmsg).cmsg_level = libc::SOL_SOCKET;
                (*cmsg).cmsg_type = libc::SCM_RIGHTS;
                (*cmsg).cmsg_len = libc::CMSG_LEN((fds.len() * 4) as u32) as _;
                let data = libc::CMSG_DATA(cmsg) as *mut RawFd;
                for (i, fd) in fds.iter().enumerate() {
                    *data.add(i) = *fd;
                }
            }
        }
        let n = unsafe { libc::sendmsg(self.fd, &mh, 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_args() {
        let m = Message::new(3, 0)
            .u32(0xdead)
            .i32(-7)
            .string("wl_shm")
            .array(&[1, 2, 3]);
        let mut buf = Vec::new();
        m.encode(&mut buf);
        // header: object then (size<<16|op)
        assert_eq!(u32::from_ne_bytes(buf[0..4].try_into().unwrap()), 3);
        let word = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
        assert_eq!(word & 0xffff, 0);
        assert_eq!((word >> 16) as usize, buf.len());

        let mut r = m.reader();
        assert_eq!(r.u32(), 0xdead);
        assert_eq!(r.i32(), -7);
        assert_eq!(r.string(), "wl_shm");
    }

    #[test]
    fn string_padding_is_4_aligned() {
        // "ab" -> len 3 (incl NUL) -> padded to 4 data bytes, plus the 4-byte length word = 8.
        let m = Message::new(1, 0).string("ab");
        assert_eq!(m.body.len(), 8);
    }
}
