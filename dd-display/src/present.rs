//! Presenters: what the compositor does with a committed surface. The trait is the seam between the
//! (portable) protocol core and the (platform) window backend.
//!
//! - [`PngPresenter`] dumps each committed surface to a PNG — the headless proof of the whole CPU path
//!   (memfd → wl_shm pool → `SCM_RIGHTS` → mmap → framebuffer), verifiable on the Linux dev host with no
//!   GPU or display. This is how `dd-term-core` proves its renderer, applied to the display pipeline.
//! - The Cocoa presenter (see `present_cocoa.rs`, macOS only) opens a native `NSWindow`+`CALayer` per
//!   surface and blits the same BGRA framebuffer via `CALayer.contents`.

/// A committed surface's pixels, unpacked to a tight BGRA framebuffer (wl_shm ARGB8888/XRGB8888 is
/// little-endian, i.e. memory order B,G,R,A — the same order Metal's `BGRA8Unorm` wants).
pub struct SurfaceBuffer {
    pub sid: u32,
    /// Logical surface size after Wayland viewport/buffer transforms. Presenters size the native output
    /// to this, not necessarily to the backing texture dimensions.
    pub width: i32,
    pub height: i32,
    /// Backing texture size. For CPU buffers this matches `width/height`; for IOSurface/dmabuf it can be
    /// larger when `wp_viewport` crops/scales Chrome's render target into a logical surface.
    pub texture_width: i32,
    pub texture_height: i32,
    pub stride: i32, // tight: width*4
    pub format: u32,
    pub bgra: Vec<u8>,
    pub title: String,
    /// GPU rung 2: when `Some(id)`, this surface's pixels live in a host `IOSurface` (id) — the
    /// compositor wraps it as an `MTLTexture` and composites zero-copy instead of reading `bgra`.
    pub iosurface_id: Option<u32>,
    /// GPU rung 3 (first slice): the guest requested the host GPU to *render* into this IOSurface (a
    /// forwarded render command). The Metal presenter runs a render pass before compositing.
    pub gpu_render: bool,
    /// Normalized source rectangle in the backing texture, `(u0, v0, u1, v1)`.
    pub uv_rect: [f32; 4],
}

impl SurfaceBuffer {
    /// Convert to RGBA (swap B/R, force opaque alpha for XRGB) for PNG encoding / inspection.
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.bgra.len()];
        for i in (0..self.bgra.len()).step_by(4) {
            out[i] = self.bgra[i + 2]; // R <- ...
            out[i + 1] = self.bgra[i + 1]; // G
            out[i + 2] = self.bgra[i]; // B
            out[i + 3] = if self.format == 1 {
                0xff
            } else {
                self.bgra[i + 3]
            }; // XRGB ⇒ opaque
        }
        out
    }
}

pub trait Presenter {
    /// A surface has committed a new frame. `surf` aliases nothing — it's a fresh snapshot. Returns
    /// `true` if the frame actually reached the screen; `false` if the present was skipped (e.g. an
    /// IOSurface lookup or drawable acquisition failed). The compositor uses this to decide whether to
    /// release the buffer and fire frame callbacks — a failed present must NOT advance frame pacing.
    fn present(&mut self, surf: &SurfaceBuffer) -> bool;
    /// Number of frames presented so far (for a generic multiplexer's disconnect log). Default 0.
    fn frame_count(&self) -> u32 {
        0
    }
    /// The on-screen size (w,h) of the window backing surface `sid`, if the presenter has one. The live
    /// input path uses this to flip Cocoa's bottom-left `locationInWindow` into top-left surface space.
    fn surface_size(&self, _sid: u32) -> Option<(i32, i32)> {
        None
    }
    /// Dump every live window's current contents to `<dir>/live-surface-<sid>.png`, returning how many
    /// were written. The live `--window` loop calls this on `SIGUSR1` so a headless driver can Read back
    /// what is actually on screen (there is no way to screen-record the Mac). Default: nothing to dump.
    fn dump_pngs(&self, _dir: &str) -> usize {
        0
    }
    /// If `win_ptr` (an `NSWindow*`) is one of this presenter's live windows, return the surface id it
    /// backs. The multi-client live loop uses this to route an `NSEvent` to the client that owns the
    /// window the event targeted. Default: this presenter owns no native windows.
    fn window_ptr_to_sid(&self, _win_ptr: *const std::ffi::c_void) -> Option<u32> {
        None
    }
    /// The LIVE on-screen content size (w,h) of the window backing `sid`, read from AppKit. This differs
    /// from `surface_size` (the last committed buffer size) after the user drags the window edge — the
    /// live loop feeds it to `Server::maybe_resize` to emit an `xdg_toplevel.configure`. Default: none.
    fn window_content_size(&self, _sid: u32) -> Option<(i32, i32)> {
        None
    }
    /// AppKit backing scale for a native window. Input events arrive in points, while Wayland clients in
    /// this compositor use pixel-space surface coordinates.
    fn surface_scale(&self, _sid: u32) -> f64 {
        1.0
    }
    /// Integer output scale (`wl_output.scale`) the compositor advertises. Chrome/GTK commit a buffer of
    /// `logical_size * scale` when this is >1, so on a Retina display returning 2 makes the client render
    /// a crisp HiDPI buffer instead of a 1x buffer stretched to fill the backing store. Default 1 (the
    /// headless/PngPresenter path and any SDR display). See weston `weston_output::current_scale` and
    /// wlroots `wlr_output.scale`; both feed exactly this value into the `scale` event.
    fn output_scale(&self) -> i32 {
        1
    }
    /// Destroy the native window backing surface `sid`, if one exists. Used when a surface turns out not to
    /// be a window after all — e.g. it was assigned the cursor role via `wl_pointer.set_cursor` after its
    /// image was already committed (and thus already shown in a tiny window). Default: nothing to remove.
    fn drop_window(&mut self, _sid: u32) {}
    /// The Wayland client issued `xdg_toplevel.move` for surface `sid` (a user-initiated window drag). A
    /// windowed presenter should start a native, host-driven move of that window. This is invoked ONLY when
    /// the client actually requests a move, so it is the precise alternative to making every window blindly
    /// movable-by-background. Default: no native window to move.
    fn begin_interactive_move(&self, _sid: u32) {}
}

/// Writes each committed surface to `<dir>/surface-<sid>.png`, and records the last frame for tests.
pub struct PngPresenter {
    dir: std::path::PathBuf,
    pub last: Option<(u32, i32, i32, Vec<u8>)>, // (sid, w, h, rgba) — asserted by the headless self-test
    pub frames: u32,
}

impl PngPresenter {
    pub fn new(dir: impl Into<std::path::PathBuf>) -> PngPresenter {
        PngPresenter {
            dir: dir.into(),
            last: None,
            frames: 0,
        }
    }
}

impl Presenter for PngPresenter {
    fn frame_count(&self) -> u32 {
        self.frames
    }
    fn present(&mut self, surf: &SurfaceBuffer) -> bool {
        let rgba = surf.to_rgba();
        let png = dd_term_core::png::encode_rgba(surf.width as u32, surf.height as u32, &rgba);
        let _ = std::fs::create_dir_all(&self.dir);
        let path = self.dir.join(format!("surface-{}.png", surf.sid));
        let _ = std::fs::write(&path, png);
        self.frames += 1;
        self.last = Some((surf.sid, surf.width, surf.height, rgba));
        eprintln!(
            "[dd-display] present sid={} {}x{} title={:?} -> {}",
            surf.sid,
            surf.width,
            surf.height,
            surf.title,
            path.display()
        );
        true
    }
}
