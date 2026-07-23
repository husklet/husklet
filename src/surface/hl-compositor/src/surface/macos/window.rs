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

use super::metal::MetalCtx;
use crate::scene::model::Visibility;

mod application;

pub use application::{DisplayConfig, NativeApplication};

/// A native window backing one surface: an `NSWindow` whose content view hosts a `CAMetalLayer`.
pub struct MetalWindow {
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
    /// Device pixels per point (`backingScaleFactor`); the layer drawable is sized `size * scale`.
    #[allow(dead_code)] // read by pixel_size (the retina readback helper)
    scale: f64,
    /// Logical surface size in points (window content size).
    #[allow(dead_code)] // read by pixel_size (the retina readback helper)
    size: Cell<(u32, u32)>,
    /// Requested AppKit full-screen state. Kept separately because `toggleFullScreen` transitions
    /// asynchronously and the style-mask bit does not change immediately.
    fullscreen: Cell<bool>,
    maximized: Cell<bool>,
    /// Floating frame restored when XDG maximize is withdrawn. `None` until the first maximize.
    floating_frame: Cell<Option<NSRect>>,
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
    /// Open a titled `NSWindow` of logical size `w`×`h` points, install a `CAMetalLayer` sized in device
    /// pixels, and order it front. Must run on the AppKit main thread.
    pub fn new(mtm: MainThreadMarker, ctx: &MetalCtx, w: u32, h: u32, title: &str) -> MetalWindow {
        Self::new_kind(mtm, ctx, w, h, title, false)
    }

    pub fn new_popup(
        mtm: MainThreadMarker,
        ctx: &MetalCtx,
        w: u32,
        h: u32,
        title: &str,
    ) -> MetalWindow {
        Self::new_kind(mtm, ctx, w, h, title, true)
    }

