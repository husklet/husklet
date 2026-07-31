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

use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_log::{hl_debug, hl_span, tag, Level};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventPhase, NSEventType,
    NSPasteboard, NSPasteboardTypeString,
};
use objc2_foundation::{MainThreadMarker, NSDate, NSDefaultRunLoopMode, NSRunLoop, NSString};
use objc2_metal::{MTLRenderPipelineState, MTLTexture};

use crate::scene::model::{
    OutputId, PresentableImage, Rect, SurfaceId, Visibility, WindowInteraction, WindowKind,
    WindowState,
};
use crate::scene::port::{
    Clipboard, CompletionOutcome, HostEvents, PresentOutcome, PresentTiming, PresentationFeedback,
    PresentationId, Presenter, PresenterEvent, ScrollSource, Wake, Windows,
};

use super::capture::Capture;
use super::cursor::HostCursorState;
use super::metal::{BgraFrame, MetalCtx};
use super::transform::Sampling;
use super::window::{DisplayConfig, MetalWindow, NativeApplication, ResizeDrag};
use hl_iosurface::Surface as IOSurface;

mod compose;
mod damage;
mod event;
mod frame;
mod gesture;
mod host;
mod key;
mod latency;
pub(super) mod submission;
mod tablet;
mod window;

use damage::Damage;
use gesture::Gestures;
use key::{current_xkb_layout, KeyCode, Modifiers};
use latency::HostInput;
use submission::{NativePresent, PresentAttempt};
use tablet::Tablet;

/// The pixel source a surface last attached — resolved to an `MTLTexture` at present time.
enum Content {
    /// A `wl_shm` BGRA buffer (tight `w*4` rows), uploaded to a texture each present.
    Bgra { bgra: Vec<u8>, w: u32, h: u32 },
    /// An IOSurface retained by the presenter.
    IoSurface(IOSurface),
}

type TextureSlot = (u32, u32, Retained<ProtocolObject<dyn MTLTexture>>);

const MAX_PRESENT_DIM: u32 = 1 << 14;
const MAX_PRESENT_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PresentationKey {
    transform: crate::scene::model::BufferTransform,
    crop: Option<[u64; 4]>,
    width: u32,
    height: u32,
    format: crate::scene::model::Format,
}

impl PresentationKey {
    fn new(
        image: &PresentableImage,
        crop: Option<(f64, f64, f64, f64)>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            transform: image.transform,
            crop: crop.map(|(x, y, width, height)| {
                [x.to_bits(), y.to_bits(), width.to_bits(), height.to_bits()]
            }),
            width,
            height,
            format: image.format,
        }
    }
}

fn destination_pixels(destination: (i32, i32), backing_scale: f64) -> Result<(u32, u32), String> {
    if destination.0 <= 0 || destination.1 <= 0 {
        return Err("present destination has a zero or negative dimension".to_string());
    }
    if !backing_scale.is_finite() || backing_scale < 1.0 {
        return Err("native backing scale is invalid".to_string());
    }
    let width = f64::from(destination.0) * backing_scale;
    let height = f64::from(destination.1) * backing_scale;
    if !width.is_finite()
        || !height.is_finite()
        || width > f64::from(MAX_PRESENT_DIM)
        || height > f64::from(MAX_PRESENT_DIM)
    {
        return Err(format!(
            "present destination {}x{} at scale {backing_scale} exceeds {MAX_PRESENT_DIM}",
            destination.0, destination.1
        ));
    }
    let width = width.round().max(1.0) as u32;
    let height = height.round().max(1.0) as u32;
    let bytes = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "present destination byte size overflow".to_string())?;
    if bytes > MAX_PRESENT_BYTES {
        return Err(format!(
            "present destination {width}x{height} requires {bytes} bytes, limit is {MAX_PRESENT_BYTES}"
        ));
    }
    Ok((width, height))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PixelStats {
    min: u8,
    max: u8,
    nonzero: usize,
}

impl PixelStats {
    fn from_bytes(bytes: &[u8]) -> Self {
        let mut stats = Self {
            min: u8::MAX,
            max: u8::MIN,
            nonzero: 0,
        };
        for &byte in bytes {
            stats.min = stats.min.min(byte);
            stats.max = stats.max.max(byte);
            stats.nonzero += usize::from(byte != 0);
        }
        if bytes.is_empty() {
            stats.min = 0;
        }
        stats
    }
}

/// Per-surface presenter state: the attached content, the persistent composite target, and (windowed
/// mode) the native window.
struct SurfState {
    content: Option<Content>,
    /// The last composited frame `(w, h, texture)` in device pixels — readable back for verification.
    composite: Option<TextureSlot>,
    /// Authoritative native-role composition rebuilt from all current layers each submitted frame.
    frame: Option<TextureSlot>,
    native_presents: VecDeque<NativePresent>,
    native_submission: Option<PresentationKey>,
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
    observed_backing_scale: Option<u64>,
    /// Whether this popup has already described its placement (see `describe_popup_placement`). Latched
    /// so a menu that repaints at refresh rate states its geometry once, not sixty times a second.
    described_popup: bool,
}

