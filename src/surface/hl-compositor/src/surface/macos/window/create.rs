use super::*;
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
        // `MetalWindow` owns a retained NSWindow. AppKit's release-on-close policy would release
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
            layer::configure(&layer);
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

        // State what WindowServer made of this window, once, at birth.
        //
        // `windowNumber` is the decisive field: WindowServer assigns it, so a window it never accepted
        // reports 0 and can never appear on screen however healthy AppKit looks from inside the process.
        // A popup is the interesting case — it is deliberately NOT ordered front here, so it reaches the
        // screen only via `add_child`, and this line is what distinguishes "never accepted" from
        // "accepted but never parented".
        let frame = window.frame();
        hl_log::hl_log!(
            hl_log::tag::PRESENT,
            hl_log::Level::Error,
            "native window created kind={} number={} visible={} on_screen={} frame={},{} {}x{}",
            if popup { "popup" } else { "toplevel" },
            unsafe { window.windowNumber() },
            window.isVisible(),
            window.screen().is_some(),
            frame.origin.x,
            frame.origin.y,
            frame.size.width,
            frame.size.height
        );

        MetalWindow {
            window,
            layer,
            size: Cell::new((w, h)),
            fullscreen: Cell::new(false),
            maximized: Cell::new(false),
            floating_frame: Cell::new(None),
            last_presented_ns: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
}
