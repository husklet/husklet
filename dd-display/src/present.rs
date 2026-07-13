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
    /// The region of the backing texture this commit actually changed, as `(x, y, w, h)` in backing
    /// texture pixels. `None` means the whole texture is new (a fresh buffer, a resize, or the first
    /// upload) and must be uploaded in full. When `Some`, everything outside the rectangle is byte-for-
    /// byte identical to the previously presented frame, so a presenter MAY upload only that sub-region
    /// to its Metal/IOSurface texture (`wl_surface.damage`/`damage_buffer` honoured). This is a pure
    /// upload hint: `bgra` always holds the complete, correct frame, so a presenter that ignores
    /// `damage` (e.g. `PngPresenter`) stays pixel-identical to the full-upload path.
    pub damage: Option<(i32, i32, i32, i32)>,
    /// If this surface is an `xdg_popup`, where to place its native window: the parent surface it is
    /// anchored to plus the positioner-resolved `(x, y)` offset from the parent's window-geometry top-left
    /// (logical points, y-down). A windowed presenter opens the popup's window at
    /// parent-content-top-left + (x, y) so a menu/combobox dropdown appears AT the widget instead of at a
    /// default cascade position. `None` for toplevels and any surface with no positioner.
    pub popup: Option<PopupPlacement>,
    /// Overlay layers to composite ON TOP of this surface, bottom-to-top, when this surface is a GPU
    /// (IOSurface) root whose pixels are not CPU-addressable. A CPU (`wl_shm`) root pre-composites its
    /// subsurfaces/popups into `bgra` and leaves this empty; a GPU root instead carries each subsurface
    /// and popup here as a [`GpuCompositeNode`] (a `wl_shm` layer + its device-pixel offset) so the
    /// presenter draws the mixed shm/IOSurface tree — the IOSurface base plus each overlay on top —
    /// instead of losing the child surfaces. See `dd-compositor`'s `present_tree`.
    pub overlays: Vec<GpuCompositeNode>,
}

/// One overlay layer in a mixed shm/IOSurface present tree: a composited child surface (`buffer`, a
/// `wl_shm` [`SurfaceBuffer`] with `iosurface_id == None`) and its offset within the window root, in the
/// root's backing-texture (device) pixels. Emitted only for GPU roots (see [`SurfaceBuffer::overlays`]);
/// the presenter uploads `buffer` and alpha-composites it at `(x, y)` over the resolved IOSurface base.
pub struct GpuCompositeNode {
    pub buffer: SurfaceBuffer,
    pub x: i32,
    pub y: i32,
}

/// Placement for an `xdg_popup`'s native window: which parent surface it hangs off and the
/// positioner-resolved offset from that parent's window-geometry top-left (logical points, y-down).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PopupPlacement {
    pub parent_sid: u32,
    pub x: i32,
    pub y: i32,
}

/// Presentation timing evidence for a delivered frame — the host's monotonic present time and the
/// output's refresh interval, both in nanoseconds. Carried by [`PresentOutcome::Delivered`] so the
/// compositor can answer `wp_presentation` feedback with real (not invented) timing when the backend
/// knows it. `None` timing on `Delivered` means the frame reached the screen but the backend did not
/// report a hardware present time (the compositor then falls back to its own monotonic clock).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PresentTiming {
    /// Host monotonic time the frame became visible, in nanoseconds.
    pub present_ns: u64,
    /// Output refresh interval in nanoseconds (`0` = unknown / variable).
    pub refresh_ns: u64,
}

/// Metadata read from a live host IOSurface allocation before accepting a guest import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IOSurfaceMetadata {
    pub width: u32,
    pub height: u32,
    pub bytes_per_row: u32,
    pub pixel_format: u32,
}