impl SurfState {
    fn new() -> SurfState {
        SurfState {
            content: None,
            composite: None,
            frame: None,
            native_presents: VecDeque::new(),
            native_submission: None,
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
            observed_backing_scale: None,
            described_popup: false,
        }
    }

    fn observe_backing_scale(&mut self, scale: f64) -> bool {
        let scale = scale.max(1.0).to_bits();
        let changed = self
            .observed_backing_scale
            .replace(scale)
            .is_some_and(|previous| previous != scale);
        if changed {
            self.native_submission = None;
        }
        changed
    }
}

/// A [`Presenter`] that draws each composed frame to a real macOS window (Cocoa `NSWindow` +
/// `CAMetalLayer` + Metal), or headless into an offscreen `MTLTexture` it can read back.
pub struct MacPresenter {
    _application: Option<NativeApplication>,
    ctx: MetalCtx,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    surfaces: HashMap<SurfaceId, SurfState>,
    retired_native_presents: VecDeque<(SurfaceId, NativePresent)>,
    /// `Some` ⇒ windowed mode (open real `NSWindow`s); `None` ⇒ headless offscreen mode.
    mtm: Option<MainThreadMarker>,
    /// Total frames presented.
    pub frames: u32,
    capture: Option<Capture>,
    requested_capture: Option<Capture>,
    events: Vec<PresenterEvent>,
    drag_event: Option<Retained<NSEvent>>,
    native_resize: Option<(SurfaceId, ResizeDrag)>,
    key_modifiers: Modifiers,
    gestures: Gestures,
    tablet: Tablet,
    pasteboard_change: isize,
    presentation_tx: Sender<PresenterEvent>,
    presentation_rx: Receiver<PresenterEvent>,
    next_submission: u64,
    present_surfaces: HashMap<PresentationId, SurfaceId>,
    wake: Option<Arc<dyn Wake>>,
    /// The host pointer cursor the guest last asked for. Only applied in windowed mode: headless there is
    /// no AppKit cursor to own.
    cursor: HostCursorState,
}

impl MacPresenter {
    fn backing_scale_for(&self, sid: SurfaceId) -> f64 {
        let Some(marker) = self.mtm else {
            return 1.0;
        };
        let state = self.surfaces.get(&sid);
        if let Some(scale) = state
            .and_then(|state| state.window.as_ref())
            .map(MetalWindow::backing_scale)
        {
            return scale;
        }
        let parent = state
            .and_then(|state| state.desired.as_ref())
            .and_then(|window| match window.kind {
                WindowKind::Toplevel { parent } => parent,
                WindowKind::Popup { parent, .. } => Some(parent),
            });
        if let Some(scale) = parent
            .and_then(|parent| self.surfaces.get(&parent))
            .and_then(|state| state.window.as_ref())
            .map(MetalWindow::backing_scale)
        {
            return scale;
        }
        DisplayConfig::new(marker).primary_scale()
    }

