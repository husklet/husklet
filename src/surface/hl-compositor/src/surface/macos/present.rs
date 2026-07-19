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
    NSApplication, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSPasteboard,
    NSPasteboardTypeString,
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
    key_modifiers: u8,
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
            key_modifiers: 0,
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
            key_modifiers: 0,
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
        const CTRL: u8 = 1;
        const SHIFT: u8 = 2;
        const ALT: u8 = 4;
        let mut next = 0;
        // macOS Command is the platform's primary shortcut modifier; Linux GUI clients conventionally
        // bind those actions to Control. Physical Control joins the same logical modifier.
        if flags.intersects(
            NSEventModifierFlags::NSEventModifierFlagCommand
                | NSEventModifierFlags::NSEventModifierFlagControl,
        ) {
            next |= CTRL;
        }
        if flags.contains(NSEventModifierFlags::NSEventModifierFlagShift) {
            next |= SHIFT;
        }
        if flags.contains(NSEventModifierFlags::NSEventModifierFlagOption) {
            next |= ALT;
        }
        for (bit, keycode) in [(CTRL, 29), (SHIFT, 42), (ALT, 56)] {
            if self.key_modifiers & bit != next & bit {
                self.events.push(PresenterEvent::Key {
                    keycode,
                    pressed: next & bit != 0,
                });
            }
        }
        self.key_modifiers = next;
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
        let state = self
            .surfaces
            .entry(desired.surface)
            .or_insert_with(SurfState::new);
        let previous = state.desired.clone();
        if previous.as_ref().map(|window| window.visibility) != Some(desired.visibility) {
            hl_debug!(
                tag::PRESENT,
                "native window surface={} visibility={:?}->{:?}",
                desired.surface.0,
                previous.as_ref().map(|window| window.visibility),
                desired.visibility
            );
        }
        let parent = match desired.kind {
            WindowKind::Toplevel { parent } => parent,
            WindowKind::Popup { parent, .. } => Some(parent),
        };
        state.desired = Some(desired.clone());
        if let Some(window) = &state.window {
            if previous
                .as_ref()
                .is_none_or(|previous| previous.title != desired.title)
            {
                window.set_title(&desired.title);
            }
            if previous.as_ref().is_none_or(|previous| {
                previous.min_size != desired.min_size || previous.max_size != desired.max_size
            }) {
                window.set_size_constraints(desired.min_size, desired.max_size);
            }
            if previous.as_ref().is_none_or(|previous| {
                previous.maximized != desired.maximized || previous.fullscreen != desired.fullscreen
            }) {
                window.set_mode(desired.maximized, desired.fullscreen);
                state.reported_native_size = Some(window.logical_size());
                state.native_resize_pending = None;
                state.native_resize_last_sent = None;
            }
            if previous
                .as_ref()
                .is_none_or(|previous| previous.visibility != desired.visibility)
            {
                window.set_visibility(desired.visibility);
            }
        }
        let previous_parent = previous.as_ref().and_then(|window| match window.kind {
            WindowKind::Toplevel { parent } => parent,
            WindowKind::Popup { parent, .. } => Some(parent),
        });
        if previous_parent != parent {
            if let Some(window) = self
                .surfaces
                .get(&desired.surface)
                .and_then(|state| state.window.as_ref())
            {
                window.detach_parent();
            }
        }
        if let Some(parent) = parent.filter(|_| previous_parent != parent) {
            if let (Some(owner), Some(child)) = (
                self.surfaces
                    .get(&parent)
                    .and_then(|state| state.window.as_ref()),
                self.surfaces
                    .get(&desired.surface)
                    .and_then(|state| state.window.as_ref()),
            ) {
                owner.add_child(child);
            }
        }
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
        let _span = hl_span!(tag::PRESENT, "macos_poll_events");
        let Some(mtm) = self.mtm else { return };
        let app = NSApplication::sharedApplication(mtm);
        // A nil `untilDate` tells AppKit to WAIT for the next event.  This method is called from the
        // Wayland/calloop serve loop and must only drain events already queued; blocking here prevents
        // subsequent client requests from being dispatched (a GTK client connects, then hangs before its
        // first surface can map). `distantPast` is AppKit's documented non-blocking poll deadline.
        let deadline = unsafe { NSDate::distantPast() };
        unsafe {
            while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&deadline),
                NSDefaultRunLoopMode,
                true,
            ) {
                let mut consumed = false;
                let event_type = event.r#type();
                let window_number = event.windowNumber();
                let window = self.surfaces.iter().find_map(|(&surface, state)| {
                    state.window.as_ref().and_then(|window| {
                        (window.number() == window_number).then_some((
                            surface,
                            window,
                            state.input_origin,
                        ))
                    })
                });
                if let Some((surface, window, input_origin)) = window {
                    match event_type {
                        NSEventType::MouseMoved
                        | NSEventType::LeftMouseDragged
                        | NSEventType::RightMouseDragged
                        | NSEventType::OtherMouseDragged => {
                            if event_type == NSEventType::LeftMouseDragged {
                                if let Some((resize_surface, drag)) = &self.native_resize {
                                    if *resize_surface == surface {
                                        window.update_resize(drag);
                                        continue;
                                    }
                                }
                            }
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            let (x, y) = (x + input_origin.0, y + input_origin.1);
                            self.events.push(PresenterEvent::PointerMotion {
                                window: surface,
                                x,
                                y,
                            });
                        }
                        NSEventType::LeftMouseDown
                        | NSEventType::LeftMouseUp
                        | NSEventType::RightMouseDown
                        | NSEventType::RightMouseUp
                        | NSEventType::OtherMouseDown
                        | NSEventType::OtherMouseUp => {
                            if event_type == NSEventType::LeftMouseDown {
                                if let Some(drag) = window.begin_resize(event.locationInWindow()) {
                                    self.native_resize = Some((surface, drag));
                                    continue;
                                }
                            }
                            if event_type == NSEventType::LeftMouseUp
                                && self
                                    .native_resize
                                    .as_ref()
                                    .is_some_and(|(resize_surface, _)| *resize_surface == surface)
                            {
                                self.native_resize = None;
                                continue;
                            }
                            // Focus the native window that produced this event before routing its pointer
                            // coordinates; multi-window hit testing uses keyboard focus as its z-order key.
                            self.events.push(PresenterEvent::Focus(surface));
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            let (x, y) = (x + input_origin.0, y + input_origin.1);
                            self.events.push(PresenterEvent::PointerMotion {
                                window: surface,
                                x,
                                y,
                            });
                            let pressed = matches!(
                                event_type,
                                NSEventType::LeftMouseDown
                                    | NSEventType::RightMouseDown
                                    | NSEventType::OtherMouseDown
                            );
                            if pressed && event_type == NSEventType::LeftMouseDown {
                                self.drag_event = Some(event.clone());
                            }
                            let button = 0x110 + event.buttonNumber().max(0) as u32;
                            self.events.push(PresenterEvent::PointerButton {
                                window: surface,
                                button,
                                pressed,
                                click_count: event.clickCount().clamp(1, u8::MAX as isize) as u8,
                            });
                            if !pressed {
                                self.drag_event = None;
                            }
                        }
                        NSEventType::ScrollWheel => self.events.push(PresenterEvent::PointerAxis {
                            horizontal: -event.scrollingDeltaX(),
                            vertical: -event.scrollingDeltaY(),
                        }),
                        NSEventType::KeyDown | NSEventType::KeyUp => {
                            self.sync_key_modifiers(event.modifierFlags());
                            if let Some(keycode) = KeyCode(event.keyCode()).evdev() {
                                self.events.push(PresenterEvent::Key {
                                    keycode,
                                    pressed: event_type == NSEventType::KeyDown,
                                });
                            }
                            consumed = true;
                        }
                        NSEventType::FlagsChanged => {
                            self.sync_key_modifiers(event.modifierFlags());
                            consumed = true;
                        }
                        _ => {}
                    }
                }
                if !consumed {
                    app.sendEvent(&event);
                }
            }
            // AppKit window operations are not event-only. Native full-screen transitions, animation,
            // notifications, and Space hand-off also run through main-run-loop sources and timers. Give
            // them a tightly bounded slice every compositor tick; without this `toggleFullScreen` creates
            // transition windows but never completes. One millisecond keeps Wayland input/render latency
            // bounded while allowing Cocoa to make forward progress.
            let until = NSDate::dateWithTimeIntervalSinceNow(0.001);
            NSRunLoop::mainRunLoop().runMode_beforeDate(NSDefaultRunLoopMode, &until);
        }
        for (&surface, state) in &mut self.surfaces {
            let Some(window) = state.window.as_ref() else {
                continue;
            };
            if !matches!(
                state.desired.as_ref().map(|window| window.kind),
                Some(WindowKind::Toplevel { .. })
            ) {
                continue;
            }
            let size = window.logical_size();
            let native_fullscreen = window.native_fullscreen();
            let fullscreen_changed = state
                .observed_native_fullscreen
                .replace(native_fullscreen)
                .is_some_and(|previous| previous != native_fullscreen);
            if state.reported_native_size.is_none() {
                state.reported_native_size = Some(size);
            } else if state.reported_native_size != Some(size) {
                hl_debug!(
                    tag::PRESENT,
                    "native frame changed surface={} size={}x{}",
                    surface.0,
                    size.0,
                    size.1
                );
                state.reported_native_size = Some(size);
                // While entering full-screen AppKit briefly reports intermediate frames before the
                // FullScreen style becomes authoritative. The client already has its XDG configure;
                // do not turn those animation frames into contradictory windowed configures.
                if state
                    .desired
                    .as_ref()
                    .is_some_and(|desired| desired.fullscreen)
                    && !native_fullscreen
                    && !fullscreen_changed
                {
                    continue;
                }
                let live_resize = self
                    .native_resize
                    .as_ref()
                    .is_some_and(|(resize_surface, _)| *resize_surface == surface);
                let maximized = !live_resize
                    && !native_fullscreen
                    && state
                        .desired
                        .as_ref()
                        .is_some_and(|desired| desired.maximized);
                state.native_resize_pending =
                    Some((size.0, size.1, maximized, native_fullscreen, live_resize));
                state.native_resize_changed_at = Some(Instant::now());
            } else if fullscreen_changed {
                // A native full-screen exit can finish without a final size delta. Still acknowledge
                // the mode change so stale XDG state cannot request full-screen again.
                state.native_resize_pending =
                    Some((size.0, size.1, false, native_fullscreen, false));
                state.native_resize_changed_at = Some(Instant::now());
            }
        }
        let now = Instant::now();
        for (&surface, state) in &mut self.surfaces {
            // Coalesce native geometry changes to at most one configure per display frame. Always keep
            // `native_resize_pending` at the newest AppKit size; XDG permits clients to acknowledge the
            // latest configure directly, and rendering superseded sizes creates seconds of resize debt.
            if let Some((width, height, maximized, fullscreen, resizing)) =
                state.native_resize_pending
            {
                let due = state
                    .native_resize_sent_at
                    .is_none_or(|sent| now.duration_since(sent) >= Duration::from_millis(8));
                if due
                    && state.native_resize_last_sent
                        != Some((width, height, maximized, fullscreen, resizing))
                {
                    state.native_resize_sent_at = Some(now);
                    state.native_resize_last_sent =
                        Some((width, height, maximized, fullscreen, resizing));
                    self.events.push(PresenterEvent::Resize {
                        surface,
                        width,
                        height,
                        maximized,
                        fullscreen,
                        resizing,
                    });
                }
            }
            if state
                .native_resize_changed_at
                .is_some_and(|changed| now.duration_since(changed) >= Duration::from_millis(75))
            {
                state.native_resize_changed_at = None;
                // If the drag ended between pacing slots, emit its exact final size before clearing the
                // XDG resizing state. This prevents the last few pixels from arriving after ResizeEnd.
                if let Some((width, height, maximized, fullscreen, resizing)) =
                    state.native_resize_pending
                {
                    if state.native_resize_last_sent
                        != Some((width, height, maximized, fullscreen, resizing))
                    {
                        self.events.push(PresenterEvent::Resize {
                            surface,
                            width,
                            height,
                            maximized,
                            fullscreen,
                            resizing,
                        });
                    }
                }
                state.native_resize_sent_at = None;
                state.native_resize_last_sent = None;
                self.events.push(PresenterEvent::ResizeEnd { surface });
            }
        }
    }

    fn take_events(&mut self) -> Vec<PresenterEvent> {
        std::mem::take(&mut self.events)
    }

    fn present(
        &mut self,
        _output: OutputId,
        image: &PresentableImage,
        _damage: &[Rect],
        _timing: PresentTiming,
    ) -> PresentationFeedback {
        let _span = hl_span!(tag::PRESENT, "macos_present");
        let sid = image.surface;
        let (w, h) = match self.compose(image) {
            Ok(dims) => dims,
            Err(err) => {
                eprintln!("[macos-surface] present sid={sid:?}: {err}");
                // No content / unresolvable device resource: retryable — the compositor keeps pacing so a
                // re-attached buffer next cycle can succeed.
                return PresentationFeedback {
                    outcome: PresentOutcome::RetryableFailure,
                };
            }
        };
        self.frames += 1;

        // Windowed mode: open the window lazily (sized to the image's logical points), size its drawable
        // to the composite's device pixels, and blit. If no window/session is up the frame stays offscreen.
        let has_window_role = self
            .surfaces
            .get(&sid)
            .is_some_and(|state| state.desired.is_some());
        let shown = if let (Some(mtm), true) = (self.mtm, has_window_role) {
            let desired = self
                .surfaces
                .get(&sid)
                .and_then(|state| state.desired.clone());
            let popup = desired.as_ref().and_then(|window| match window.kind {
                WindowKind::Popup { parent, position } => Some((parent, position)),
                WindowKind::Toplevel { .. } => None,
            });
            let transient_parent = desired.as_ref().and_then(|window| match window.kind {
                WindowKind::Toplevel { parent } => parent,
                WindowKind::Popup { .. } => None,
            });
            let content_origin = desired
                .as_ref()
                .and_then(|window| window.geometry)
                .map(|geometry| (f64::from(geometry.x), f64::from(geometry.y)))
                .unwrap_or((0.0, 0.0));
            let visible_size = desired
                .as_ref()
                .and_then(|window| {
                    window
                        .geometry
                        .map(|geometry| (geometry.w, geometry.h))
                        .or(window.logical_size)
                })
                .unwrap_or((image.width, image.height));
            let popup_origin = popup.and_then(|(parent, (x, y))| {
                self.surfaces
                    .get(&parent)
                    .and_then(|parent| parent.window.as_ref())
                    .map(|parent| parent.popup_origin(x, y, visible_size.1.max(1) as u32))
            });
            let root_origin = popup.map_or((0.0, 0.0), |(parent_id, (x, y))| {
                let parent = self
                    .surfaces
                    .get(&parent_id)
                    .map(|state| state.input_origin)
                    .unwrap_or((0.0, 0.0));
                (parent.0 + f64::from(x), parent.1 + f64::from(y))
            });
            let input_origin = (
                root_origin.0 + content_origin.0,
                root_origin.1 + content_origin.1,
            );
            let toplevel_index = if popup.is_none() && transient_parent.is_none() {
                self.surfaces
                    .values()
                    .filter(|state| state.window.is_some() && state.input_origin == (0.0, 0.0))
                    .count()
            } else {
                0
            };
            let created = {
                let st = self.surfaces.get_mut(&sid).unwrap();
                st.input_origin = input_origin;
                if st.window.is_none() {
                    let title = if desired
                        .as_ref()
                        .is_none_or(|window| window.title.is_empty())
                    {
                        format!("hl surface {}", sid.0)
                    } else {
                        desired.as_ref().unwrap().title.clone()
                    };
                    let window = if popup.is_some() {
                        MetalWindow::new_popup(
                            mtm,
                            &self.ctx,
                            visible_size.0.max(1) as u32,
                            visible_size.1.max(1) as u32,
                            &title,
                        )
                    } else {
                        MetalWindow::new(
                            mtm,
                            &self.ctx,
                            visible_size.0.max(1) as u32,
                            visible_size.1.max(1) as u32,
                            &title,
                        )
                    };
                    window.set_size_constraints(
                        desired.as_ref().map_or((None, None), |w| w.min_size),
                        desired.as_ref().map_or((None, None), |w| w.max_size),
                    );
                    if let Some(desired) = desired.as_ref() {
                        window.set_mode(desired.maximized, desired.fullscreen);
                    }
                    let visibility = desired
                        .as_ref()
                        .map_or(Visibility::Visible, |window| window.visibility);
                    if visibility != Visibility::Visible {
                        window.set_visibility(visibility);
                    }
                    if popup.is_none() && transient_parent.is_none() {
                        window.cascade(toplevel_index);
                    }
                    st.window = Some(window);
                    true
                } else {
                    false
                }
            };
            if created {
                let parent = popup.map(|(parent, _)| parent).or(transient_parent);
                if let Some(parent) = parent {
                    if let (Some(parent), Some(child)) = (
                        self.surfaces
                            .get(&parent)
                            .and_then(|state| state.window.as_ref()),
                        self.surfaces
                            .get(&sid)
                            .and_then(|state| state.window.as_ref()),
                    ) {
                        parent.add_child(child);
                    }
                }
            }
            let st = self.surfaces.get_mut(&sid).unwrap();
            let win = st.window.as_ref().unwrap();
            if let Some(origin) = popup_origin {
                win.set_screen_origin(origin);
            }
            let desired_native_size = (visible_size.0.max(1) as u32, visible_size.1.max(1) as u32);
            match st.native_resize_pending {
                Some((width, height, _, _, _)) if (width, height) != desired_native_size => {
                    // AppKit owns the live bounds until the client commits the configure. Contents gravity
                    // preserves the old drawable's aspect instead of stretching it in the interim.
                }
                _ => {
                    st.native_resize_pending = None;
                    st.native_resize_last_sent = None;
                    win.set_logical_size(desired_native_size.0, desired_native_size.1);
                    st.reported_native_size = Some(desired_native_size);
                }
            }
            win.set_drawable_size(w, h);
            let composite = st.composite.as_ref().unwrap().2.clone();
            win.present(&self.ctx, &composite)
        } else {
            false
        };

        if let Some(capture) = &self.capture {
            if let Some((capture_w, capture_h, rgba)) = self.last_rgba(sid) {
                if let Err(err) = capture.write(sid, capture_w, capture_h, &rgba) {
                    eprintln!("[macos-surface] capture sid={sid:?}: {err}");
                }
            }
        }

        if shown {
            self.serial += 1;
            PresentationFeedback::delivered(self.serial, None)
        } else {
            // Composited into the backing target but not visibly shown (headless, or window not yet on
            // screen). Honest `Offscreen` so the schedule service does not advance pacing as if it shipped.
            PresentationFeedback::offscreen()
        }
    }
}

