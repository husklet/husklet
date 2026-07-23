//! [`MacPresenter`]: the concrete macOS [`Presenter`] the neutral scene hands finished frames to.
//!
//! It composites the currently-attached content for each surface (a `wl_shm` BGRA buffer or a zero-copy
//! host `IOSurface`) through the Metal composite pipeline into a persistent offscreen target, then — in
//! windowed mode — blits that into a `CAMetalLayer` drawable. Headless (offscreen) mode keeps only the
//! backing target, which [`MacPresenter::last_rgba`] reads back to prove the present on a real GPU
//! without a visible window / GUI session.
//!
//! The neutral [`PresentableImage`] carries geometry only, so pixels are supplied out-of-band via
//! [`MacPresenter::attach_bgra`] / [`MacPresenter::attach_iosurface`] before `present()` runs — the same
//! commit-then-compose split a Wayland compositor uses.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use hl_log::{hl_debug, hl_span, tag};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventPhase, NSEventType,
    NSPasteboard, NSPasteboardTypeString,
};
use objc2_foundation::{MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSRunLoop, NSString};
use objc2_metal::{MTLRenderPipelineState, MTLTexture};

use crate::scene::model::{
    BufferTransform, OutputId, PresentableImage, Rect, SurfaceId, Visibility, WindowInteraction,
    WindowKind, WindowState,
};
use crate::scene::port::{
    PresentOutcome, PresentTiming, PresentationFeedback, Presenter, PresenterEvent,
};

use super::capture::Capture;
use super::iosurface::IOSurface;
use super::metal::{BgraFrame, MetalCtx};
use super::window::{DisplayConfig, MetalWindow, NativeApplication, ResizeDrag};

mod event;
mod frame;
mod gesture;
mod key;
mod tablet;
mod window;

use gesture::Gestures;
use key::{KeyCode, Modifiers};
use tablet::Tablet;

/// The pixel source a surface last attached — resolved to an `MTLTexture` at present time.
enum Content {
    /// A `wl_shm` BGRA buffer (tight `w*4` rows), uploaded to a texture each present.
    Bgra {
        bgra: Vec<u8>,
        w: u32,
        h: u32,
        damage: Option<Vec<Rect>>,
    },
    /// A host `IOSurface` global id, wrapped zero-copy each present.
    IoSurfaceId(u32),
}

type TextureSlot = (u32, u32, Retained<ProtocolObject<dyn MTLTexture>>);

/// Per-surface presenter state: the attached content, the persistent composite target, and (windowed
/// mode) the native window.
struct SurfState {
    content: Option<Content>,
    /// The last composited frame `(w, h, texture)` in device pixels — readable back for verification.
    composite: Option<TextureSlot>,
    /// Three reusable upload textures for same-sized wl_shm frames. Rotating with the CAMetalLayer's
    /// drawable depth avoids overwriting shared storage while an earlier command buffer still reads it.
    uploads: Vec<TextureSlot>,
    upload_cursor: usize,
    window: Option<MetalWindow>,
    /// Root-toplevel logical origin used to translate AppKit popup-local pointer coordinates.
    input_origin: (f64, f64),
    desired: Option<WindowState>,
    reported_native_size: Option<(u32, u32)>,
    native_resize_pending: Option<(u32, u32, bool, bool, bool)>,
    native_resize_changed_at: Option<Instant>,
    /// Last time the newest native size was sent to the Wayland client. AppKit can report hundreds of
    /// intermediate frame sizes per second; XDG configures are state snapshots, so queueing every one
    /// only makes the toolkit render obsolete sizes and causes the visible resize to lag behind the hand.
    native_resize_sent_at: Option<Instant>,
    native_resize_last_sent: Option<(u32, u32, bool, bool, bool)>,
    observed_native_fullscreen: Option<bool>,
}

impl SurfState {
    fn new() -> SurfState {
        SurfState {
            content: None,
            composite: None,
            uploads: Vec::new(),
            upload_cursor: 0,
            window: None,
            input_origin: (0.0, 0.0),
            desired: None,
            reported_native_size: None,
            native_resize_pending: None,
            native_resize_changed_at: None,
            native_resize_sent_at: None,
            native_resize_last_sent: None,
            observed_native_fullscreen: None,
        }
    }
}

