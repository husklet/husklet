use super::*;

// ==================================================================================================
// live wl_shm present client (self-contained connection)
// ==================================================================================================

/// Surface geometry the shim advertises to the compositor (logical size + attach offset).
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
    /// A backing-sized geometry (logical == backing, no offset).
    pub fn backing(w: u32, h: u32) -> Self {
        Geometry {
            backing_w: w,
            backing_h: h,
            logical_w: w as i32,
            logical_h: h as i32,
            ..Default::default()
        }
    }

    /// Whether `xdg_surface.set_window_geometry` differs from the backing rectangle and should be sent.
    pub(super) fn should_send(&self) -> bool {
        self.logical_w > 0
            && self.logical_h > 0
            && !(self.logical_w == self.backing_w as i32
                && self.logical_h == self.backing_h as i32
                && self.geom_x == 0
                && self.geom_y == 0)
    }
}

/// A typed outcome for the fallible wayland present (a commit/handshake/pacing failure must never look
/// like a successful frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WlError {
    /// The compositor closed the connection (EOF / write error) — the frame was not delivered.
    Disconnected,
    /// A socket write could not deliver the whole message.
    ShortWrite,
    /// Passing the shm fd (`sendmsg`/`SCM_RIGHTS`) failed.
    FdSend,
    /// Allocating / mapping the shared-memory pool failed.
    ShmAlloc,
    /// A required global (`wl_compositor`/`wl_shm`/`xdg_wm_base`) was not advertised.
    MissingGlobal,
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

// ---- client-assigned wayland object ids (our id space; server names are discovered) ----
pub(super) const OBJ_DISPLAY: u32 = 1;
pub(super) const OBJ_REGISTRY: u32 = 2;
pub(super) const OBJ_SYNC_CB: u32 = 3;
pub(super) const OBJ_COMPOSITOR: u32 = 4;
pub(super) const OBJ_SHM: u32 = 5;
pub(super) const OBJ_XDG_WM_BASE: u32 = 6;
pub(super) const OBJ_WL_SURFACE: u32 = 7;
pub(super) const OBJ_XDG_SURFACE: u32 = 8;
pub(super) const OBJ_TOPLEVEL: u32 = 9;
pub(super) const OBJ_SHM_POOL: u32 = 10;
pub(super) const OBJ_WL_BUFFER: u32 = 11;
pub(super) const OBJ_FRAME_CB: u32 = 12;

/// `WL_SHM_FORMAT_XRGB8888` (the byte order [`rgba_to_xrgb8888`] packs into).
pub(super) const WL_SHM_FORMAT_XRGB8888: u32 = 1;

/// How long to wait for the compositor's per-frame callback before reporting a pacing failure.
pub(super) const FRAME_DEADLINE_MS: u64 = 100;
/// Bound on the initial registry/configure handshake reads.
pub(super) const HANDSHAKE_DEADLINE_MS: u64 = 400;

// ---- minimal libc surface (dependency-free) ----
#[repr(C)]
pub(super) struct IoVec {
    pub(super) base: *mut c_void,
    pub(super) len: usize,
}
#[repr(C)]
pub(super) struct MsgHdr {
    pub(super) name: *mut c_void,
    pub(super) namelen: u32,
    pub(super) _pad0: u32,
    pub(super) iov: *mut IoVec,
    pub(super) iovlen: usize,
    pub(super) control: *mut c_void,
    pub(super) controllen: usize,
    pub(super) flags: c_int,
}
#[repr(C)]
pub(super) struct PollFd {
    pub(super) fd: c_int,
    pub(super) events: i16,
    pub(super) revents: i16,
}
extern "C" {
    pub(super) fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    pub(super) fn connect(fd: c_int, addr: *const c_void, len: u32) -> c_int;
    pub(super) fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    pub(super) fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
    pub(super) fn sendmsg(fd: c_int, msg: *const MsgHdr, flags: c_int) -> isize;
    pub(super) fn poll(fds: *mut PollFd, n: u64, timeout: c_int) -> c_int;
    pub(super) fn close(fd: c_int) -> c_int;
    pub(super) fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        off: i64,
    ) -> *mut c_void;
    pub(super) fn munmap(addr: *mut c_void, len: usize) -> c_int;
}

pub(super) const AF_UNIX: c_int = 1;
pub(super) const SOCK_STREAM: c_int = 1;
pub(super) const SOL_SOCKET: c_int = 1;
pub(super) const SCM_RIGHTS: c_int = 1;
pub(super) const POLLIN: i16 = 0x001;
pub(super) const PROT_READ: c_int = 1;
pub(super) const PROT_WRITE: c_int = 2;
pub(super) const MAP_SHARED: c_int = 1;
pub(super) const MAP_FAILED: isize = -1;

/// Decode a `wl_registry.global` event body: `name(u32), interface(string), version(u32)`.
pub(super) struct RegistryEvent;
impl RegistryEvent {
    pub(super) fn parse(body: &[u8]) -> Option<(u32, String, u32)> {
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
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