/// macOS virtual key code to Linux evdev. Covers the standard ANSI keyboard; unknown media/vendor keys
/// are deliberately ignored instead of emitting the wrong key.
struct KeyCode(u16);

impl KeyCode {
    fn evdev(self) -> Option<u32> {
        Some(match self.0 {
            0 => 30,
            1 => 31,
            2 => 32,
            3 => 33,
            4 => 35,
            5 => 34,
            6 => 44,
            7 => 45,
            8 => 46,
            9 => 47,
            11 => 48,
            12 => 16,
            13 => 17,
            14 => 18,
            15 => 19,
            16 => 21,
            17 => 20,
            18 => 2,
            19 => 3,
            20 => 4,
            21 => 5,
            22 => 7,
            23 => 6,
            24 => 13,
            25 => 10,
            26 => 8,
            27 => 12,
            28 => 9,
            29 => 11,
            30 => 27,
            31 => 24,
            32 => 22,
            33 => 26,
            34 => 23,
            35 => 25,
            36 => 28,
            37 => 38,
            38 => 36,
            39 => 40,
            40 => 37,
            41 => 39,
            42 => 43,
            43 => 51,
            44 => 53,
            45 => 49,
            46 => 50,
            47 => 52,
            48 => 15,
            49 => 57,
            50 => 41,
            51 => 14,
            53 => 1,
            123 => 105,
            124 => 106,
            125 => 108,
            126 => 103,
            _ => return None,
        })
    }
}