/// A [`Presenter`] that draws each composed frame to a real macOS window (Cocoa `NSWindow` +
/// `CAMetalLayer` + Metal), or headless into an offscreen `MTLTexture` it can read back.
pub struct MacPresenter {
    ctx: MetalCtx,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    surfaces: HashMap<SurfaceId, SurfState>,
    /// `Some` ⇒ windowed mode (open real `NSWindow`s); `None` ⇒ headless offscreen mode.
    mtm: Option<MainThreadMarker>,
    /// Monotonic per-frame pacing serial for delivered frames.
    serial: u64,
    /// Total frames presented.
    pub frames: u32,
    capture: Option<Capture>,
    events: Vec<PresenterEvent>,
    drag_event: Option<Retained<NSEvent>>,
    native_resize: Option<(SurfaceId, ResizeDrag)>,
    key_modifiers: Modifiers,
    gestures: Gestures,
    tablet: Tablet,
    pasteboard_change: isize,
}

impl MacPresenter {
    pub fn primary_output_spec_on_main_thread() -> Option<String> {
        DisplayConfig::new(MainThreadMarker::new()?).primary_spec()
    }
    pub fn primary_refresh_millihz_on_main_thread() -> Option<i64> {
        DisplayConfig::new(MainThreadMarker::new()?).primary_refresh_millihz()
    }
    /// Construct the visible presenter when called from the process main thread.
    pub fn new_windowed_on_main_thread() -> Option<MacPresenter> {
        Self::new_windowed(MainThreadMarker::new()?)
    }

    /// Headless: composite into offscreen `MTLTexture`s only (no windows). Provable on any Metal GPU with
    /// no GUI login session. `None` if Metal is unavailable / the composite pipeline fails to compile.
    pub fn new_offscreen() -> Option<MacPresenter> {
        let ctx = MetalCtx::new()?;
        let pipeline = ctx.make_composite_pipeline()?;
        Some(MacPresenter {
            ctx,
            pipeline,
            surfaces: HashMap::new(),
            mtm: None,
            serial: 0,
            frames: 0,
            capture: None,
            events: Vec::new(),
            drag_event: None,
            native_resize: None,
            key_modifiers: Modifiers::default(),
            gestures: Gestures::default(),
            tablet: Tablet::default(),
            pasteboard_change: unsafe { NSPasteboard::generalPasteboard().changeCount() },
        })
    }

    /// Windowed: open a real `NSWindow` + `CAMetalLayer` per surface. Must be constructed on the AppKit
    /// main thread; a visible window additionally needs a GUI login session. `None` if Metal is
    /// unavailable / the pipeline fails.
    pub fn new_windowed(mtm: MainThreadMarker) -> Option<MacPresenter> {
        NativeApplication::ensure(mtm);
        let ctx = MetalCtx::new()?;
        let pipeline = ctx.make_composite_pipeline()?;
        let mut presenter = MacPresenter {
            ctx,
            pipeline,
            surfaces: HashMap::new(),
            mtm: Some(mtm),
            serial: 0,
            frames: 0,
            capture: None,
            events: Vec::new(),
            drag_event: None,
            native_resize: None,
            key_modifiers: Modifiers::default(),
            gestures: Gestures::default(),
            tablet: Tablet::default(),
            pasteboard_change: unsafe { NSPasteboard::generalPasteboard().changeCount() },
        };
        if let Some(directory) = std::env::var_os("HL_SURFACE_CAPTURE_DIR") {
            match Capture::new(PathBuf::from(directory)) {
                Ok(capture) => presenter.capture = Some(capture),
                Err(error) => eprintln!("[macos-surface] diagnostic capture disabled: {error}"),
            }
        }
        Some(presenter)
    }

    /// Persist the latest composited RGBA frame for every surface as a PPM image.
    pub fn capture_to(mut self, directory: impl Into<PathBuf>) -> io::Result<Self> {
        self.capture = Some(Capture::new(directory)?);
        Ok(self)
    }

    /// The Metal device (GPU/adapter) name the present runs on, e.g. "Apple M-series".
    pub fn device_name(&self) -> String {
        self.ctx.device_name()
    }

    /// Attach a `wl_shm` BGRA buffer (tight `w*4` rows, little-endian ARGB8888 memory order B,G,R,A) as
    /// surface `sid`'s content for the next present.
    pub fn attach_bgra(&mut self, sid: SurfaceId, bgra: Vec<u8>, w: u32, h: u32) {
        self.attach_bgra_damage(sid, bgra, w, h, None);
    }

    /// Attach BGRA content with optional buffer-coordinate damage for incremental backend uploads.
    pub fn attach_bgra_damage(
        &mut self,
        sid: SurfaceId,
        bgra: Vec<u8>,
        w: u32,
        h: u32,
        damage: Option<Vec<Rect>>,
    ) {
        self.surfaces
            .entry(sid)
            .or_insert_with(SurfState::new)
            .content = Some(Content::Bgra { bgra, w, h, damage });
    }

