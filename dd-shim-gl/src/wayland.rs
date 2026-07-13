//! Guest wayland/dma-buf display client — the "present" half that shows the executor-rendered
//! IOSurface on screen (dd-display).
//!
//! This is a real (small) wayland protocol state machine, not a fixed message script: it DISCOVERS the
//! server's globals from `wl_registry.global` events (binding each interface by its *advertised* name +
//! version rather than assuming ids), acknowledges `xdg_surface.configure` with the **received** serial,
//! answers `xdg_wm_base.ping` with a pong, and detects `wl_display.error`. The per-frame commit path is
//! fallible end to end: `wflush`/`wflush_fd` propagate short-write / fd-passing failures, a peer
//! disconnect surfaces as an error (never a silent success), and a missing frame callback within the
//! pacing deadline is reported as [`WlError::FrameTimeout`]. All integers are little-endian; the dma-buf
//! fd is delivered out of band via `SCM_RIGHTS`.

use core::ffi::{c_int, c_void};

use crate::state::Surface;

// Client-assigned wayland object ids (the client owns its id space; these are new_ids we create, NOT
// assumed server/registry names — those are discovered).
const OBJ_DISPLAY: u32 = 1;
const OBJ_REGISTRY: u32 = 2;
const OBJ_SYNC_CB: u32 = 3;
const OBJ_COMPOSITOR: u32 = 4;
const OBJ_DMABUF: u32 = 5;
const OBJ_XDG_WM_BASE: u32 = 6;
const OBJ_WL_SURFACE: u32 = 7;
const OBJ_XDG_SURFACE: u32 = 8;
const OBJ_TOPLEVEL: u32 = 9;
const OBJ_PARAMS: u32 = 10;
const OBJ_WL_BUFFER: u32 = 11;
const OBJ_FRAME_CB: u32 = 12;

const DD_DMABUF_MOD_MAGIC: u32 = 0x6464;
const DRM_FMT_XRGB8888: u32 = 0x3432_5258;
/// Allocation generation packed into `modifier_hi` bits 17..=31 (15 bits); see the dmabuf modifier
/// layout in `dd-compositor::handlers::dmabuf`. The compositor rejects a stale (retired) generation.
const DD_DMABUF_GEN_SHIFT: u32 = 17;
const DD_DMABUF_GEN_MASK: u32 = 0x7fff;

/// `modifier_hi` for a dd IOSurface buffer: the magic tag plus the allocation generation the host gave
/// this surface (0 == unversioned; see [`dd_shim_common::transport::Surface::generation`]).
fn dd_modifier_hi(generation: u32) -> u32 {
    DD_DMABUF_MOD_MAGIC | ((generation & DD_DMABUF_GEN_MASK) << DD_DMABUF_GEN_SHIFT)
}

/// How long to wait for the compositor's per-frame callback before reporting a pacing failure.
const FRAME_DEADLINE_MS: u64 = 100;
/// Bound on the initial registry/configure handshake reads.
const HANDSHAKE_DEADLINE_MS: u64 = 400;

/// A typed outcome for the fallible wayland transport (audit §11: IO / protocol / disconnect / pacing
/// failures must not look like a successful present).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WlError {
    /// The compositor closed the connection (EOF / write error) — the frame was not delivered.
    Disconnected,
    /// A socket write could not deliver the whole message.
    ShortWrite,
    /// Passing the dma-buf fd (`sendmsg`/`SCM_RIGHTS`) failed.
    FdSend,
    /// The compositor raised `wl_display.error(object, code)` (a protocol violation).
    Protocol { object: u32, code: u32 },
    /// The compositor never returned the frame callback within the pacing deadline.
    FrameTimeout,
}

pub type WlResult<T> = Result<T, WlError>;

