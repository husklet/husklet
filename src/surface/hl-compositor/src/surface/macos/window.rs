//! The Cocoa window side: one `NSWindow` + `CAMetalLayer` per surface. Ported (trimmed) from
//! `hl-display::present_cocoa` — enough to open a real window, size its layer in device pixels for a
//! retina-crisp present, and blit a composited `MTLTexture` into the layer's next drawable.
//!
//! Everything here needs the AppKit main thread (`MainThreadMarker`) + a GUI login session to become
//! visible. The presenter falls back to a headless offscreen target when no window is available.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSColor, NSEvent, NSScreen, NSView, NSWindow, NSWindowButton, NSWindowCollectionBehavior,
    NSWindowOrderingMode, NSWindowSharingType, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLTexture,
};
use objc2_quartz_core::{kCAGravityTopLeft, CAMetalDrawable, CAMetalLayer};
use std::cell::Cell;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;
use std::time::Instant;

use super::layer;
use super::metal::MetalCtx;
use super::present::submission::{drawable_matches, DisplayTiming, NativePresent, PresentAttempt};
use crate::scene::model::Visibility;
use crate::scene::port::{PresentationId, PresenterEvent, Wake};
use std::sync::Arc;

#[path = "window_application.rs"]
mod application;
mod create;

pub use application::{DisplayConfig, NativeApplication};

/// A native window backing one surface: an `NSWindow` whose content view hosts a `CAMetalLayer`.
pub struct MetalWindow {
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
    /// Logical surface size in points (window content size).
    size: Cell<(u32, u32)>,
    /// Requested AppKit full-screen state. Kept separately because `toggleFullScreen` transitions
    /// asynchronously and the style-mask bit does not change immediately.
    fullscreen: Cell<bool>,
    maximized: Cell<bool>,
    /// Floating frame restored when XDG maximize is withdrawn. `None` until the first maximize.
    floating_frame: Cell<Option<NSRect>>,
    /// `presentedTime` of the last frame this layer actually put on screen (`0` = none). Shared with each
    /// submission's presented handler, which runs off the main thread — it is the only evidence available
    /// for whether presentation is landing on the display's refresh cadence.
    last_presented_ns: Arc<AtomicU64>,
}

/// State for a compositor-driven native edge resize. AppKit's ordinary titled-window resize enters a
/// modal tracking loop inside `sendEvent`, which prevents the Wayland loop from delivering intermediate
/// XDG configures. Keeping the drag in the presenter preserves native edge hit targets without blocking.
pub struct ResizeDrag {
    start_mouse: NSPoint,
    start_frame: NSRect,
    left: bool,
    right: bool,
    bottom: bool,
    top: bool,
}

impl MetalWindow {
    pub fn set_title(&self, title: &str) {
        self.window.setTitle(&NSString::from_str(title));
    }

    pub fn set_size_constraints(
        &self,
        min: (Option<i32>, Option<i32>),
        max: (Option<i32>, Option<i32>),
    ) {
        unsafe {
            self.window.setContentMinSize(NSSize::new(
                f64::from(min.0.unwrap_or(1).max(1)),
                f64::from(min.1.unwrap_or(1).max(1)),
            ));
            self.window.setContentMaxSize(NSSize::new(
                max.0.map_or(f64::MAX, |value| f64::from(value.max(1))),
                max.1.map_or(f64::MAX, |value| f64::from(value.max(1))),
            ));
        }
    }

    /// Apply explicit xdg window state. Never infer a native mode from buffer dimensions: ordinary
    /// resize/tiling frames can equal an output dimension transiently and must not start a Space change.
    pub fn set_mode(&self, maximized: bool, fullscreen: bool) {
        // Only XDG fullscreen enters a separate macOS Space. XDG maximize (including a GTK titlebar
        // double-click) is ordinary zoom into the current screen's visible work area.
        let native_fullscreen = self.native_fullscreen();
        self.fullscreen.set(fullscreen);
        if native_fullscreen != fullscreen {
            self.window.toggleFullScreen(None);
        }
        if fullscreen {
            self.maximized.set(maximized);
            return;
        }
        let was_maximized = self.maximized.replace(maximized);
        if maximized && !was_maximized {
            self.floating_frame.set(Some(self.window.frame()));
            if let Some(screen) = self.window.screen() {
                self.window.setFrame_display(screen.visibleFrame(), true);
            }
        } else if !maximized && was_maximized {
            if let Some(frame) = self.floating_frame.take() {
                self.window.setFrame_display(frame, true);
            }
        }
    }

