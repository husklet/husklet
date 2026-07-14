//! The Cocoa window side: one `NSWindow` + `CAMetalLayer` per surface. Ported (trimmed) from
//! `hl-display::present_cocoa` — enough to open a real window, size its layer in device pixels for a
//! retina-crisp present, and blit a composited `MTLTexture` into the layer's next drawable.
//!
//! Everything here needs the AppKit main thread (`MainThreadMarker`) + a GUI login session to become
//! visible. The presenter falls back to a headless offscreen target when no window is available.

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSScreen, NSView,
    NSWindow, NSWindowOcclusionState, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLTexture,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use super::metal::MetalCtx;

/// Ensure there is a running `NSApplication` with a foreground (Regular) activation policy, so a window
/// this presenter opens can actually become visible and key. Idempotent. Returns the main-thread marker.
pub fn ensure_app(mtm: MainThreadMarker) -> Retained<NSApplication> {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    app
}

/// A native window backing one surface: an `NSWindow` whose content view hosts a `CAMetalLayer`.
pub struct MetalWindow {
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
    /// Device pixels per point (`backingScaleFactor`); the layer drawable is sized `size * scale`.
    scale: f64,
    /// Logical surface size in points (window content size).
    size: (u32, u32),
}

impl MetalWindow {
    /// Open a titled `NSWindow` of logical size `w`×`h` points, install a `CAMetalLayer` sized in device
    /// pixels, and order it front. Must run on the AppKit main thread.
    pub fn new(mtm: MainThreadMarker, ctx: &MetalCtx, w: u32, h: u32, title: &str) -> MetalWindow {
        let scale = NSScreen::mainScreen(mtm)
            .map(|s| s.backingScaleFactor())
            .unwrap_or(1.0)
            .max(1.0);
        let content = NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(w as f64, h as f64));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::Miniaturizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                content,
                style,
                NSBackingStoreType::NSBackingStoreBuffered,
                false,
            )
        };
        window.setTitle(&NSString::from_str(title));
        window.setOpaque(true);
        unsafe { window.setBackgroundColor(Some(&NSColor::whiteColor())) };

        let layer = unsafe { CAMetalLayer::new() };
        unsafe {
            layer.setDevice(Some(&ctx.device));
            layer.setPixelFormat(objc2_metal::MTLPixelFormat::BGRA8Unorm);
            layer.setFramebufferOnly(false);
            layer.setContentsScale(scale);
            layer.setDrawableSize(NSSize::new(w as f64 * scale, h as f64 * scale));
        }
        // Host the layer in the window's content view.
        let view = unsafe { NSView::initWithFrame(mtm.alloc(), content) };
        unsafe {
            view.setWantsLayer(true);
            view.setLayer(Some(&layer));
        }
        window.setContentView(Some(&view));
        window.makeKeyAndOrderFront(None);

        MetalWindow {
            window,
            layer,
            scale,
            size: (w, h),
        }
    }

    /// Device-pixel size of the drawable (`size * scale`).
    pub fn pixel_size(&self) -> (u32, u32) {
        (
            (self.size.0 as f64 * self.scale).round() as u32,
            (self.size.1 as f64 * self.scale).round() as u32,
        )
    }

    /// Size the layer's drawable to `w`×`h` DEVICE pixels so a `copyFromTexture_toTexture` blit of a
    /// composite target of that exact size into the drawable matches dimensions. Called before present.
    pub fn set_drawable_size(&self, w: u32, h: u32) {
        unsafe {
            self.layer
                .setDrawableSize(NSSize::new(w as f64, h as f64));
        }
    }

    /// Whether the window is currently on screen (its `occlusionState` reports Visible). `nextDrawable`
    /// on a non-visible layer can block, so the presenter only vends a drawable when this is true.
    pub fn is_visible(&self) -> bool {
        self.window
            .occlusionState()
            .contains(NSWindowOcclusionState::Visible)
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