/// The structured result of a [`Presenter::present`] call. Replaces the old `bool` that conflated
/// "rendered somewhere" with "visibly on screen" and silently swallowed output errors: a presenter now
/// says exactly what happened to the frame, and real output/device/filesystem failures propagate as the
/// `Err` half of [`Presenter::present`]'s `Result` instead of masquerading as success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PresentOutcome {
    /// The frame was visibly delivered to the display. `serial` is a monotonically increasing per-frame
    /// pacing counter (the compositor uses it to order/track delivered frames); `timing` is the optional
    /// hardware present-time evidence.
    Delivered {
        serial: u64,
        timing: Option<PresentTiming>,
    },
    /// The frame was rendered into an offscreen / backing target (e.g. a GPU IOSurface render pass) but
    /// was NOT visibly presented this cycle. Not an error — but the client's frame did not reach the
    /// screen, so the compositor must not advance that surface's frame pacing as if it had.
    Offscreen,
}

/// An output/device/filesystem error from a presenter — the failures the old `bool` return hid. Carried
/// up as the `Err` half of [`Presenter::present`]'s `Result` so the compositor can log/retry instead of
/// treating a dropped frame as a successful present.
#[derive(Debug)]
pub enum PresentError {
    /// The presenter's output sink failed (a PNG/file write, an I/O error).
    Output(std::io::Error),
    /// The host display/device rejected the frame: no drawable acquired, device lost, or a referenced
    /// IOSurface could not be resolved to a live host texture.
    Device(String),
}

impl std::fmt::Display for PresentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresentError::Output(e) => write!(f, "present output error: {e}"),
            PresentError::Device(m) => write!(f, "present device error: {m}"),
        }
    }
}

impl std::error::Error for PresentError {}

