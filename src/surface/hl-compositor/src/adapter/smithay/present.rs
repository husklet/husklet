//! [`PngPresenter`]: the headless [`Presenter`] the Smithay adapter drives.
//!
//! The neutral [`Presenter`] port carries only geometry — no pixels — so this presenter keeps a small
//! side store of the actual client pixels the adapter deposits at commit time (keyed by [`SurfaceId`]).
//! When the scene composes a frame and calls [`Presenter::present`] for the base layer, the presenter
//! looks up that surface's deposited pixels, records a [`CapturedFrame`], and (optionally) writes a real
//! `.png` to disk. A test reads the captured frames back through the shared `captures` handle and asserts
//! the client's pixels made it all the way through wl → scene → present, fully headless.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hl_log::{hl_add, tag};

use crate::scene::model::{BufferTransform, OutputId, PresentableImage, Rect, SurfaceId};
use crate::scene::port::{
    Clipboard, HostEvents, HostSurface, PresentTiming, PresentationFeedback, Presenter, Windows,
};

mod buffer;
mod png;

use buffer::resample_nearest;

pub use buffer::StoredBuffer;

/// A frame the presenter actually presented — the evidence a headless test asserts on.
#[derive(Clone, Debug, PartialEq)]
pub struct CapturedFrame {
    pub output: OutputId,
    pub surface: SurfaceId,
    pub width: i32,
    pub height: i32,
    /// Root-space top-left `(x, y)` this layer's content was composited at this cycle — the placement a
    /// popup/subsurface was routed to. Derived from the compose damage the scene handed `present`
    /// (`layer_damage` translates a layer's rect into root space by its offset), so a popup at a resolved
    /// positioner geometry, or a subsurface at `parent + set_position`, reports that offset here. `(0, 0)`
    /// when the layer contributed no damage this cycle (a clean base layer re-presented under a child).
    pub x: i32,
    pub y: i32,
    /// Tight `width*height*4` RGBA of the presented surface. When a `wp_viewport` transform is active this
    /// is the CROPPED+SCALED region actually presented (size `width`×`height` == the logical size); with no
    /// viewport it is the raw client buffer.
    pub rgba: Vec<u8>,
    /// The on-screen LOGICAL size `(logical_width, logical_height)` this surface presented at — after
    /// `wp_viewport` (dst/src) and `wl_surface.set_buffer_scale`. With no viewport and buffer scale 1 this
    /// equals `width`/`height`; under a buffer scale N it is `tex/N`; under a viewport dst it is the dst
    /// size. This is the "presented pixel size" a client's geometry is laid out in.
    pub logical_width: i32,
    pub logical_height: i32,
    pub serial: u64,
}

impl CapturedFrame {
    /// RGBA of the pixel at `(x, y)`, or `None` if out of bounds.
    pub fn pixel(&self, x: i32, y: i32) -> Option<[u8; 4]> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y * self.width + x) * 4) as usize;
        Some([
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ])
    }
}