    /// AppKit's completed mode, as opposed to the most recently requested XDG mode. Full-screen
    /// transitions are asynchronous and users can leave them through native controls, so callers must
    /// observe the style mask instead of treating `toggleFullScreen` as an immediate state assignment.
    pub fn native_fullscreen(&self) -> bool {
        self.window
            .styleMask()
            .contains(NSWindowStyleMask::FullScreen)
    }

    /// Break AppKit's retaining parent/child relationship and remove this transient immediately.
    pub fn close(&self) {
        unsafe {
            if let Some(parent) = self.window.parentWindow() {
                parent.removeChildWindow(&self.window);
            }
        }
        self.window.orderOut(None);
        self.window.close();
    }

    /// Offset independently mapped toplevels so a newly opened window does not exactly cover its owner.
    pub fn cascade(&self, index: usize) {
        if index == 0 {
            return;
        }
        let offset = (index.min(8) * 36) as f64;
        let mut origin = self.window.frame().origin;
        origin.x += offset;
        origin.y -= offset;
        self.set_screen_origin(origin);
    }

    /// Keep the host frame in lockstep with the committed Wayland logical size. XDG maximize/fullscreen
    /// changes arrive as a configure followed by a newly-sized client buffer; that buffer size is the
    /// presenter's authoritative transition point.
    pub fn set_logical_size(&self, w: u32, h: u32) {
        let next = (w.max(1), h.max(1));
        if self.size.get() == next && self.logical_size() == next {
            return;
        }

        if !self.fullscreen.get() && !self.maximized.get() {
            // FullSizeContentView gives the guest the complete native frame. `setContentSize`
            // nevertheless adds AppKit's hidden titlebar inset, causing the window to grow vertically
            // after a move/resize feedback cycle. Set the authoritative frame extent directly.
            self.window.setFrame_display(
                NSRect::new(
                    self.window.frame().origin,
                    NSSize::new(next.0 as f64, next.1 as f64),
                ),
                true,
            );
        }
        self.size.set(next);
    }

    /// Size the layer's drawable to `w`×`h` DEVICE pixels so a `copyFromTexture_toTexture` blit of a
    /// composite target of that exact size into the drawable matches dimensions. Called before present.
    pub fn set_drawable_size(&self, w: u32, h: u32) {
        let scale = self.backing_scale();
        unsafe {
            self.layer.setContentsScale(scale);
            self.layer.setDrawableSize(NSSize::new(w as f64, h as f64));
        }
    }

    pub fn backing_scale(&self) -> f64 {
        self.window.backingScaleFactor().max(1.0)
    }

    /// What this window's display can actually attest about a present: the target screen's refresh
    /// interval and whether the layer is in display-sync mode. A window on no screen reports an unknown
    /// (`0`) interval rather than a plausible-looking guess.
    fn display_timing(&self) -> DisplayTiming {
        let refresh_ns = self
            .window
            .screen()
            .map(|screen| unsafe { screen.maximumFramesPerSecond() })
            .filter(|hz| *hz > 0)
            .map(|hz| 1_000_000_000u64 / hz as u64)
            .unwrap_or(0);
        DisplayTiming {
            refresh_ns,
            display_sync: unsafe { self.layer.displaySyncEnabled() },
            last_presented_ns: Arc::clone(&self.last_presented_ns),
        }
    }

    /// Whether AppKit has ordered the window on screen. A window covered by another app remains visible
    /// and must keep accepting frames; desktop occlusion is not compositor minimization policy.
    pub fn is_visible(&self) -> bool {
        self.window.isVisible()
    }

    pub fn logical_size(&self) -> (u32, u32) {
        // FullSizeContentView makes the guest own the complete frameless window. The native frame is
        // therefore the logical surface extent. `contentRectForFrameRect` still subtracts AppKit's hidden
        // titlebar inset and produces a false height during resize/tiling, while a layer-backed view may
        // not lay out until the next run-loop pass. The frame changes synchronously and is authoritative.
        let size = self.window.frame().size;
        (
            size.width.round().max(1.0) as u32,
            size.height.round().max(1.0) as u32,
        )
    }