    fn new_kind(
        mtm: MainThreadMarker,
        ctx: &MetalCtx,
        w: u32,
        h: u32,
        title: &str,
        popup: bool,
    ) -> MetalWindow {
        let scale = NSScreen::mainScreen(mtm)
            .map(|s| s.backingScaleFactor())
            .unwrap_or(1.0)
            .max(1.0);
        let content = NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(w as f64, h as f64));
        // Keep the native window key-capable while making its frame visually absent. A raw Borderless
        // NSWindow refuses key status by default, which silently drops keyboard and normal pointer
        // routing. FullSizeContentView + a hidden transparent titlebar is AppKit's key-capable frameless
        // pattern; the guest-drawn GTK/Chrome controls remain the only visible controls.
        let style = if popup {
            NSWindowStyleMask::Borderless
        } else {
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::FullSizeContentView
        };
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                content,
                style,
                NSBackingStoreType::NSBackingStoreBuffered,
                false,
            )
        };
        // `MetalWindow` owns a retained NSWindow. AppKit's legacy release-on-close policy would release
        // the same object when an xdg_popup is dismissed, then objc2 would release the retained handle
        // again when `MetalWindow` drops (observed as EXC_BAD_ACCESS at autorelease-pool drain).
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str(title));
        // Wayland ARGB surfaces legitimately use transparent pixels around client-side decorations
        // (Chrome's window shadow is a common example).  Making the host window opaque or giving it a
        // white background turns those pixels into a conspicuous white rectangle.
        window.setOpaque(false);
        // GTK/Chrome client-side shadow pixels are removed by xdg_window_geometry before presentation,
        // leaving AppKit as the single shadow owner for both top-level windows and popup boxes.
        window.setHasShadow(true);
        // Surface windows are product content, not secret utility panels. Read-only sharing lets the
        // normal macOS screenshot/window-capture APIs see them while still preventing remote writers.
        // It also gives the real-Mac conformance tests an observable image of exactly what the user sees.
        window.setSharingType(NSWindowSharingType::NSWindowSharingReadOnly);
        window.setMovableByWindowBackground(false);
        window.setTitleVisibility(NSWindowTitleVisibility::NSWindowTitleHidden);
        window.setTitlebarAppearsTransparent(true);
        window.setAcceptsMouseMovedEvents(true);
        // The compositor is not a conventional document app with restorable window placement. Every
        // Wayland map creates fresh product state and must appear in the user's current Space, including
        // after a prior instance entered full screen or was minimized.
        let behavior = if popup {
            NSWindowCollectionBehavior::Transient | NSWindowCollectionBehavior::FullScreenAuxiliary
        } else {
            NSWindowCollectionBehavior::MoveToActiveSpace
                | NSWindowCollectionBehavior::FullScreenPrimary
        };
        unsafe { window.setCollectionBehavior(behavior) };
        for button in [
            NSWindowButton::NSWindowCloseButton,
            NSWindowButton::NSWindowMiniaturizeButton,
            NSWindowButton::NSWindowZoomButton,
        ] {
            if let Some(button) = window.standardWindowButton(button) {
                button.setHidden(true);
            }
        }
        unsafe { window.setBackgroundColor(Some(&NSColor::clearColor())) };

        let layer = unsafe { CAMetalLayer::new() };
        unsafe {
            layer.setDevice(Some(&ctx.device));
            layer.setPixelFormat(objc2_metal::MTLPixelFormat::BGRA8Unorm);
            layer.setFramebufferOnly(false);
            layer.setOpaque(false);
            layer.setContentsScale(scale);
            layer.setDrawableSize(NSSize::new(w as f64 * scale, h as f64 * scale));
            // Never stretch a stale guest drawable while an xdg configure is in flight. Anchor it at
            // the native content area's top-left: preserving the title/header bar is more important
            // than splitting a transient size mismatch across both edges, which could clip the bar.
            layer.setContentsGravity(kCAGravityTopLeft);
            // Keep the host clip and WindowServer shadow contour aligned. CALayer geometry is expressed
            // in points even when its drawable is Retina-sized, so this radius must not be multiplied by
            // backing scale. Popups use the slightly tighter macOS menu radius.
            if !popup {
                layer.setCornerRadius(10.0);
                layer.setMasksToBounds(true);
            }
        }
        // Host the layer in the window's content view.
        // A content view is expressed in the window's local coordinates, not the screen coordinates used
        // by the NSWindow constructor. It must also follow the content bounds during every native resize;
        // otherwise AppKit resizes only the outer window while `logical_size` keeps reporting the stale
        // initial view size, so no xdg configure ever reaches the guest.
        let view_frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
        let view = unsafe { NSView::initWithFrame(mtm.alloc(), view_frame) };
        unsafe {
            view.setAutoresizingMask(
                NSAutoresizingMaskOptions::NSViewWidthSizable
                    | NSAutoresizingMaskOptions::NSViewHeightSizable,
            );
            view.setWantsLayer(true);
            view.setLayer(Some(&layer));
        }
        window.setContentView(Some(&view));
        if !popup {
            window.makeKeyAndOrderFront(None);
            // Activating before any windows exist does not move a newly-created window out of a Space
            // retained by an earlier full-screen/minimized instance. Activate once the key window is
            // real so a fresh Wayland map behaves like a freshly launched native application.
            #[allow(deprecated)]
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        }

        MetalWindow {
            window,
            layer,
            scale,
            size: Cell::new((w, h)),
            fullscreen: Cell::new(false),
            maximized: Cell::new(false),
            floating_frame: Cell::new(None),
        }
    }

    pub fn add_child(&self, child: &MetalWindow) {
        child.detach_parent();
        unsafe {
            self.window
                .addChildWindow_ordered(&child.window, NSWindowOrderingMode::NSWindowAbove)
        };
    }

    pub fn detach_parent(&self) {
        if let Some(parent) = unsafe { self.window.parentWindow() } {
            unsafe { parent.removeChildWindow(&self.window) };
        }
    }

    pub fn drag(&self, event: &NSEvent) {
        self.window.performWindowDragWithEvent(event);
    }

    /// Begin a non-blocking edge resize when `point` lies in the native resize rim.
    pub fn begin_resize(&self, point: NSPoint) -> Option<ResizeDrag> {
        if self.fullscreen.get() {
            return None;
        }
        let size = self.window.frame().size;
        let rim = 7.0;
        let left = point.x <= rim;
        let right = point.x >= size.width - rim;
        let bottom = point.y <= rim;
        let top = point.y >= size.height - rim;
        if !(left || right || bottom || top) {
            return None;
        }
        Some(ResizeDrag {
            start_mouse: unsafe { NSEvent::mouseLocation() },
            start_frame: self.window.frame(),
            left,
            right,
            bottom,
            top,
        })
    }

    /// Apply the newest global pointer position directly to the host frame. This returns immediately;
    /// the presenter's next poll observes the new size and sends GTK a coalesced XDG configure.
    pub fn update_resize(&self, drag: &ResizeDrag) {
        let mouse = unsafe { NSEvent::mouseLocation() };
        let dx = mouse.x - drag.start_mouse.x;
        let dy = mouse.y - drag.start_mouse.y;
        let mut frame = drag.start_frame;
        if drag.left {
            frame.origin.x += dx;
            frame.size.width -= dx;
        } else if drag.right {
            frame.size.width += dx;
        }
        if drag.bottom {
            frame.origin.y += dy;
            frame.size.height -= dy;
        } else if drag.top {
            frame.size.height += dy;
        }

        let (min, max) = unsafe { (self.window.contentMinSize(), self.window.contentMaxSize()) };
        let width = frame.size.width.clamp(min.width.max(1.0), max.width);
        let height = frame.size.height.clamp(min.height.max(1.0), max.height);
        if drag.left {
            frame.origin.x += frame.size.width - width;
        }
        if drag.bottom {
            frame.origin.y += frame.size.height - height;
        }
        frame.size.width = width;
        frame.size.height = height;
        self.window.setFrame_display(frame, true);
    }

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

    /// Device-pixel size of the drawable (`size * scale`).
    #[allow(dead_code)] // helper for reading back a windowed frame at device resolution
    pub fn pixel_size(&self) -> (u32, u32) {
        (
            (self.size.get().0 as f64 * self.scale).round() as u32,
            (self.size.get().1 as f64 * self.scale).round() as u32,
        )
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
        unsafe {
            self.layer.setDrawableSize(NSSize::new(w as f64, h as f64));
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

    /// Blit a composited texture into the layer's next drawable and present it. Returns `true` if a
    /// drawable was vended and the flip scheduled, `false` if none was available (window not yet on
    /// screen) — in which case the caller keeps the frame for a later refresh.
    pub fn present(&self, ctx: &MetalCtx, composite: &ProtocolObject<dyn MTLTexture>) -> bool {
        if !self.is_visible() {
            return false;
        }
        let Some(drawable) = (unsafe { self.layer.nextDrawable() }) else {
            return false;
        };
        let dst = unsafe { drawable.texture() };
        let cmd = ctx.queue.commandBuffer().expect("commandBuffer");
        let blit = cmd.blitCommandEncoder().expect("blit");
        unsafe { blit.copyFromTexture_toTexture(composite, &dst) };
        blit.endEncoding();
        cmd.presentDrawable(ProtocolObject::from_ref(&*drawable));
        cmd.commit();
        true
    }
}