    /// XKB layout corresponding to the user's currently selected macOS keyboard input source.
    pub fn xkb_layout_on_main_thread() -> Option<&'static str> {
        MainThreadMarker::new()?;
        current_xkb_layout()
    }

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
        let (presentation_tx, presentation_rx) = mpsc::channel();
        Some(MacPresenter {
            _application: None,
            ctx,
            pipeline,
            surfaces: HashMap::new(),
            retired_native_presents: VecDeque::new(),
            mtm: None,
            frames: 0,
            capture: None,
            requested_capture: None,
            events: Vec::new(),
            drag_event: None,
            native_resize: None,
            key_modifiers: Modifiers::default(),
            gestures: Gestures::default(),
            tablet: Tablet::default(),
            pasteboard_change: unsafe { NSPasteboard::generalPasteboard().changeCount() },
            presentation_tx,
            presentation_rx,
            next_submission: 1,
            present_surfaces: HashMap::new(),
            wake: None,
            cursor: HostCursorState::default(),
        })
    }

    /// Windowed: open a real `NSWindow` + `CAMetalLayer` per surface. Must be constructed on the AppKit
    /// main thread; a visible window additionally needs a GUI login session. `None` if Metal is
    /// unavailable / the pipeline fails.
    pub fn new_windowed(mtm: MainThreadMarker) -> Option<MacPresenter> {
        let application = NativeApplication::ensure(mtm);
        let ctx = MetalCtx::new()?;
        let pipeline = ctx.make_composite_pipeline()?;
        let (presentation_tx, presentation_rx) = mpsc::channel();
        let mut presenter = MacPresenter {
            _application: Some(application),
            ctx,
            pipeline,
            surfaces: HashMap::new(),
            retired_native_presents: VecDeque::new(),
            mtm: Some(mtm),
            frames: 0,
            capture: None,
            requested_capture: None,
            events: Vec::new(),
            drag_event: None,
            native_resize: None,
            key_modifiers: Modifiers::default(),
            gestures: Gestures::default(),
            tablet: Tablet::default(),
            pasteboard_change: unsafe { NSPasteboard::generalPasteboard().changeCount() },
            presentation_tx,
            presentation_rx,
            next_submission: 1,
            present_surfaces: HashMap::new(),
            wake: None,
            cursor: HostCursorState::default(),
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

    /// Capture the first presented frame after `request` appears in `directory`.
    pub fn capture_once_to(mut self, directory: impl Into<PathBuf>) -> io::Result<Self> {
        self.requested_capture = Some(Capture::requested(directory)?);
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

    /// Attach BGRA content with optional buffer-coordinate damage. The current three-slot Metal rotation
    /// refreshes only its selected slot, but refreshes that slot completely because damage is relative to
    /// the immediately preceding frame rather than the older contents of a rotated slot.
    pub fn attach_bgra_damage(
        &mut self,
        sid: SurfaceId,
        bgra: Vec<u8>,
        w: u32,
        h: u32,
        _damage: Option<Vec<Rect>>,
    ) {
        let state = self.surfaces.entry(sid).or_insert_with(SurfState::new);
        state.content = Some(Content::Bgra { bgra, w, h });
        state.native_submission = None;
    }

    /// Attach an owned IOSurface lease as surface `sid`'s content.
    pub fn attach_iosurface(&mut self, sid: SurfaceId, surface: IOSurface) {
        let state = self.surfaces.entry(sid).or_insert_with(SurfState::new);
        state.content = Some(Content::IoSurface(surface));
        state.native_submission = None;
    }

    fn reap_native_presents(&mut self) {
        let now = Instant::now();
        self.retired_native_presents
            .retain(|(_, present)| !present.poll(now));
        for state in self.surfaces.values_mut() {
            state.native_presents.retain(|present| !present.poll(now));
        }
        while let Ok(event) = self.presentation_rx.try_recv() {
            if let PresenterEvent::Presentation(completion) = event {
                let surface = self.present_surfaces.remove(&completion.id);
                if completion.outcome == CompletionOutcome::TerminalFailure {
                    if let Some(surface) = surface {
                        if let Some(state) = self.surfaces.get_mut(&surface) {
                            state.native_submission = None;
                        }
                        self.events.push(PresenterEvent::Repaint(surface));
                    }
                }
            }
            self.events.push(event);
        }
    }

    fn sync_key_modifiers(&mut self, flags: NSEventModifierFlags) {
        self.events.extend(self.key_modifiers.update(flags));
    }

    /// Retire attached pixel resources while preserving the native window lifecycle. A null-buffer
    /// commit unmaps content; it does not destroy the xdg window role.
    pub fn forget(&mut self, sid: SurfaceId) {
        if let Some(state) = self.surfaces.get_mut(&sid) {
            state.content = None;
            state.native_submission = None;
            state.composite = None;
            state.frame = None;
            state.uploads.clear();
        }
    }

    fn destroy(&mut self, sid: SurfaceId) {
        if let Some(mut state) = self.surfaces.remove(&sid) {
            self.retired_native_presents.extend(
                state
                    .native_presents
                    .drain(..)
                    .map(|present| (sid, present)),
            );
            if let Some(window) = state.window {
                window.close();
            }
        }
    }

    /// Read surface `sid`'s last composited frame back as `(w, h, RGBA)` — the GUI-session-free proof of
    /// what the presenter put on (or would put on) screen.
    pub fn last_rgba(&self, sid: SurfaceId) -> Option<(u32, u32, Vec<u8>)> {
        let state = self.surfaces.get(&sid)?;
        let (w, h, tex) = state.frame.as_ref().or(state.composite.as_ref())?;
        let bgra = self.ctx.readback_bgra(tex, *w, *h);
        Some((*w, *h, BgraFrame::new(&bgra).rgba()))
    }

    #[cfg(test)]
    fn attached_iosurface_bgra(&self, sid: SurfaceId) -> Option<(usize, usize, Vec<u8>)> {
        let Content::IoSurface(surface) = self.surfaces.get(&sid)?.content.as_ref()? else {
            return None;
        };
        let (width, height, _) = surface.dimensions();
        Some((width, height, surface.read_bgra().ok()?))
    }
}

#[cfg(test)]
include!("present/tests.rs");