impl From<std::io::Error> for PresentError {
    fn from(e: std::io::Error) -> Self {
        PresentError::Output(e)
    }
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
    /// A surface has committed a new frame. `surf` aliases nothing — it's a fresh snapshot. Returns a
    /// structured [`PresentOutcome`] on success (`Delivered` with a pacing serial + optional timing when
    /// the frame reached the screen, `Offscreen` when it was rendered to a backing target but not shown),
    /// or a [`PresentError`] when the output/device/filesystem failed. The compositor uses this to decide
    /// whether to advance frame pacing: only a `Delivered` frame fires `wl_surface.frame` callbacks and a
    /// `presented` feedback; an `Offscreen`/`Err` present must NOT advance pacing (the client would think
    /// its frame shipped and recycle a buffer the compositor still needs to retry).
    fn present(&mut self, surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError>;
    /// Resolve and describe a live IOSurface allocation. `None` means the id is stale, unavailable, or
    /// this presenter cannot authenticate IOSurface-backed imports.
    fn iosurface_metadata(&self, _id: u32) -> Option<IOSurfaceMetadata> {
        None
    }
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
    /// The Wayland client issued `xdg_toplevel.resize` for surface `sid` (a user-initiated window edge/corner
    /// drag). `edges` is the `xdg_toplevel.resize_edge` bitmask (top=1, bottom=2, left=4, right=8; a corner is
    /// the OR of two, e.g. bottom_right=10). A windowed presenter should start a native, host-driven resize of
    /// that window, anchoring the opposite edge so the requested edge/corner tracks the pointer. Invoked ONLY
    /// on an explicit client request. Default: no native window to resize.
    fn begin_interactive_resize(&self, _sid: u32, _edges: u32) {}
    /// The client asked (via `xdg_activation_v1`) to have surface `sid` focused/raised — e.g. a launcher
    /// activating the window it spawned, or an app raising itself. A windowed presenter should bring that
    /// window to the front (and, if appropriate, make it key). Default: no native window to raise.
    fn raise_window(&self, _sid: u32) {}
    /// The client picked a themed pointer shape via `wp_cursor_shape_device_v1.set_shape`. `shape` is the
    /// `wp_cursor_shape_device_v1.shape` enum (1=default, 4=pointer, 9=text, 16=grab, …). A windowed
    /// presenter maps it to the matching host cursor (e.g. `NSCursor`). Default: no native cursor to set.
    fn set_cursor_shape(&self, _shape: u32) {}
    /// The client committed a CUSTOM cursor image via `wl_pointer.set_cursor` (surface + buffer + hotspot):
    /// a CSS `cursor: url(...)`, a custom app cursor, a game's crosshair — anything `wp_cursor_shape`'s named
    /// set cannot express. `bgra` is the cursor buffer's tight BGRA pixels (`width`×`height`, one row every
    /// `width*4` bytes, little-endian ARGB8888 memory order B,G,R,A); `(hotspot_x, hotspot_y)` is the click
    /// point IN THOSE PIXELS. A windowed presenter turns it into a host cursor (`NSCursor` from an `NSImage`).
    /// The compositor calls this in place of `set_cursor_shape` for the duration of the custom cursor, and
    /// re-calls it whenever the client re-commits the cursor surface (animated/updated cursors). Default: no
    /// native cursor to set. Mirrors `set_cursor_shape`, keeping the Smithay core free of any Cocoa.
    fn set_cursor_buffer(
        &self,
        _bgra: &[u8],
        _width: i32,
        _height: i32,
        _hotspot_x: i32,
        _hotspot_y: i32,
    ) {
    }
    /// Hide or show the host pointer. Driven by `wl_pointer.set_cursor` with a null surface
    /// (`CursorImageStatus::Hidden` — a client that draws its own cursor or wants none), and by
    /// `zwp_pointer_constraints` pointer LOCK (an FPS/3D app hides the system cursor while it reads relative
    /// motion). Idempotent: a windowed presenter must not stack hide/show so a single show reveals the
    /// cursor regardless of how many hides preceded it. Default: no native cursor to toggle.
    fn set_cursor_hidden(&self, _hidden: bool) {}

    // ---- Host clipboard bridge (wl_data_device selection ⇄ host pasteboard) ----
    // These four hooks are the platform seam for bridging the guest's Wayland clipboard to the host's
    // native clipboard (macOS `NSPasteboard`), added the same way `set_cursor_shape` was: a default-no-op
    // on the trait, implemented only by the windowed (Cocoa) presenter, so the Smithay compositor core
    // stays free of any Cocoa. The headless `PngPresenter`/test presenters keep the defaults, so the whole
    // data-device path is exercised in-process without a host clipboard.

    /// A guest set the Wayland selection (copy). `bytes` is the guest source's payload for `mime`, already
    /// read by the compositor; push it onto the host clipboard so a host app can paste it. Default: drop.
    fn clipboard_set_host(&self, _mime: &str, _bytes: &[u8]) {}
    /// The mime types the host clipboard currently offers, so the compositor can advertise a host→guest
    /// selection (`wl_data_offer.offer` per type). Default: none (no host clipboard).
    fn clipboard_host_mimes(&self) -> Vec<String> {
        Vec::new()
    }
    /// Read the host clipboard payload for `mime` (guest paste). Returns the raw bytes the compositor
    /// writes into the reader's `wl_data_offer.receive` fd. Default: nothing on the host clipboard.
    fn clipboard_host_read(&self, _mime: &str) -> Option<Vec<u8>> {
        None
    }
    /// A change token for the host clipboard (macOS `NSPasteboard.changeCount`). It bumps whenever the host
    /// clipboard changes, so the compositor loop can re-offer the new host selection to guests without
    /// polling contents every frame. `0` means there is no host clipboard. Default: `0`.
    fn clipboard_host_generation(&self) -> u64 {
        0
    }
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
    fn present(&mut self, surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        let rgba = surf.to_rgba();
        let png = dd_term_core::png::encode_rgba(surf.width as u32, surf.height as u32, &rgba);
        // Propagate a real output failure rather than reporting a phantom success: if the directory
        // cannot be created or the PNG cannot be written, the frame did NOT reach this presenter's sink,
        // and the compositor must learn that (so it does not fire frame callbacks / release the buffer).
        std::fs::create_dir_all(&self.dir).map_err(PresentError::Output)?;
        let path = self.dir.join(format!("surface-{}.png", surf.sid));
        std::fs::write(&path, png).map_err(PresentError::Output)?;
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
        Ok(PresentOutcome::Delivered {
            serial: self.frames as u64,
            timing: None,
        })
    }
}