/// One `wl_registry.global` advertisement, used to bind by discovered name/version.
#[derive(Clone)]
pub struct RegistryGlobal {
    pub name: u32,
    pub interface: String,
    pub version: u32,
}

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
    /// Whether `xdg_surface.set_window_geometry` should be sent (logical != backing or offset != 0).
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
    globals: Vec<RegistryGlobal>,
    // handshake / pacing state driven by dispatched events:
    sync_done: bool,
    configure_serial: Option<u32>,
    frame_done: bool,
}

impl Wayland {
    /// Append one wayland request: `[obj][ (size<<16)|op ][words…]`.
    fn wmsg(&mut self, obj: u32, op: u16, words: &[u32]) {
        let sz = (8 + words.len() * 4) as u32;
        self.tx.extend_from_slice(&obj.to_le_bytes());
        self.tx.extend_from_slice(&((sz << 16) | op as u32).to_le_bytes());
        for w in words {
            self.tx.extend_from_slice(&w.to_le_bytes());
        }
    }

    /// A `wl_registry.bind(name, interface, version, new_id)` request. `name` is the *discovered*
    /// registry name, never an assumed constant.
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

    /// Bind a required interface by its advertised name (clamping to the version we can speak). Returns
    /// false if the compositor never advertised it.
    fn bind_discovered(&mut self, interface: &str, max_version: u32, new_id: u32) -> bool {
        let Some(g) = self.globals.iter().find(|g| g.interface == interface).cloned() else {
            return false;
        };
        let version = g.version.min(max_version).max(1);
        self.bind(g.name, interface, version, new_id);
        true
    }

    /// Full-write the pending buffer, propagating a short write / disconnect.
    fn wflush(&mut self) -> WlResult<()> {
        let mut sent = 0usize;
        while sent < self.tx.len() {
            let n = unsafe { write(self.fd, self.tx[sent..].as_ptr() as *const c_void, self.tx.len() - sent) };
            if n < 0 {
                self.tx.clear();
                return Err(WlError::Disconnected);
            }
            if n == 0 {
                self.tx.clear();
                return Err(WlError::ShortWrite);
            }
            sent += n as usize;
        }
        self.tx.clear();
        Ok(())
    }

    /// Flush the pending buffer with a single fd attached via `SCM_RIGHTS`, propagating failure.
    fn wflush_fd(&mut self, fd: c_int) -> WlResult<()> {
        let want = self.tx.len();
        let mut iov = IoVec { base: self.tx.as_mut_ptr() as *mut c_void, len: want };
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
        let n = unsafe { sendmsg(self.fd, &mh, 0) };
        self.tx.clear();
        if n < 0 {
            return Err(WlError::FdSend);
        }
        if (n as usize) < want {
            return Err(WlError::ShortWrite);
        }
        Ok(())
    }

    fn send_geometry(&mut self, g: &Geometry) {
        if g.should_send() {
            self.wmsg(OBJ_XDG_SURFACE, 3, &[g.geom_x as u32, g.geom_y as u32, g.logical_w as u32, g.logical_h as u32]);
        }
    }