    /// Attach a host `IOSurface` global id as surface `sid`'s content — wrapped zero-copy at present.
    pub fn attach_iosurface(&mut self, sid: SurfaceId, id: u32) {
        self.surfaces
            .entry(sid)
            .or_insert_with(SurfState::new)
            .content = Some(Content::IoSurfaceId(id));
    }

    fn sync_key_modifiers(&mut self, flags: NSEventModifierFlags) {
        self.events.extend(self.key_modifiers.update(flags));
    }

    /// Retire attached pixel resources while preserving the native window lifecycle. A null-buffer
    /// commit unmaps content; it does not destroy the xdg window role.
    pub fn forget(&mut self, sid: SurfaceId) {
        if let Some(state) = self.surfaces.get_mut(&sid) {
            state.content = None;
            state.composite = None;
            state.uploads.clear();
        }
    }

    fn destroy(&mut self, sid: SurfaceId) {
        if let Some(state) = self.surfaces.remove(&sid) {
            if let Some(window) = state.window {
                window.close();
            }
        }
    }

    /// Read surface `sid`'s last composited frame back as `(w, h, RGBA)` — the GUI-session-free proof of
    /// what the presenter put on (or would put on) screen.
    pub fn last_rgba(&self, sid: SurfaceId) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h, tex) = self.surfaces.get(&sid)?.composite.as_ref()?;
        let bgra = self.ctx.readback_bgra(tex, *w, *h);
        Some((*w, *h, BgraFrame::new(&bgra).rgba()))
    }

    /// Compose surface `sid`'s attached content into its persistent target. Returns the composite
    /// `(w, h)` on success, or an error string on device failure (unresolvable IOSurface, missing
    /// content). Does not touch the window.
    fn compose(&mut self, image: &PresentableImage) -> Result<(u32, u32), String> {
        let sid = image.surface;
        let format = image.format;
        let st = self.surfaces.get_mut(&sid).ok_or("no state for surface")?;
        let content = st.content.as_ref().ok_or("no content attached")?;

        // Resolve the source texture (and its device-pixel size) from the attached content.
        let (src, w, h) = match content {
            Content::Bgra { bgra, w, h, damage } => {
                let expected = usize::try_from(*w)
                    .ok()
                    .and_then(|width| {
                        usize::try_from(*h)
                            .ok()
                            .and_then(|height| width.checked_mul(height))
                    })
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or("BGRA dimensions overflow address space")?;
                if *w == 0 || *h == 0 {
                    return Err("BGRA content has a zero dimension".to_string());
                }
                if bgra.len() != expected {
                    return Err(format!(
                        "BGRA content length {} does not match {}x{}x4 ({expected})",
                        bgra.len(),
                        w,
                        h
                    ));
                }
                if st
                    .uploads
                    .first()
                    .is_some_and(|(uw, uh, _)| (*uw, *uh) != (*w, *h))
                {
                    st.uploads.clear();
                    st.upload_cursor = 0;
                }
                for (_, _, texture) in &st.uploads {
                    match damage {
                        Some(damage) => self.ctx.update_bgra_regions(texture, bgra, *w, *h, damage),
                        None => self.ctx.update_bgra(texture, bgra, *w, *h),
                    }
                }
                let tex = if st.uploads.len() < 3 {
                    let tex = self.ctx.upload_bgra(bgra, *w, *h);
                    st.uploads.push((*w, *h, tex.clone()));
                    tex
                } else {
                    let index = st.upload_cursor % st.uploads.len();
                    st.uploads[index].2.clone()
                };
                st.upload_cursor = (st.upload_cursor + 1) % 3;
                (tex, *w, *h)
            }
            Content::IoSurfaceId(id) => {
                let surface =
                    IOSurface::lookup(*id).ok_or_else(|| format!("IOSurface id {id} not found"))?;
                let (sw, sh, _) = surface.dimensions();
                let tex = self
                    .ctx
                    .texture_from_iosurface(surface.as_ptr(), sw as u32, sh as u32);
                (tex, sw as u32, sh as u32)
            }
        };

        // Start with wp_viewport's buffer-pixel source rectangle, then map xdg window geometry from
        // logical surface coordinates into that rectangle. This composition is essential on Retina GTK:
        // its dst-only viewport describes the complete 2x buffer while xdg geometry removes client-side
        // shadow margins. Ignoring either contract leaves a non-integral drawable that AppKit resamples.
        let crop = st
            .desired
            .as_ref()
            .and_then(|window| window.geometry.zip(window.logical_size))
            .filter(|_| image.transform == BufferTransform::Normal)
            .and_then(|(geometry, logical)| {
                if geometry.w <= 0 || geometry.h <= 0 || logical.0 <= 0 || logical.1 <= 0 {
                    return None;
                }
                let (base_x, base_y, base_w, base_h) =
                    image.present_crop.unwrap_or((0.0, 0.0, w as f64, h as f64));
                if base_w <= 0.0 || base_h <= 0.0 {
                    return None;
                }
                let x0 = (base_x + geometry.x as f64 * base_w / logical.0 as f64)
                    .round()
                    .clamp(0.0, w as f64) as u32;
                let y0 = (base_y + geometry.y as f64 * base_h / logical.1 as f64)
                    .round()
                    .clamp(0.0, h as f64) as u32;
                let x1 = (base_x + (geometry.x + geometry.w) as f64 * base_w / logical.0 as f64)
                    .round()
                    .clamp(0.0, w as f64) as u32;
                let y1 = (base_y + (geometry.y + geometry.h) as f64 * base_h / logical.1 as f64)
                    .round()
                    .clamp(0.0, h as f64) as u32;
                (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
            });
        let (out_w, out_h, uv) = match crop {
            Some((x, y, cw, ch)) => (
                cw,
                ch,
                [
                    x as f32 / w as f32,
                    y as f32 / h as f32,
                    (x + cw) as f32 / w as f32,
                    (y + ch) as f32 / h as f32,
                ],
            ),
            None => (w, h, [0.0, 0.0, 1.0, 1.0]),
        };

        // Reuse a persistent composite target sized to the visible source region.
        let need_new = match &self.surfaces.get(&sid).unwrap().composite {
            Some((cw, ch, _)) => *cw != out_w || *ch != out_h,
            None => true,
        };
        if need_new {
            let tex = self.ctx.new_bgra_texture(out_w, out_h);
            self.surfaces.get_mut(&sid).unwrap().composite = Some((out_w, out_h, tex));
        }
        let dst = self
            .surfaces
            .get(&sid)
            .unwrap()
            .composite
            .as_ref()
            .unwrap()
            .2
            .clone();
        self.ctx.compose_into(
            &self.pipeline,
            &src,
            &dst,
            uv,
            format.is_opaque(),
            // A native present enqueues its drawable blit on the same Metal command queue immediately
            // after this render, so queue ordering is the required synchronization and a CPU fence only
            // adds latency. Offscreen verification and diagnostic capture read pixels synchronously and
            // therefore still require completion here.
            self.mtm.is_none() || self.capture.is_some(),
        );
        Ok((out_w, out_h))
    }
}

