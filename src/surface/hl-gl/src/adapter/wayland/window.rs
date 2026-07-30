use super::*;

// ==================================================================================================
// libwayland-egl ABI: the `wl_egl_window` handle
// ==================================================================================================

/// Magic tag in the first (`version`) field of our [`WlEglWindow`], so `eglCreateWindowSurface` can tell
/// OUR `wl_egl_window*` (created by the staged `libwayland-egl.so.1`) from a stray struct. Positive, so it
/// is a legal `intptr_t version` a stock Mesa parser would just treat as a very large version.
pub const HL_WL_EGL_MAGIC: isize = 0x686c_776c_5f65_676c; // "hlwl_egl"

/// The `wl_egl_window` the app links against (the `libwayland-egl` ABI). It is a plain
/// `wl_surface` + backing size; our libEGL reads it in `eglCreateWindowSurface`. `#[repr(C)]` with the
/// field order the staged C shim (`shim/wayland_egl.c`) allocates, so the two agree on the
/// 64-byte layout byte-for-byte (asserted in the tests + the dlopen integration test).
#[repr(C)]
pub struct WlEglWindow {
    /// `HL_WL_EGL_MAGIC` (Mesa keeps an `intptr_t version` here).
    pub version: isize,
    pub width: i32,
    pub height: i32,
    pub dx: i32,
    pub dy: i32,
    pub attached_width: i32,
    pub attached_height: i32,
    driver_private: *mut c_void,
    resize_cb: *mut c_void,
    destroy_cb: *mut c_void,
    /// The app's `wl_surface*` (an opaque `wl_proxy*` on the app's own `libwayland-client` connection).
    pub surface: *mut c_void,
}

impl WlEglWindow {
    /// `wl_egl_window_create(surface, width, height)` — a fresh window wrapping `surface` at `width`×`height`.
    pub fn new(surface: *mut c_void, width: i32, height: i32) -> Self {
        WlEglWindow {
            version: HL_WL_EGL_MAGIC,
            width,
            height,
            dx: 0,
            dy: 0,
            attached_width: 0,
            attached_height: 0,
            driver_private: core::ptr::null_mut(),
            resize_cb: core::ptr::null_mut(),
            destroy_cb: core::ptr::null_mut(),
            surface,
        }
    }

    /// `wl_egl_window_resize(w, width, height, dx, dy)` — update the backing size + attach offset.
    pub fn resize(&mut self, width: i32, height: i32, dx: i32, dy: i32) {
        self.width = width;
        self.height = height;
        self.dx = dx;
        self.dy = dy;
    }

    /// `wl_egl_window_get_attached_size` — the last-attached size (falling back to the current size).
    pub fn attached_size(&self) -> (i32, i32) {
        let w = if self.attached_width != 0 {
            self.attached_width
        } else {
            self.width
        };
        let h = if self.attached_height != 0 {
            self.attached_height
        } else {
            self.height
        };
        (w, h)
    }
}

/// The native-window geometry `eglCreateWindowSurface` extracts: the backing size + the app's `wl_surface`.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct WlWindowInfo {
    pub width: u32,
    pub height: u32,
    /// The app's `wl_surface*` as a `usize` (0 if the native window carried none).
    pub wl_surface: usize,
}