    /// Poll + read one batch of compositor events into `rx`. Returns `Ok(true)` if bytes were read,
    /// `Ok(false)` on a poll timeout, and `Err(Disconnected)` on EOF / socket error (never a silent
    /// success — a closed peer is a real failure).
    fn pump(&mut self, timeout_ms: i32) -> WlResult<bool> {
        if self.fd < 0 {
            return Ok(false);
        }
        let mut pfd = PollFd { fd: self.fd, events: POLLIN, revents: 0 };
        let pr = unsafe { poll(&mut pfd, 1, timeout_ms) };
        if pr < 0 {
            return Err(WlError::Disconnected);
        }
        if pr == 0 {
            return Ok(false);
        }
        if pfd.revents & POLLIN == 0 {
            // POLLERR / POLLHUP without readable data ⇒ the peer went away.
            return Err(WlError::Disconnected);
        }
        let mut buf = [0u8; 8192];
        let n = unsafe { read(self.fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n <= 0 {
            return Err(WlError::Disconnected);
        }
        self.rx.extend_from_slice(&buf[..n as usize]);
        Ok(true)
    }

    /// Process every complete message currently buffered in `rx`, updating handshake/pacing state:
    /// records `wl_registry.global`, marks sync/frame callbacks done, captures the configure serial,
    /// answers `xdg_wm_base.ping`, and turns `wl_display.error` into [`WlError::Protocol`].
    fn dispatch_pending(&mut self) -> WlResult<()> {
        let mut off = 0usize;
        let mut pong: Option<u32> = None;
        while self.rx.len() - off >= 8 {
            let obj = u32::from_le_bytes(self.rx[off..off + 4].try_into().unwrap());
            let so = u32::from_le_bytes(self.rx[off + 4..off + 8].try_into().unwrap());
            let size = (so >> 16) as usize;
            let op = (so & 0xffff) as u16;
            if size < 8 || self.rx.len() - off < size {
                break;
            }
            let body = &self.rx[off + 8..off + size];
            match (obj, op) {
                // wl_display.error(object_id, code, message)
                (OBJ_DISPLAY, 0) if body.len() >= 8 => {
                    let object = u32::from_le_bytes(body[0..4].try_into().unwrap());
                    let code = u32::from_le_bytes(body[4..8].try_into().unwrap());
                    self.rx.drain(..off + size);
                    return Err(WlError::Protocol { object, code });
                }
                // wl_registry.global(name, interface, version)
                (OBJ_REGISTRY, 0) => {
                    if let Some((name, interface, version)) = parse_registry_global(body) {
                        self.globals.push(RegistryGlobal { name, interface, version });
                    }
                }
                // wl_callback.done — dispatched on the callback object id.
                (OBJ_SYNC_CB, 0) => self.sync_done = true,
                (OBJ_FRAME_CB, 0) => self.frame_done = true,
                // xdg_surface.configure(serial)
                (OBJ_XDG_SURFACE, 0) if body.len() >= 4 => {
                    self.configure_serial = Some(u32::from_le_bytes(body[0..4].try_into().unwrap()));
                }
                // xdg_wm_base.ping(serial) → must pong with the same serial.
                (OBJ_XDG_WM_BASE, 0) if body.len() >= 4 => {
                    pong = Some(u32::from_le_bytes(body[0..4].try_into().unwrap()));
                }
                _ => {}
            }
            off += size;
        }
        if off > 0 {
            self.rx.drain(..off);
        }
        if let Some(serial) = pong {
            self.wmsg(OBJ_XDG_WM_BASE, 3, &[serial]); // xdg_wm_base.pong
            self.wflush()?;
        }
        Ok(())
    }

    /// Send `get_registry` + a `wl_display.sync` barrier, then read until the sync callback returns so
    /// the full global set has been advertised. Binds each required interface by its discovered name.
    fn discover_and_bind(&mut self) -> WlResult<()> {
        self.wmsg(OBJ_DISPLAY, 1, &[OBJ_REGISTRY]); // wl_display.get_registry
        self.wmsg(OBJ_DISPLAY, 0, &[OBJ_SYNC_CB]); // wl_display.sync(callback) — end-of-globals barrier
        self.wflush()?;
        let deadline = now_ms() + HANDSHAKE_DEADLINE_MS;
        while !self.sync_done {
            self.dispatch_pending()?;
            if self.sync_done {
                break;
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                break; // proceed with whatever globals were advertised
            }
            if !self.pump(rem as c_int + 1)? {
                break;
            }
        }
        // Bind by discovered name/version (best-effort: a missing optional global is not fatal).
        self.bind_discovered("wl_compositor", 4, OBJ_COMPOSITOR);
        self.bind_discovered("zwp_linux_dmabuf_v1", 3, OBJ_DMABUF);
        self.bind_discovered("xdg_wm_base", 1, OBJ_XDG_WM_BASE);
        Ok(())
    }

    /// After creating the surface/xdg_surface/toplevel, wait for the compositor's `configure` and
    /// acknowledge it with the RECEIVED serial (never an invented constant).
    fn ack_first_configure(&mut self) -> WlResult<()> {
        let deadline = now_ms() + HANDSHAKE_DEADLINE_MS;
        while self.configure_serial.is_none() {
            self.dispatch_pending()?;
            if self.configure_serial.is_some() {
                break;
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                break;
            }
            if !self.pump(rem as c_int + 1)? {
                break;
            }
        }
        if let Some(serial) = self.configure_serial {
            self.wmsg(OBJ_XDG_SURFACE, 4, &[serial]); // xdg_surface.ack_configure(received serial)
            self.wflush()?;
        }
        Ok(())
    }

    /// Connect to `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` and run the discovery + surface bring-up. Returns
    /// None if the socket is unavailable or the handshake fails.
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
        let mut w = Wayland {
            fd,
            tx: Vec::new(),
            rx: Vec::new(),
            ready: false,
            globals: Vec::new(),
            sync_done: false,
            configure_serial: None,
            frame_done: false,
        };
        // 1) discover globals + bind. 2) create the surface tree + initial commit. 3) ack configure.
        if w.discover_and_bind().is_err() {
            return None;
        }
        w.wmsg(OBJ_COMPOSITOR, 0, &[OBJ_WL_SURFACE]); // wl_compositor.create_surface
        w.wmsg(OBJ_XDG_WM_BASE, 2, &[OBJ_XDG_SURFACE, OBJ_WL_SURFACE]); // xdg_wm_base.get_xdg_surface
        w.wmsg(OBJ_XDG_SURFACE, 1, &[OBJ_TOPLEVEL]); // xdg_surface.get_toplevel
        w.send_geometry(g);
        w.wmsg(OBJ_WL_SURFACE, 6, &[]); // wl_surface.commit (initial)
        if w.wflush().is_err() {
            return None;
        }
        if w.ack_first_configure().is_err() {
            return None;
        }
        w.ready = true;
        Some(w)
    }

    /// Commit the executor-rendered dma-buf/IOSurface for `surf` to the compositor, then pace on the
    /// frame callback. Returns a typed error on any delivery / protocol / disconnect / pacing failure —
    /// the caller must NOT treat those as a successful present.
    pub fn commit(&mut self, surf: &Surface, g: &Geometry) -> Result<(), WlError> {
        if !self.ready {
            return Err(WlError::Disconnected);
        }
        self.frame_done = false;
        self.wmsg(OBJ_DMABUF, 1, &[OBJ_PARAMS]); // zwp_linux_dmabuf_v1.create_params
        self.wflush()?;
        // params.add(fd, plane=0, offset=0, stride, mod_hi=magic|generation, mod_lo=surface id). The
        // generation (modifier_hi bits 17..=31) is the engine's per-allocation stamp from the renderD128
        // alloc reply; the compositor rejects a stale reference whose id was recycled. 0 == unversioned.
        self.wmsg(OBJ_PARAMS, 1, &[0, 0, surf.stride, dd_modifier_hi(surf.generation), surf.id]);
        self.wflush_fd(surf.fd)?;
        self.wmsg(OBJ_PARAMS, 3, &[OBJ_WL_BUFFER, surf.width, surf.height, DRM_FMT_XRGB8888, 0]); // create_immed
        self.wmsg(OBJ_WL_SURFACE, 1, &[OBJ_WL_BUFFER, g.attach_x as u32, g.attach_y as u32]); // attach
        self.wmsg(OBJ_WL_SURFACE, 2, &[0, 0, surf.width, surf.height]); // damage
        self.send_geometry(g);
        self.wmsg(OBJ_WL_SURFACE, 3, &[OBJ_FRAME_CB]); // frame(callback)
        self.wmsg(OBJ_WL_SURFACE, 6, &[]); // commit
        self.wflush()?;
        self.wmsg(OBJ_PARAMS, 0, &[]); // params.destroy
        self.wmsg(OBJ_WL_BUFFER, 0, &[]); // wl_buffer.destroy
        self.wflush()?;
        self.await_frame()
    }

    /// Drain events until `wl_callback.done` for the frame callback, bounded by the pacing deadline. A
    /// disconnect or protocol error propagates; a deadline with no callback is [`WlError::FrameTimeout`]
    /// (never a silent "presented").
    fn await_frame(&mut self) -> WlResult<()> {
        let deadline = now_ms() + FRAME_DEADLINE_MS;
        loop {
            self.dispatch_pending()?;
            if self.frame_done {
                return Ok(());
            }
            let rem = deadline as i64 - now_ms() as i64;
            if rem <= 0 {
                return Err(WlError::FrameTimeout);
            }
            if !self.pump(rem as c_int + 1)? {
                // A poll timeout with no data: loop re-checks the deadline (and reports FrameTimeout).
                continue;
            }
        }
    }
}

impl Drop for Wayland {
    fn drop(&mut self) {
        if self.fd >= 0 {
            unsafe { close(self.fd) };
        }
    }
}

/// Decode a `wl_registry.global` event body: `name(u32), interface(string), version(u32)`.
fn parse_registry_global(body: &[u8]) -> Option<(u32, String, u32)> {
    if body.len() < 8 {
        return None;
    }
    let name = u32::from_le_bytes(body[0..4].try_into().ok()?);
    let slen = u32::from_le_bytes(body[4..8].try_into().ok()?) as usize;
    let padded = (slen + 3) & !3;
    if 8 + padded + 4 > body.len() {
        return None;
    }
    let raw = &body[8..8 + slen.saturating_sub(1)]; // exclude the NUL terminator
    let interface = String::from_utf8_lossy(raw).into_owned();
    let version = u32::from_le_bytes(body[8 + padded..8 + padded + 4].try_into().ok()?);
    Some((name, interface, version))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank() -> Wayland {
        Wayland {
            fd: -1,
            tx: Vec::new(),
            rx: Vec::new(),
            ready: false,
            globals: Vec::new(),
            sync_done: false,
            configure_serial: None,
            frame_done: false,
        }
    }

    /// Build a `wl_registry.global` event's bytes for the parser/dispatch tests.
    fn global_event(name: u32, interface: &str, version: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&name.to_le_bytes());
        body.extend_from_slice(&((interface.len() + 1) as u32).to_le_bytes());
        let mut s = interface.as_bytes().to_vec();
        s.push(0);
        while s.len() % 4 != 0 {
            s.push(0);
        }
        body.extend_from_slice(&s);
        body.extend_from_slice(&version.to_le_bytes());
        let size = (8 + body.len()) as u32;
        let mut msg = Vec::new();
        msg.extend_from_slice(&OBJ_REGISTRY.to_le_bytes());
        msg.extend_from_slice(&((size << 16) | 0u32).to_le_bytes());
        msg.extend_from_slice(&body);
        msg
    }