impl Presenter for MacPresenter {
    fn set_clipboard_text(&mut self, text: &str) {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
        unsafe {
            pasteboard.clearContents();
            pasteboard.setString_forType(&NSString::from_str(text), NSPasteboardTypeString);
            self.pasteboard_change = pasteboard.changeCount();
        }
    }

    fn take_clipboard_text(&mut self) -> Option<String> {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
        let change = unsafe { pasteboard.changeCount() };
        if change == self.pasteboard_change {
            return None;
        }
        self.pasteboard_change = change;
        unsafe { pasteboard.stringForType(NSPasteboardTypeString) }.map(|text| text.to_string())
    }

    fn reconcile_window(&mut self, desired: &WindowState) {
        self.reconcile_native_window(desired);
    }

    fn destroy_window(&mut self, surface: SurfaceId) {
        self.destroy(surface);
    }

    fn begin_interaction(&mut self, surface: SurfaceId, interaction: WindowInteraction) {
        if interaction == WindowInteraction::Move {
            if let (Some(window), Some(event)) = (
                self.surfaces
                    .get(&surface)
                    .and_then(|state| state.window.as_ref()),
                self.drag_event.take(),
            ) {
                window.drag(&event);
            }
        }
    }

    fn poll_events(&mut self) {
        self.poll_native_events();
    }

    fn take_events(&mut self) -> Vec<PresenterEvent> {
        std::mem::take(&mut self.events)
    }

    fn present(
        &mut self,
        output: OutputId,
        image: &PresentableImage,
        damage: &[Rect],
        timing: PresentTiming,
    ) -> PresentationFeedback {
        self.present_native(output, image, damage, timing)
    }
}