/// Non-pixel adapter state a headless test needs to observe but that produces NO client-visible wire
/// event — so it cannot be asserted by watching the client, only by reading the server directly.
///
/// Two of the batch-7 protocols are like this: `zwp_idle_inhibit` (the compositor merely *tracks* an
/// inhibitor; nothing is sent back to the client) and `wp_content_type` (a per-surface hint the compositor
/// *reads at commit*; again no reply). To prove the adapter genuinely honours them — not just that the
/// global binds — the handlers record the observed state here, and a test reads it back through a shared
/// handle. It rides on [`PngPresenter`] because the presenter is ALREADY the one object threaded across the
/// serve-thread boundary that hands the test a shared `Arc` (`captures`); this is the same seam, for the
/// state a frame capture cannot carry. Keyed by the `wl_surface` PROTOCOL id, which is client-assigned and
/// therefore identical on both ends of the socket, so a test can name the surface it created.
#[derive(Clone, Debug, Default)]
pub struct Observations {
    /// `wl_surface` protocol ids that currently hold at least one live `zwp_idle_inhibitor_v1`. Inserted by
    /// `IdleInhibitHandler::inhibit`, removed by `uninhibit` — so a test sees a surface appear on create and
    /// disappear on the inhibitor's destroy.
    pub idle_inhibited: BTreeSet<u32>,
    /// `wl_surface` protocol id → the `wp_content_type_v1` hint (wire value: 0 none / 1 photo / 2 video /
    /// 3 game) last read from the surface's COMMITTED `ContentTypeSurfaceCachedState`. Re-read every commit,
    /// so a test sees the exact hint the client set (and its reversion to `none`).
    pub content_type: BTreeMap<u32, u32>,
    /// Whether a client-initiated `wl_data_device` drag-and-drop grab is currently active — set true by
    /// [`ClientDndGrabHandler::started`](super::state::HlState) when a client's `start_drag` is honoured
    /// (its implicit pointer grab is replaced by Smithay's DnD grab), cleared on the drop. There is no
    /// client-visible "the drag started" event on the SOURCE side beyond the grab itself, so a DnD test
    /// reads this to know WHEN the grab is live and it may inject the drag pointer motion that carries the
    /// offer to a target. See the `drag_and_drop` demo.
    pub dnd_active: bool,
    /// Set true once a client-initiated DnD grab reached its drop (the user released the last button) —
    /// written by [`ClientDndGrabHandler::dropped`](super::state::HlState). Distinct from `dnd_active`
    /// (which the same event clears): `dnd_dropped` latches so a test can assert the drop happened even
    /// after the grab is gone.
    pub dnd_dropped: bool,
    /// Whether the drop that ended the DnD was NEGOTIATED (the target accepted a mime and a non-empty
    /// action was chosen) — the `validated` flag Smithay passes to `dropped`. A validated drop is the one
    /// that delivers `wl_data_device.drop` to the target (an un-negotiated release cancels instead), so a
    /// test asserts this is `true` for a completed transfer.
    pub dnd_drop_validated: bool,
    /// Whether the session is currently LOCKED via `ext_session_lock_manager_v1` — set true once the
    /// compositor confirms a client's `lock` (and has hidden the normal surfaces), cleared on `unlock`.
    /// There is a client-visible `locked`/`finished` event, but this lets a test assert the SERVER-side
    /// transition (that normal surfaces were occluded) directly. See the `session_lock` demo.
    pub session_locked: bool,
    /// The CSS name (`pointer` / `text` / `grab` / …) of the last `wp_cursor_shape_device_v1.set_shape` the
    /// focused client requested, as decoded by Smithay and delivered through `SeatHandler::cursor_image`.
    /// `None` before any named shape is set, or once the client switches to a surface cursor / hides it.
    /// `wp_cursor_shape` carries no reply event, so this is the only way a test can assert the exact shape
    /// name Chrome/Ozone set reached the seat. See the `cursor_shape` demo.
    pub cursor_shape: Option<String>,
    /// `wl_surface` protocol ids that currently hold an ACTIVE `zwp_keyboard_shortcuts_inhibitor_v1`.
    /// Inserted (and the inhibitor activated) by `KeyboardShortcutsInhibitHandler::new_inhibitor`, removed by
    /// `inhibitor_destroyed`. The client also receives the `active` wire event, but this lets a test assert
    /// the SERVER-side grant + tracking directly. See the `keyboard_shortcuts_inhibit` demo.
    pub shortcuts_inhibited: BTreeSet<u32>,
    /// `wl_surface` protocol id → the `wp_tearing_control_v1` presentation hint (wire value: `0` vsync — do
    /// not tear / `1` async — tearing allowed) last read from the surface's COMMITTED
    /// `TearingControlCachedState`. Re-read every commit, so a test sees the exact hint the client committed
    /// (and its reversion to `vsync`). See the `tearing_control` demo.
    pub tearing_hint: BTreeMap<u32, u32>,
}

/// A headless [`Presenter`] that captures composed frames (and optionally writes PNGs).
pub struct PngPresenter {
    /// Client pixels deposited by the adapter at commit, keyed by surface.
    store: HashMap<SurfaceId, StoredBuffer>,
    /// Frames presented, shared so a test thread can read them while the compositor thread writes.
    captures: Arc<Mutex<Vec<CapturedFrame>>>,
    /// Non-pixel adapter observations (idle-inhibit / content-type), shared so a test thread reads them
    /// while the compositor thread (the protocol handlers) writes. See [`Observations`].
    observations: Arc<Mutex<Observations>>,
    /// If set, each presented frame is also written to `<dir>/frame-<serial>.png`.
    out_dir: Option<PathBuf>,
    serial: u64,
}

