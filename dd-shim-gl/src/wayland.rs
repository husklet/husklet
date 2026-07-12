//! Guest wayland/dma-buf display client — the "present" half that shows the executor-rendered
//! IOSurface on screen (dd-display). A byte-for-byte port of gl_shim.c's hand-rolled wayland protocol
//! (`surface_up` handshake + `wl_commit` per-frame dma-buf commit + frame-callback pacing).
//!
//! dd-display advertises a fixed set of globals with fixed registry names, and the shim uses fixed
//! wayland object ids — so the whole exchange is a small, deterministic message script. All integers
//! are little-endian; the dma-buf fd is delivered out-of-band via `SCM_RIGHTS`.

use core::ffi::{c_int, c_void};

use crate::state::Surface;

// Fixed wayland object ids (gl_shim.c).
const OBJ_DISPLAY: u32 = 1;
const OBJ_REGISTRY: u32 = 2;
const OBJ_COMPOSITOR: u32 = 3;
const OBJ_DMABUF: u32 = 4;
const OBJ_XDG_WM_BASE: u32 = 5;
const OBJ_WL_SURFACE: u32 = 6;
const OBJ_XDG_SURFACE: u32 = 7;
const OBJ_TOPLEVEL: u32 = 8;
const OBJ_PARAMS: u32 = 9;
const OBJ_WL_BUFFER: u32 = 10;
const OBJ_FRAME_CB: u32 = 11;

const DD_DMABUF_MOD_MAGIC: u32 = 0x6464;
const DRM_FMT_XRGB8888: u32 = 0x3432_5258;