    pub fn number(&self) -> isize {
        unsafe { self.window.windowNumber() }
    }

    /// Convert AppKit's bottom-left window coordinate to Wayland's top-left logical coordinate.
    pub fn wayland_point(&self, x: f64, y: f64) -> (f64, f64) {
        let height = f64::from(self.logical_size().1);
        (x, (height - y).max(0.0))
    }

    /// Convert a Wayland child-popup origin (parent-content-local, top-left/y-down) into AppKit screen
    /// coordinates (bottom-left/y-up). `convertRectToScreen` accounts for the parent's current Space,
    /// full-screen transition, titlebar/content inset, and display origin.
    pub fn popup_origin(&self, x: i32, y: i32, popup_height: u32) -> NSPoint {
        let content_height = f64::from(self.logical_size().1);
        let local = NSRect::new(
            NSPoint::new(x as f64, content_height - y as f64 - popup_height as f64),
            NSSize::new(1.0, 1.0),
        );
        self.window.convertRectToScreen(local).origin
    }

    /// Reposition a popup window without resizing it.
    pub fn set_screen_origin(&self, origin: NSPoint) {
        // SAFETY: the retained NSWindow is alive and all presenter/window calls are confined to AppKit's
        // main thread by the MainThreadMarker used to construct this MetalWindow.
        unsafe { self.window.setFrameOrigin(origin) };
    }

    /// Apply the compositor's visibility state to the native window.
    pub fn set_visibility(&self, visibility: Visibility) {
        match visibility {
            Visibility::Visible if self.window.isMiniaturized() => unsafe {
                self.window.deminiaturize(None)
            },
            Visibility::Visible if !self.window.isVisible() => {
                self.window.makeKeyAndOrderFront(None)
            }
            Visibility::Occluded if self.window.isVisible() => self.window.orderOut(None),
            Visibility::Minimized if !self.window.isMiniaturized() => self.window.miniaturize(None),
            _ => {}
        }
    }

    /// Blit a composited texture into the layer's next drawable and retain the submitted native work.
    /// A missing or asynchronously resized drawable is retryable; no frame is reported displayed.
    pub fn present(
        &self,
        ctx: &MetalCtx,
        composite: &ProtocolObject<dyn MTLTexture>,
        queue_depth: usize,
        id: PresentationId,
        events: Sender<PresenterEvent>,
        wake: Option<Arc<dyn Wake>>,
    ) -> PresentAttempt {
        if !self.is_visible() {
            return PresentAttempt::Retry;
        }
        if !layer::can_acquire(queue_depth) {
            layer::record_acquire(std::time::Duration::ZERO, queue_depth, false);
            return PresentAttempt::Retry;
        }
        let started = Instant::now();
        let drawable = unsafe { self.layer.nextDrawable() };
        layer::record_acquire(started.elapsed(), queue_depth, drawable.is_some());
        let Some(drawable) = drawable else {
            return PresentAttempt::Retry;
        };
        let dst = unsafe { drawable.texture() };
        let source_size = (composite.width(), composite.height());
        let drawable_size = (dst.width(), dst.height());
        if !drawable_matches(composite, &dst) {
            hl_log::hl_log!(
                hl_log::tag::PRESENT,
                hl_log::Level::Warn,
                "skip native frame: composite={}x{} drawable={}x{} logical={}x{}",
                source_size.0,
                source_size.1,
                drawable_size.0,
                drawable_size.1,
                self.logical_size().0,
                self.logical_size().1,
            );
            return PresentAttempt::Retry;
        }
        let Some(cmd) = ctx.queue.commandBuffer() else {
            return PresentAttempt::Terminal;
        };
        let Some(blit) = cmd.blitCommandEncoder() else {
            return PresentAttempt::Terminal;
        };
        unsafe { blit.copyFromTexture_toTexture(composite, &dst) };
        blit.endEncoding();
        cmd.presentDrawable(ProtocolObject::from_ref(&*drawable));
        let present = NativePresent::new(
            id,
            cmd.clone(),
            drawable,
            self.display_timing(),
            events,
            wake,
        );
        cmd.commit();
        PresentAttempt::Submitted(present)
    }
}

#[cfg(test)]
#[path = "window/tests.rs"]
mod tests;