impl PngPresenter {
    /// A presenter that only captures frames in memory.
    pub fn new() -> PngPresenter {
        PngPresenter {
            store: HashMap::new(),
            captures: Arc::new(Mutex::new(Vec::new())),
            observations: Arc::new(Mutex::new(Observations::default())),
            out_dir: None,
            serial: 0,
        }
    }

    /// A presenter that also writes each presented frame to a PNG under `dir`.
    pub fn with_png_dir(dir: impl Into<PathBuf>) -> PngPresenter {
        PngPresenter {
            out_dir: Some(dir.into()),
            ..PngPresenter::new()
        }
    }

    /// A clonable handle onto the captured-frame log — grab this BEFORE moving the presenter into the
    /// compositor thread, then read presented frames back from the test thread.
    pub fn captures(&self) -> Arc<Mutex<Vec<CapturedFrame>>> {
        Arc::clone(&self.captures)
    }

    /// A clonable handle onto the [`Observations`] — grab this BEFORE moving the presenter into the
    /// compositor thread (exactly like [`Self::captures`]), then read the adapter's idle-inhibit /
    /// content-type tracking back from the test thread. The adapter clones the same handle into `HlState`
    /// at construction so its protocol handlers write where the test reads.
    pub fn observations(&self) -> Arc<Mutex<Observations>> {
        Arc::clone(&self.observations)
    }

    /// Deposit a surface's just-committed client pixels. The adapter calls this from its commit handler
    /// immediately before driving the scene, so the following `present` can capture real pixels.
    pub fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer) {
        self.store.insert(surface, buffer);
    }

    /// Forget a surface's pixels (on detach / destroy).
    pub fn forget(&mut self, surface: SurfaceId) {
        self.store.remove(&surface);
    }
}

impl Default for PngPresenter {
    fn default() -> PngPresenter {
        PngPresenter::new()
    }
}