/// Parse the app's native window handle to `(width, height, wl_surface)`. Recognises OUR
/// `wl_egl_window` (the `HL_WL_EGL_MAGIC` struct the staged `libwayland-egl` hands the app) AND a REAL
/// `libwayland-egl` `wl_egl_window` (the stable `wayland-egl-backend.h` ABI a bundled/system libwayland-egl
/// — Chrome/ANGLE's — hands us). Sizes are clamped to a sane `1..=8192`, defaulting to 256.
///
/// The real ABI is `intptr_t version; int width, height, dx, dy; struct wl_surface *surface; …`, so on LP64
/// the `version` occupies the FIRST 8 bytes and the size lives at offset 8/12, with the wrapped `wl_surface*`
/// at offset 24 — NOT at offset 0/4. Reading offset 0/4 as `(width, height)` (the old heuristic) misreads a
/// real window's `version` (`WL_EGL_WINDOW_VERSION`, e.g. 3) as `width=3` and its zero high word as
/// `height→256`, which is exactly the bogus 3×256 window Chrome presented. Our OWN magic struct shares the
/// width@8/height@12 offsets but keeps its `surface` at the end, so it is handled by the magic branch.
///
/// # Safety
/// `w` must be null or point at a readable `wl_egl_window`-ABI struct (what `eglCreateWindowSurface` is
/// always handed on Wayland).
impl WlWindowInfo {
    /// # Safety
    /// `w` must be null or point to a readable `wl_egl_window` ABI value.
    pub unsafe fn parse(w: *const c_void) -> Self {
        let clamp = |v: i32| if v > 0 && v <= 8192 { v as u32 } else { 256 };
        if w.is_null() {
            return WlWindowInfo {
                width: 256,
                height: 256,
                wl_surface: 0,
            };
        }
        let version = *(w as *const isize);
        if version == HL_WL_EGL_MAGIC {
            let win = &*(w as *const WlEglWindow);
            let (mut ww, mut hh) = (win.width, win.height);
            if win.attached_width > 0
                && win.attached_height > 0
                && win.attached_width <= 8192
                && win.attached_height <= 8192
            {
                ww = ww.max(win.attached_width);
                hh = hh.max(win.attached_height);
            }
            return WlWindowInfo {
                width: clamp(ww),
                height: clamp(hh),
                wl_surface: win.surface as usize,
            };
        }
        // A REAL libwayland-egl `wl_egl_window` (Mesa / the reference wayland-egl backend). Its stable ABI puts an
        // `intptr_t version` in the first 8 bytes (LP64), then `int width;`@8, `int height;`@12, `int dx;`@16,
        // `int dy;`@20, and `struct wl_surface *surface;`@24. Read those real fields so the size is the app's
        // actual window and the wrapped `wl_surface*` drives the app-surface present path (present onto the app's
        // OWN toplevel), instead of misreading the version word as a 3×256 window.
        let words = w as *const i32;
        let width = *words.add(2); // offset 8
        let height = *words.add(3); // offset 12
        let surface = *((w as *const u8).add(24) as *const *mut c_void); // offset 24
        WlWindowInfo {
            width: clamp(width),
            height: clamp(height),
            wl_surface: surface as usize,
        }
    }
}

// ==================================================================================================
// readback → wl_shm pixel convert
// ==================================================================================================

/// Convert a `glReadPixels(GL_RGBA)` plane (tight-packed `w`×`h`, **bottom-left** origin, `[R,G,B,A]`) into
/// the `WL_SHM_FORMAT_XRGB8888` little-endian byte order a `wl_shm` buffer wants (`[B,G,R,X]` per texel,
/// **top-left** origin). The vertical flip turns GL's bottom-up scanlines into wayland's top-down ones.
///
/// The input must be a plane that already passed through `glReadPixels`, which is where the driver's single
/// GL row flip lives. The live present path does NOT go through here: it packs the render target's own
/// top-down rows straight into XRGB (`readpixels::xrgb_plane`) and flips nothing, so the two paths together
/// apply exactly one flip each and never two.
pub fn rgba_to_xrgb8888(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h * 4];
    if rgba.len() < w * h * 4 {
        return out;
    }
    for y in 0..h {
        let src_row = (h - 1 - y) * w * 4; // GL bottom-left → top-left
        let dst_row = y * w * 4;
        for x in 0..w {
            let s = src_row + x * 4;
            let d = dst_row + x * 4;
            let (r, g, b) = (rgba[s], rgba[s + 1], rgba[s + 2]);
            // XRGB8888 little-endian in memory is [B, G, R, X].
            out[d] = b;
            out[d + 1] = g;
            out[d + 2] = r;
            out[d + 3] = 0xFF;
        }
    }
    out
}