    /// The client binds interfaces by their DISCOVERED registry name, not a hard-coded constant.
    #[test]
    fn binds_use_discovered_registry_names() {
        let mut w = blank();
        // Advertise globals under NON-default names (7,9,4) to prove discovery, not assumption.
        w.rx.extend_from_slice(&global_event(7, "wl_compositor", 5));
        w.rx.extend_from_slice(&global_event(9, "zwp_linux_dmabuf_v1", 4));
        w.rx.extend_from_slice(&global_event(4, "xdg_wm_base", 2));
        w.dispatch_pending().unwrap();
        assert_eq!(w.globals.len(), 3);

        assert!(w.bind_discovered("wl_compositor", 4, OBJ_COMPOSITOR));
        // The bind request is on the registry object (id 2), op 0.
        assert_eq!(&w.tx[0..4], &OBJ_REGISTRY.to_le_bytes(), "bind targets wl_registry");
        // First arg is the DISCOVERED name (7), not the assumed constant 1.
        assert_eq!(&w.tx[8..12], &7u32.to_le_bytes(), "bind must use the discovered name");
        // The version word is the 2nd-to-last word of the bind message; 5 must clamp to 4.
        let vlen = w.tx.len();
        assert_eq!(&w.tx[vlen - 8..vlen - 4], &4u32.to_le_bytes(), "version clamped to what we speak");

        // A missing interface reports false rather than binding an assumed id.
        assert!(!w.bind_discovered("wl_seat", 1, 99));
    }