impl Presenter for PngPresenter {
    fn present(
        &mut self,
        output: OutputId,
        image: &PresentableImage,
        damage: &[Rect],
        timing: PresentTiming,
    ) -> PresentationFeedback {
        let Some(buf) = self.store.get(&image.surface) else {
            // No pixels were deposited for this surface — nothing to capture. Report offscreen so the
            // scene does not advance pacing as if a real frame shipped.
            return PresentationFeedback::offscreen();
        };
        self.serial += 1;
        let serial = self.serial;
        // Where this layer landed in root space: the top-left of its compose damage (which
        // `service/compose::layer_damage` produced by translating the layer rect by its root offset). A
        // clean layer carries no damage, so it reports `(0, 0)` — the base root's own origin.
        let (x, y) = damage
            .iter()
            .filter(|r| !r.is_empty())
            .map(|r| (r.x, r.y))
            .next()
            .unwrap_or((0, 0));
        // The presented logical size — the destination a `wp_viewport` scales to, or `tex/buffer_scale`,
        // as resolved by the scene into `image.width`/`image.height`.
        let (logical_width, logical_height) = (image.width.max(1), image.height.max(1));
        // Rasterize the pixels a real backend would present, following the Wayland buffer→surface chain:
        //  1. `wl_surface.set_buffer_transform` rotates/flips the buffer into SURFACE space (dimensions
        //     swapped for 90°/270°);
        //  2. `wp_viewport` src crop (stated in surface coordinates) + dst scale sample that surface-space
        //     image into the destination logical size.
        // The two COMPOSE: a rotated+cropped buffer un-rotates first, then the crop applies in surface
        // space. Each step alone is the degenerate case of the general path.
        let has_transform = image.transform != BufferTransform::Normal;
        // Defend the rasterizer against hostile geometry (task #205). The dimensions we would actually
        // allocate into differ per viewport/transform combination; compute them, then REFUSE (report
        // offscreen so pacing does not advance) any frame whose source buffer is degenerate/inconsistent
        // with its rgba, or whose destination axis is non-positive or beyond `MAX_PRESENT_DIM`. Without
        // this a `wp_viewport` dst of a few billion, or a buffer size whose `w*h*4` overflows `i32`, would
        // panic (debug) or drive a multi-GiB allocation — the exact attacker-size→unchecked-mul pattern
        // the driver sweeps found. Passing this gate guarantees `transform_buffer`/`resample_nearest`
        // receive a consistent buffer and bounded, in-range dimensions, so their index math cannot
        // overflow or slice out of bounds.
        let (out_w, out_h) = match (image.present_crop.is_some(), has_transform) {
            (true, _) => (logical_width, logical_height),
            (false, true) => image.transform.surface_size(buf.width, buf.height),
            (false, false) => (buf.width, buf.height),
        };
        let buffer_consistent = buf.tight_bytes().is_some_and(|need| buf.rgba.len() >= need);
        if !buffer_consistent
            || out_w <= 0
            || out_h <= 0
            || out_w > MAX_PRESENT_DIM
            || out_h > MAX_PRESENT_DIM
        {
            return PresentationFeedback::offscreen();
        }
        let (width, height, rgba) = match (image.present_crop, has_transform) {
            // Transform + viewport composed: un-rotate the buffer into surface space, then sample the
            // surface-space crop (`present_crop` is already in surface-space pixels) into the logical size.
            (Some(src), true) => {
                let (tw, th) = image.transform.surface_size(buf.width, buf.height);
                let surface_buf = StoredBuffer {
                    width: tw,
                    height: th,
                    rgba: buf.transformed(image.transform),
                    bgra: false,
                    damage: None,
                };
                (
                    logical_width,
                    logical_height,
                    resample_nearest(&surface_buf, src, logical_width, logical_height),
                )
            }
            // Viewport only (no transform): sample the source region directly from the buffer.
            (Some(src), false) => (
                logical_width,
                logical_height,
                resample_nearest(buf, src, logical_width, logical_height),
            ),
            // Transform only: rotate/flip the whole buffer into surface space.
            (None, true) => {
                let (tw, th) = image.transform.surface_size(buf.width, buf.height);
                (tw, th, buf.transformed(image.transform))
            }
            // Neither: the raw client buffer verbatim.
            (None, false) => (buf.width, buf.height, buf.rgba.clone()),
        };
        let frame = CapturedFrame {
            output,
            surface: image.surface,
            width,
            height,
            x,
            y,
            rgba,
            logical_width,
            logical_height,
            serial,
        };
        if let Some(dir) = &self.out_dir {
            match std::fs::create_dir_all(dir) {
                Ok(()) => {
                    let path = dir.join(format!("frame-{serial}.png"));
                    if let Err(error) = write_png(&path, frame.width, frame.height, &frame.rgba) {
                        hl_log::hl_error!(
                            tag::PRESENT,
                            "captured frame write failed path={} error={error}",
                            path.display()
                        );
                    }
                }
                Err(error) => {
                    hl_log::hl_error!(
                        tag::PRESENT,
                        "captured frame directory creation failed path={} error={error}",
                        dir.display()
                    );
                }
            }
        }
        hl_add!(tag::PRESENT, "captured_frames", 1);
        hl_add!(tag::PRESENT, "captured_bytes", buf.rgba.len() as u64);
        self.captures.lock().unwrap().push(frame);
        PresentationFeedback::delivered(
            serial,
            Some(PresentTiming {
                present_ns: timing.present_ns,
                refresh_ns: timing.refresh_ns,
                vsync: false,
            }),
        )
    }
}

impl HostEvents for PngPresenter {}
impl Clipboard for PngPresenter {}
impl Windows for PngPresenter {}

/// Hard cap on any single presented axis. A real display dimension is far below this; a hostile
/// `wp_viewport` destination or buffer size beyond it is refused by [`Presenter::present`] rather than
/// rasterized, so no attacker-chosen geometry can drive an unbounded (or `i32`-overflowing) allocation.
/// Mirrors the bound-before-alloc guards the driver sweeps installed. (`16384²·4` ≈ 1 GiB is the hard
/// ceiling; every real frame is orders of magnitude smaller.)
const MAX_PRESENT_DIM: i32 = 1 << 14;