// ---- minimal libc surface (dependency-free) --------------------------------------------------------
#[repr(C)]
struct IoVec {
    base: *mut c_void,
    len: usize,
}
#[repr(C)]
struct MsgHdr {
    name: *mut c_void,
    namelen: u32,
    _pad0: u32,
    iov: *mut IoVec,
    iovlen: usize,
    control: *mut c_void,
    controllen: usize,
    flags: c_int,
}
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}
extern "C" {
    fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
    fn sendmsg(fd: c_int, msg: *const MsgHdr, flags: c_int) -> isize;
    fn poll(fds: *mut PollFd, n: u64, timeout: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
}

const AF_UNIX: c_int = 1;
const SOCK_STREAM: c_int = 1;
const SOL_SOCKET: c_int = 1;
const SCM_RIGHTS: c_int = 1;
const POLLIN: i16 = 0x001;

/// Surface geometry the shim advertises to the compositor (logical size + centering offset).
#[derive(Clone, Copy, Default)]
pub struct Geometry {
    pub backing_w: u32,
    pub backing_h: u32,
    pub logical_w: i32,
    pub logical_h: i32,
    pub geom_x: i32,
    pub geom_y: i32,
    pub attach_x: i32,
    pub attach_y: i32,
}

impl Geometry {
    /// Whether `xdg_surface.set_window_geometry` should be sent (logical != backing or offset != 0),
    /// mirroring gl_shim.c `wl_send_window_geometry`.
    fn should_send(&self) -> bool {
        self.logical_w > 0
            && self.logical_h > 0
            && !(self.logical_w == self.backing_w as i32
                && self.logical_h == self.backing_h as i32
                && self.geom_x == 0
                && self.geom_y == 0)
    }
}

/// A connected wayland session to dd-display.
pub struct Wayland {
    fd: c_int,
    tx: Vec<u8>,
    rx: Vec<u8>,
    ready: bool,
}

impl Wayland {
    /// Append one wayland request: `[obj][ (size<<16)|op ][words…]` (gl_shim.c `wmsg`).
    fn wmsg(&mut self, obj: u32, op: u16, words: &[u32]) {
        let sz = (8 + words.len() * 4) as u32;
        self.tx.extend_from_slice(&obj.to_le_bytes());
        self.tx.extend_from_slice(&((sz << 16) | op as u32).to_le_bytes());
        for w in words {
            self.tx.extend_from_slice(&w.to_le_bytes());
        }
    }

    /// A `registry.bind(name, interface, version, new_id)` request (gl_shim.c `BIND`).
    fn bind(&mut self, name: u32, interface: &str, version: u32, new_id: u32) {
        let mut words = vec![name, (interface.len() + 1) as u32];
        let mut sbuf = interface.as_bytes().to_vec();
        sbuf.push(0);
        while sbuf.len() % 4 != 0 {
            sbuf.push(0);
        }
        for chunk in sbuf.chunks(4) {
            let mut w = [0u8; 4];
            w[..chunk.len()].copy_from_slice(chunk);
            words.push(u32::from_le_bytes(w));
        }
        words.push(version);
        words.push(new_id);
        self.wmsg(OBJ_REGISTRY, 0, &words);
    }

    fn wflush(&mut self) {
        if !self.tx.is_empty() {
            unsafe { write(self.fd, self.tx.as_ptr() as *const c_void, self.tx.len()) };
            self.tx.clear();
        }
    }

    /// Flush the pending buffer with a single fd attached via `SCM_RIGHTS` (gl_shim.c `wflush_fd`).
    fn wflush_fd(&mut self, fd: c_int) {
        let mut iov = IoVec { base: self.tx.as_mut_ptr() as *mut c_void, len: self.tx.len() };
        // control buffer: CMSG_SPACE(sizeof(int)) == 24 on LP64.
        let mut cbuf = [0u8; 24];
        cbuf[0..8].copy_from_slice(&20usize.to_ne_bytes()); // cmsg_len = CMSG_LEN(4) = 20
        cbuf[8..12].copy_from_slice(&SOL_SOCKET.to_ne_bytes());
        cbuf[12..16].copy_from_slice(&SCM_RIGHTS.to_ne_bytes());
        cbuf[16..20].copy_from_slice(&fd.to_ne_bytes());
        let mh = MsgHdr {
            name: core::ptr::null_mut(),
            namelen: 0,
            _pad0: 0,
            iov: &mut iov,
            iovlen: 1,
            control: cbuf.as_mut_ptr() as *mut c_void,
            controllen: cbuf.len(),
            flags: 0,
        };
        unsafe { sendmsg(self.fd, &mh, 0) };
        self.tx.clear();
    }

    fn send_geometry(&mut self, g: &Geometry) {
        if g.should_send() {
            self.wmsg(OBJ_XDG_SURFACE, 3, &[g.geom_x as u32, g.geom_y as u32, g.logical_w as u32, g.logical_h as u32]);
        }
    }

    /// Connect to `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` and run the registry handshake (gl_shim.c
    /// `surface_up` wayland half). Returns None if the socket is unavailable.
    pub fn connect_and_handshake(g: &Geometry) -> Option<Wayland> {
        let disp = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
        let rd = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/0".to_string());
        let path = if disp.starts_with('/') { disp } else { format!("{rd}/{disp}") };

        let fd = unsafe { socket(AF_UNIX, SOCK_STREAM, 0) };
        if fd < 0 {
            return None;
        }
        // sockaddr_un: family (u16) + path (108 bytes).
        let mut sa = [0u8; 110];
        sa[0..2].copy_from_slice(&(AF_UNIX as u16).to_ne_bytes());
        let pb = path.as_bytes();
        let n = pb.len().min(107);
        sa[2..2 + n].copy_from_slice(&pb[..n]);
        if unsafe { connect(fd, sa.as_ptr() as *const c_void, sa.len() as u32) } != 0 {
            unsafe { close(fd) };
            return None;
        }
        let mut w = Wayland { fd, tx: Vec::new(), rx: Vec::new(), ready: false };
        w.wmsg(OBJ_DISPLAY, 1, &[OBJ_REGISTRY]); // get_registry
        w.bind(1, "wl_compositor", 4, OBJ_COMPOSITOR);
        w.bind(6, "zwp_linux_dmabuf_v1", 3, OBJ_DMABUF);
        w.bind(3, "xdg_wm_base", 1, OBJ_XDG_WM_BASE);
        w.wmsg(OBJ_COMPOSITOR, 0, &[OBJ_WL_SURFACE]); // create_surface
        w.wmsg(OBJ_XDG_WM_BASE, 2, &[OBJ_XDG_SURFACE, OBJ_WL_SURFACE]); // get_xdg_surface
        w.wmsg(OBJ_XDG_SURFACE, 1, &[OBJ_TOPLEVEL]); // get_toplevel
        w.send_geometry(g);
        w.wmsg(OBJ_WL_SURFACE, 6, &[]); // commit (initial)
        w.wflush();
        w.wmsg(OBJ_XDG_SURFACE, 4, &[1]); // ack_configure(serial=1)
        w.wflush();
        w.ready = true;
        Some(w)
    }

    /// Commit the executor-rendered dma-buf/IOSurface for `surf` to the compositor (gl_shim.c
    /// `wl_commit`), then pace on the frame callback.
    pub fn commit(&mut self, surf: &Surface, g: &Geometry) {
        if !self.ready {
            return;
        }
        self.wmsg(OBJ_DMABUF, 1, &[OBJ_PARAMS]); // create_params
        self.wflush();
        // params.add(fd, plane=0, offset=0, stride, mod_hi=magic, mod_lo=surface id)
        self.wmsg(OBJ_PARAMS, 1, &[0, 0, surf.stride, DD_DMABUF_MOD_MAGIC, surf.id]);
        self.wflush_fd(surf.fd);
        self.wmsg(OBJ_PARAMS, 3, &[OBJ_WL_BUFFER, surf.width, surf.height, DRM_FMT_XRGB8888, 0]); // create_immed
        self.wmsg(OBJ_WL_SURFACE, 1, &[OBJ_WL_BUFFER, g.attach_x as u32, g.attach_y as u32]); // attach
        self.wmsg(OBJ_WL_SURFACE, 2, &[0, 0, surf.width, surf.height]); // damage
        self.send_geometry(g);
        self.wmsg(OBJ_WL_SURFACE, 3, &[OBJ_FRAME_CB]); // frame(callback)
        self.wmsg(OBJ_WL_SURFACE, 6, &[]); // commit
        self.wflush();
        self.wmsg(OBJ_PARAMS, 0, &[]); // params.destroy
        self.wmsg(OBJ_WL_BUFFER, 0, &[]); // wl_buffer.destroy
        self.wflush();
        self.drain_until_frame();
    }

    /// Drain wayland events until `wl_callback.done` for the frame callback, bounded by 100 ms
    /// (gl_shim.c `wl_drain_until_frame` — paces the guest to the compositor's present rate).
    fn drain_until_frame(&mut self) {
        let deadline = now_ms() + 100;
        loop {
            // parse buffered events for wl_callback.done (obj == FRAME_CB, op == 0)
            let mut off = 0usize;
            let mut hit = false;
            while self.rx.len() - off >= 8 {
                let obj = u32::from_le_bytes(self.rx[off..off + 4].try_into().unwrap());
                let so = u32::from_le_bytes(self.rx[off + 4..off + 8].try_into().unwrap());
                let size = (so >> 16) as usize;
                let op = so & 0xffff;
                if size < 8 {
                    off = self.rx.len();
                    break;
                }
                if self.rx.len() - off < size {
                    break;
                }
                if obj == OBJ_FRAME_CB && op == 0 {
                    hit = true;
                }
                off += size;
            }
            if off > 0 {
                self.rx.drain(..off);
            }
            if hit {
                return;
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                return;
            }
            let mut pfd = PollFd { fd: self.fd, events: POLLIN, revents: 0 };
            let pr = unsafe { poll(&mut pfd, 1, rem as c_int + 1) };
            if pr <= 0 {
                return;
            }
            if pfd.revents & POLLIN != 0 {
                let mut buf = [0u8; 8192];
                let n = unsafe { read(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
                if n <= 0 {
                    return;
                }
                self.rx.extend_from_slice(&buf[..n as usize]);
            }
        }
    }
}

impl Drop for Wayland {
    fn drop(&mut self) {
        unsafe { close(self.fd) };
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build the handshake byte stream (without a live socket) and assert the wire encoding matches
    // gl_shim.c's exact message script. Uses a dummy fd = -1 (no writes are made in this builder path).
    #[test]
    fn handshake_stream_matches_gl_shim_c() {
        let g = Geometry { backing_w: 640, backing_h: 480, logical_w: 640, logical_h: 480, ..Default::default() };
        let mut w = Wayland { fd: -1, tx: Vec::new(), rx: Vec::new(), ready: false };
        // Replicate connect_and_handshake's message building (no socket I/O — collect into one buffer).
        let mut all = Vec::new();
        w.wmsg(OBJ_DISPLAY, 1, &[OBJ_REGISTRY]);
        w.bind(1, "wl_compositor", 4, OBJ_COMPOSITOR);
        w.bind(6, "zwp_linux_dmabuf_v1", 3, OBJ_DMABUF);
        w.bind(3, "xdg_wm_base", 1, OBJ_XDG_WM_BASE);
        w.wmsg(OBJ_COMPOSITOR, 0, &[OBJ_WL_SURFACE]);
        w.wmsg(OBJ_XDG_WM_BASE, 2, &[OBJ_XDG_SURFACE, OBJ_WL_SURFACE]);
        w.wmsg(OBJ_XDG_SURFACE, 1, &[OBJ_TOPLEVEL]);
        w.send_geometry(&g); // full-size → not sent
        w.wmsg(OBJ_WL_SURFACE, 6, &[]);
        all.append(&mut w.tx);
        w.wmsg(OBJ_XDG_SURFACE, 4, &[1]);
        all.append(&mut w.tx);

        // get_registry: obj=1, op=1, size=12, arg=registry(2)
        assert_eq!(&all[0..12], &[1, 0, 0, 0, /*size12|op1*/ 1, 0, 12, 0, 2, 0, 0, 0]);
        // The wl_compositor bind follows: obj=registry(2), op=0. name=1, strlen+1=14, "wl_compositor\0"
        // padded to 16 (4 words), version=4, new_id=3. size = 8 + (2 + 4 + 2)*4 = 40.
        let bind0 = &all[12..12 + 40];
        assert_eq!(&bind0[0..8], &[2, 0, 0, 0, 0, 0, 40, 0]); // obj=2, (40<<16)|0
        assert_eq!(&bind0[8..12], &[1, 0, 0, 0]); // name = 1
        assert_eq!(&bind0[12..16], &[14, 0, 0, 0]); // strlen+1 = 14
        assert_eq!(&bind0[16..30], b"wl_compositor\0"); // interface + nul
        assert_eq!(&bind0[32..36], &[4, 0, 0, 0]); // version
        assert_eq!(&bind0[36..40], &[3, 0, 0, 0]); // new_id = compositor(3)
    }

    #[test]
    fn full_size_geometry_is_not_sent() {
        let g = Geometry { backing_w: 100, backing_h: 100, logical_w: 100, logical_h: 100, ..Default::default() };
        assert!(!g.should_send());
        let g2 = Geometry { backing_w: 100, backing_h: 100, logical_w: 80, logical_h: 60, geom_x: 10, geom_y: 20, ..Default::default() };
        assert!(g2.should_send());
    }
}