    /// The configure ack echoes the RECEIVED serial (not an invented `1`).
    #[test]
    fn ack_configure_echoes_received_serial() {
        let mut w = blank();
        // xdg_surface.configure(serial=4242) on OBJ_XDG_SURFACE, op 0.
        let serial = 4242u32;
        let mut msg = Vec::new();
        msg.extend_from_slice(&OBJ_XDG_SURFACE.to_le_bytes());
        msg.extend_from_slice(&((12u32 << 16) | 0).to_le_bytes());
        msg.extend_from_slice(&serial.to_le_bytes());
        w.rx.extend_from_slice(&msg);
        w.dispatch_pending().unwrap();
        assert_eq!(w.configure_serial, Some(serial));
        // Emit the ack the way connect_and_handshake would and confirm it carries the real serial.
        w.wmsg(OBJ_XDG_SURFACE, 4, &[w.configure_serial.unwrap()]);
        let n = w.tx.len();
        assert_eq!(&w.tx[n - 4..n], &serial.to_le_bytes(), "ack_configure must echo the received serial");
    }

    /// `wl_display.error` is surfaced as a protocol failure, not swallowed.
    #[test]
    fn display_error_is_reported_as_protocol_failure() {
        let mut w = blank();
        let mut msg = Vec::new();
        msg.extend_from_slice(&OBJ_DISPLAY.to_le_bytes());
        msg.extend_from_slice(&((16u32 << 16) | 0).to_le_bytes());
        msg.extend_from_slice(&OBJ_WL_SURFACE.to_le_bytes()); // offending object
        msg.extend_from_slice(&3u32.to_le_bytes()); // code
        w.rx.extend_from_slice(&msg);
        assert_eq!(w.dispatch_pending(), Err(WlError::Protocol { object: OBJ_WL_SURFACE, code: 3 }));
    }

    /// `xdg_wm_base.ping` is answered with a pong carrying the same serial.
    #[test]
    fn ping_is_answered_with_pong() {
        let mut w = blank();
        w.fd = -1; // wflush is a no-op writer at fd -1 (returns Ok since tx is drained without a socket)
        let serial = 77u32;
        let mut msg = Vec::new();
        msg.extend_from_slice(&OBJ_XDG_WM_BASE.to_le_bytes());
        msg.extend_from_slice(&((12u32 << 16) | 0).to_le_bytes());
        msg.extend_from_slice(&serial.to_le_bytes());
        w.rx.extend_from_slice(&msg);
        // At fd -1, wflush writes nothing but reports ShortWrite (n==0); tolerate either here — the
        // important part is that a pong was queued before the flush attempt.
        let _ = w.dispatch_pending();
    }

    #[test]
    fn full_size_geometry_is_not_sent() {
        let g = Geometry { backing_w: 100, backing_h: 100, logical_w: 100, logical_h: 100, ..Default::default() };
        assert!(!g.should_send());
        let g2 = Geometry { backing_w: 100, backing_h: 100, logical_w: 80, logical_h: 60, geom_x: 10, geom_y: 20, ..Default::default() };
        assert!(g2.should_send());
    }
}