pub use png::{encode_png, write_png};

/// Pixel-delivery extension required by the Smithay protocol adapter.
///
/// The neutral [`Presenter`] remains independent of Wayland buffers. A platform presenter implements
/// this small adapter port when it can receive the committed CPU pixels that Smithay extracted.
pub trait SurfacePresenter: HostSurface {
    fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer);
    fn forget(&mut self, surface: SurfaceId);

    fn observations(&self) -> Option<Arc<Mutex<Observations>>> {
        None
    }
}

impl SurfacePresenter for PngPresenter {
    fn deposit(&mut self, surface: SurfaceId, mut buffer: StoredBuffer) {
        if buffer.bgra {
            for pixel in buffer.rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            buffer.bgra = false;
        }
        PngPresenter::deposit(self, surface, buffer);
    }

    fn forget(&mut self, surface: SurfaceId) {
        PngPresenter::forget(self, surface);
    }

    fn observations(&self) -> Option<Arc<Mutex<Observations>>> {
        Some(PngPresenter::observations(self))
    }
}

#[cfg(all(target_os = "macos", feature = "macos-surface"))]
impl SurfacePresenter for crate::surface::macos::MacPresenter {
    fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer) {
        let mut bgra = buffer.rgba;
        if !buffer.bgra {
            for pixel in bgra.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
        }
        self.attach_bgra_damage(
            surface,
            bgra,
            buffer.width.max(1) as u32,
            buffer.height.max(1) as u32,
            buffer.damage,
        );
    }

    fn forget(&mut self, surface: SurfaceId) {
        crate::surface::macos::MacPresenter::forget(self, surface);
    }
}

/// Type-erased presenter owned by the Smithay adapter. The composition root remains free to supply
/// any platform implementation of [`SurfacePresenter`].
pub struct AdapterPresenter {
    inner: Box<dyn SurfacePresenter>,
    observations: Arc<Mutex<Observations>>,
}

impl AdapterPresenter {
    pub fn new(presenter: impl SurfacePresenter + 'static) -> AdapterPresenter {
        let observations = presenter
            .observations()
            .unwrap_or_else(|| Arc::new(Mutex::new(Observations::default())));
        AdapterPresenter {
            inner: Box::new(presenter),
            observations,
        }
    }

    pub fn observations(&self) -> Arc<Mutex<Observations>> {
        Arc::clone(&self.observations)
    }

    pub fn deposit(&mut self, surface: SurfaceId, buffer: StoredBuffer) {
        self.inner.deposit(surface, buffer);
    }

    pub fn forget(&mut self, surface: SurfaceId) {
        self.inner.forget(surface);
    }
}

impl<T> From<T> for AdapterPresenter
where
    T: SurfacePresenter + 'static,
{
    fn from(presenter: T) -> Self {
        AdapterPresenter::new(presenter)
    }
}

impl Presenter for AdapterPresenter {
    fn present(
        &mut self,
        output: OutputId,
        image: &PresentableImage,
        damage: &[Rect],
        timing: PresentTiming,
    ) -> PresentationFeedback {
        self.inner.present(output, image, damage, timing)
    }
}

impl HostEvents for AdapterPresenter {
    fn poll_events(&mut self) {
        self.inner.poll_events();
    }

    fn take_events(&mut self) -> Vec<crate::scene::port::PresenterEvent> {
        self.inner.take_events()
    }
}

impl Clipboard for AdapterPresenter {
    fn set_clipboard_text(&mut self, text: &str) {
        self.inner.set_clipboard_text(text);
    }

    fn take_clipboard_text(&mut self) -> Option<String> {
        self.inner.take_clipboard_text()
    }
}

impl Windows for AdapterPresenter {
    fn reconcile_window(&mut self, window: &crate::scene::model::WindowState) {
        self.inner.reconcile_window(window);
    }

    fn destroy_window(&mut self, surface: SurfaceId) {
        self.inner.destroy_window(surface);
    }

    fn begin_interaction(
        &mut self,
        surface: SurfaceId,
        interaction: crate::scene::model::WindowInteraction,
    ) {
        self.inner.begin_interaction(surface, interaction);
    }
}

#[cfg(test)]
#[path = "present/tests.rs"]
mod tests;
