//! Native macOS presenter: one `NSWindow` per guest surface, showing the committed `wl_shm` buffer.
//!
//! MVP path (milestone M1, "first pixels"): build an `NSBitmapImageRep` (self-owned buffer), copy the
//! surface's tight RGBA into it, wrap it in an `NSImage`, and set it on an `NSImageView` that fills the
//! window's content view. This is the `CALayer.contents`-class blit the plan calls for, without needing
//! CoreGraphics or Metal yet (the `MTLBuffer(bytesNoCopy)` zero-copy path is M1's follow-on / M4). All
//! AppKit calls run on the main thread, as AppKit/CoreAnimation require.
//!
//! Compiled only on macOS. The portable rest of `dd-display` (wire + shm + framebuffer) is what the Linux
//! headless self-test exercises; this file is the piece that needs the Mac (and, to *see* the window, the
//! user's eyes — the bridge cannot screen-record).

#![cfg(target_os = "macos")]

use crate::present::{Presenter, SurfaceBuffer};
use crate::server::Server;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::ClassType;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSBitmapImageFileType,
    NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSDeviceRGBColorSpace, NSEvent, NSEventMask,
    NSEventModifierFlags, NSEventType, NSGraphicsContext, NSImage, NSImageView, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSDefaultRunLoopMode, NSDictionary, NSInteger, NSPoint, NSRect, NSSize,
    NSString, NSTimeInterval,
};
use crate::metal::MetalCtx;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLBlitCommandEncoder, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLPixelFormat,
    MTLTexture,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use std::collections::HashMap;
use std::os::raw::{c_float, c_ushort};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

/// A live window bound to a guest surface.
struct Win {
    #[allow(dead_code)]
    window: Retained<NSWindow>,
    image_view: Retained<NSImageView>,
    size: (i32, i32),
}

/// Presenter that owns the NSWindows. Lives on the main thread with `NSApp`.
pub struct CocoaPresenter {
    mtm: MainThreadMarker,
    wins: HashMap<u32, Win>,
}

impl CocoaPresenter {
    fn new(mtm: MainThreadMarker) -> CocoaPresenter {
        CocoaPresenter { mtm, wins: HashMap::new() }
    }

    /// Render the ACTUAL on-screen NSView for `sid` (the content view AppKit draws) into a PNG. This
    /// proves the presenter's on-screen path renders — not just the compositor's framebuffer. Uses
    /// `cacheDisplayInRect:` (the same synchronous view-drawing AppKit uses for the window), so it works
    /// against the live backing store whether or not a human is looking. Returns true on success.
    pub fn dump_view_png(&self, sid: u32, out: &str) -> bool {
        let Some(win) = self.wins.get(&sid) else { return false };
        let view = &win.image_view;
        unsafe {
            let bounds = view.bounds();
            // `cacheDisplayInRect:` draws the view synchronously into the rep against the live backing
            // store — no need to mark dirty or run the display loop first.
            let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(bounds) else { return false };
            view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
            let empty = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
            let Some(data) = rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &empty)
            else {
                return false;
            };
            std::fs::write(out, data.bytes()).is_ok()
        }
    }

    fn make_window(mtm: MainThreadMarker, sid: u32, w: i32, h: i32, title: &str) -> Win {
        let content = NSRect::new(
            NSPoint::new(120.0 + sid as f64 * 24.0, 120.0),
            NSSize::new(w as f64, h as f64),
        );
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
        let t = if title.is_empty() { format!("dd surface {sid}") } else { title.to_string() };
        window.setTitle(&NSString::from_str(&t));

        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
        let image_view = unsafe { NSImageView::initWithFrame(mtm.alloc(), frame) };
        window.setContentView(Some(&image_view));
        window.makeKeyAndOrderFront(None);
        Win { window, image_view, size: (w, h) }
    }
}

impl Presenter for CocoaPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) {
        let rgba = surf.to_rgba();
        let (w, h) = (surf.width, surf.height);

        // Self-owned bitmap: pass a null plane so NSBitmapImageRep allocates its own buffer, then copy the
        // surface bytes in. This avoids aliasing `rgba` (which drops at end of scope).
        let rep: Retained<NSBitmapImageRep> = unsafe {
            let mut plane: *mut u8 = std::ptr::null_mut();
            let rep = NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                &mut plane,
                w as isize,
                h as isize,
                8,       // bits per sample
                4,       // samples per pixel (RGBA)
                true,    // has alpha
                false,   // not planar
                NSDeviceRGBColorSpace,
                (w * 4) as isize, // bytes per row
                32,               // bits per pixel
            )
            .expect("NSBitmapImageRep init failed");
            let dst = rep.bitmapData();
            if !dst.is_null() {
                std::ptr::copy_nonoverlapping(rgba.as_ptr(), dst, rgba.len());
            }
            rep
        };

        let image = unsafe {
            NSImage::initWithSize(NSImage::alloc(), NSSize::new(w as f64, h as f64))
        };
        unsafe { image.addRepresentation(&rep) };

        let mtm = self.mtm;
        let needs = self.wins.get(&surf.sid).map(|win| win.size) != Some((w, h));
        let sid = surf.sid;
        let title = surf.title.clone();
        let win = self
            .wins
            .entry(sid)
            .or_insert_with(|| CocoaPresenter::make_window(mtm, sid, w, h, &title));
        if needs {
            unsafe {
                win.image_view
                    .setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64)));
            }
            win.size = (w, h);
        }
        unsafe { win.image_view.setImage(Some(&image)) };
    }

    fn surface_size(&self, sid: u32) -> Option<(i32, i32)> {
        self.wins.get(&sid).map(|w| w.size)
    }

    fn dump_pngs(&self, dir: &str) -> usize {
        let _ = std::fs::create_dir_all(dir);
        let mut n = 0;
        for sid in self.wins.keys() {
            if self.dump_view_png(*sid, &format!("{dir}/live-surface-{sid}.png")) {
                n += 1;
            }
        }
        n
    }

    fn window_ptr_to_sid(&self, win_ptr: *const std::ffi::c_void) -> Option<u32> {
        self.wins.iter().find_map(|(sid, w)| {
            (Retained::as_ptr(&w.window) as *const std::ffi::c_void == win_ptr).then_some(*sid)
        })
    }

    fn window_content_size(&self, sid: u32) -> Option<(i32, i32)> {
        let w = self.wins.get(&sid)?;
        let view = w.window.contentView()?;
        let b = view.bounds();
        Some((b.size.width as i32, b.size.height as i32))
    }
}

// ---- Hardware-accelerated presenter: one NSWindow + CAMetalLayer per surface ----

struct MetalWin {
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
    size: (u32, u32),
    /// The most recently composited source texture (shm upload or IOSurface wrap). Kept so `SIGUSR1` can
    /// read it back to a PNG — the on-screen `CAMetalLayer` drawable itself isn't readable after present.
    last_tex: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
}

/// Presents each committed `wl_shm` buffer via Metal: upload → GPU blit into the `CAMetalLayer`'s
/// drawable → present. This is the accelerated replacement for the `NSImageView` copy-blit. The shared
/// [`MetalCtx`] (device + queue) is the same one `dd-gpu`'s executor targets.
pub struct MetalPresenter {
    mtm: MainThreadMarker,
    ctx: MetalCtx,
    wins: HashMap<u32, MetalWin>,
    frames: u32,
    /// `DD_DISPLAY_DUMP_EVERY`: when set to N>0, read back + PNG-dump every Nth composited frame to
    /// `DD_DISPLAY_DUMP` — a headless way to capture what a short-lived app actually put on screen
    /// (the window itself is torn down when the client exits, faster than a human/SIGUSR1 can look).
    dump_every: u32,
    dump_dir: String,
}

impl MetalPresenter {
    pub fn new(mtm: MainThreadMarker) -> Option<MetalPresenter> {
        let dump_every = std::env::var("DD_DISPLAY_DUMP_EVERY").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
        let dump_dir = std::env::var("DD_DISPLAY_DUMP").unwrap_or_else(|_| "/tmp/dd-display-live".into());
        Some(MetalPresenter { mtm, ctx: MetalCtx::new()?, wins: HashMap::new(), frames: 0, dump_every, dump_dir })
    }

    fn make_window(mtm: MainThreadMarker, ctx: &MetalCtx, sid: u32, w: u32, h: u32, title: &str) -> MetalWin {
        let content = NSRect::new(
            NSPoint::new(140.0 + sid as f64 * 24.0, 140.0),
            NSSize::new(w as f64, h as f64),
        );
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable | NSWindowStyleMask::Miniaturizable;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(), content, style, NSBackingStoreType::NSBackingStoreBuffered, false,
            )
        };
        let t = if title.is_empty() { format!("dd surface {sid} (metal)") } else { title.to_string() };
        window.setTitle(&NSString::from_str(&t));

        let layer = unsafe { CAMetalLayer::new() };
        unsafe {
            layer.setDevice(Some(&ctx.device));
            layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            layer.setFramebufferOnly(false);
            layer.setDrawableSize(NSSize::new(w as f64, h as f64));
        }
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
        let view = unsafe { NSView::initWithFrame(mtm.alloc(), frame) };
        unsafe {
            view.setWantsLayer(true);
            view.setLayer(Some(&layer));
        }
        window.setContentView(Some(&view));
        window.makeKeyAndOrderFront(None);
        // Force the new window in front of every space/app and re-assert app foreground, so Core Animation
        // composites this CAMetalLayer at the display rate rather than background-throttling its present
        // (which would pace the frame-callback-driven guest down to ~1 fps). See run_window() for why.
        unsafe { window.orderFrontRegardless() };
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        MetalWin { window, layer, size: (w, h), last_tex: None }
    }
}

impl Presenter for MetalPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) {
        let (w, h) = (surf.width as u32, surf.height as u32);
        // GPU rung 2: if the buffer is an IOSurface (dmabuf), wrap it directly (ZERO copy/upload);
        // otherwise upload the shm bytes. `CFRelease` the looked-up surface after wrapping.
        let src = match surf.iosurface_id {
            Some(id) => {
                let surface = unsafe { crate::metal::resolve_iosurface(id) };
                if surface.is_null() {
                    eprintln!("dd-display[metal]: IOSurface id {id} not found; skipping frame");
                    return;
                }
                let tex = self.ctx.texture_from_iosurface(surface, w, h);
                if surf.gpu_render {
                    self.ctx.render_triangle_into(&tex); // rung 3: host GPU renders into the guest IOSurface
                }
                unsafe { crate::metal::cfrelease(surface) };
                tex
            }
            None => self.ctx.upload_bgra(&surf.bgra, w, h),
        };

        let mtm = self.mtm;
        let title = surf.title.clone();
        let sid = surf.sid;
        let ctx = &self.ctx;
        let win = self.wins.entry(sid).or_insert_with(|| MetalPresenter::make_window(mtm, ctx, sid, w, h, &title));
        if win.size != (w, h) {
            unsafe { win.layer.setDrawableSize(NSSize::new(w as f64, h as f64)) };
            win.size = (w, h);
        }
        let Some(drawable) = (unsafe { win.layer.nextDrawable() }) else { return };
        let dst = unsafe { drawable.texture() };
        // Blit + present in one command buffer. L4: if the source is a guest IOSurface the executor renders
        // into asynchronously, guard the blit with the cross-queue tearing fence (wait for render-complete,
        // signal blit-complete) so a partly-rendered surface is never presented.
        let fence = match surf.iosurface_id {
            Some(id) if crate::metal::async_on() => crate::metal::fence_begin_present(id),
            _ => None,
        };
        let cmd = self.ctx.queue.commandBuffer().expect("commandBuffer");
        if let Some((render_ev, _p, gen)) = &fence {
            cmd.encodeWaitForEvent_value(render_ev, *gen);
        }
        let enc = cmd.blitCommandEncoder().expect("blit");
        unsafe { enc.copyFromTexture_toTexture(&src, &dst) };
        enc.endEncoding();
        if let Some((_r, present_ev, gen)) = &fence {
            cmd.encodeSignalEvent_value(present_ev, *gen);
        }
        cmd.presentDrawable(objc2::runtime::ProtocolObject::from_ref(&*drawable));
        cmd.commit();
        win.last_tex = Some(src); // keep for SIGUSR1 readback

        // Headless capture of the LIVE composited frame (opt-in), so a short-lived app's on-screen pixels
        // can be Read back even after its window is gone.
        self.frames += 1;
        if self.dump_every > 0 && self.frames % self.dump_every == 0 {
            if let Some(w) = self.wins.get(&sid) {
                if let Some(tex) = w.last_tex.as_ref() {
                    let bgra = self.ctx.readback_bgra(tex, w.size.0, w.size.1);
                    let mut rgba = vec![0u8; bgra.len()];
                    for i in (0..bgra.len()).step_by(4) {
                        rgba[i] = bgra[i + 2];
                        rgba[i + 1] = bgra[i + 1];
                        rgba[i + 2] = bgra[i];
                        rgba[i + 3] = 0xff;
                    }
                    let png = dd_term_core::png::encode_rgba(w.size.0, w.size.1, &rgba);
                    let _ = std::fs::create_dir_all(&self.dump_dir);
                    let path = format!("{}/live-surface-{sid}-{:04}.png", self.dump_dir, self.frames);
                    if std::fs::write(&path, png).is_ok() {
                        eprintln!("dd-display[metal]: live frame {} dumped -> {path}", self.frames);
                    }
                }
            }
        }
    }

    fn frame_count(&self) -> u32 {
        self.frames
    }

    fn surface_size(&self, sid: u32) -> Option<(i32, i32)> {
        self.wins.get(&sid).map(|w| (w.size.0 as i32, w.size.1 as i32))
    }

    fn dump_pngs(&self, dir: &str) -> usize {
        let _ = std::fs::create_dir_all(dir);
        let mut n = 0;
        for (sid, w) in self.wins.iter() {
            let Some(tex) = w.last_tex.as_ref() else { continue };
            let (pw, ph) = w.size;
            let bgra = self.ctx.readback_bgra(tex, pw, ph);
            // BGRA → RGBA (opaque) for the PNG encoder.
            let mut rgba = vec![0u8; bgra.len()];
            for i in (0..bgra.len()).step_by(4) {
                rgba[i] = bgra[i + 2];
                rgba[i + 1] = bgra[i + 1];
                rgba[i + 2] = bgra[i];
                rgba[i + 3] = 0xff;
            }
            let png = dd_term_core::png::encode_rgba(pw, ph, &rgba);
            if std::fs::write(format!("{dir}/live-surface-{sid}.png"), png).is_ok() {
                n += 1;
            }
        }
        n
    }

    fn window_ptr_to_sid(&self, win_ptr: *const std::ffi::c_void) -> Option<u32> {
        self.wins.iter().find_map(|(sid, w)| {
            (Retained::as_ptr(&w.window) as *const std::ffi::c_void == win_ptr).then_some(*sid)
        })
    }

    fn window_content_size(&self, sid: u32) -> Option<(i32, i32)> {
        let w = self.wins.get(&sid)?;
        let view = w.window.contentView()?;
        let b = view.bounds();
        Some((b.size.width as i32, b.size.height as i32))
    }
}

/// Headless-ish proof that the ACTUAL on-screen presenter renders: build a real NSApp + CocoaPresenter,
/// drive one frame from a forked Wayland client over a real socket, then dump the live NSView to a PNG
/// (`cacheDisplayInRect:`). Writes `out` and exits. Runs on macOS with no human looking — this shrinks
/// "needs your eyes" for M1 to essentially zero (it renders the same view AppKit would show on screen).
pub fn selftest_cocoa(out: &str) -> ! {
    let mtm = MainThreadMarker::new().expect("dd-display must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    unsafe { app.finishLaunching() };

    let sock = format!("/tmp/dd-display-cocoa-{}.sock", unsafe { libc::getpid() });
    let lfd = crate::listen_unix(&sock).expect("bind selftest socket");
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        crate::selftest::client(&sock);
        unsafe { libc::_exit(0) };
    }
    let cfd = loop {
        let fd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd >= 0 {
            break fd;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            eprintln!("selftest-cocoa: accept failed");
            std::process::exit(1);
        }
    };
    unsafe {
        let fl = libc::fcntl(cfd, libc::F_GETFL);
        libc::fcntl(cfd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    let mut server = Server::new(cfd, CocoaPresenter::new(mtm));

    // Pump the client frame + let AppKit process a few run-loop turns so the view lays out.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while server.presenter().wins.is_empty() && std::time::Instant::now() < deadline {
        let mut pfd = libc::pollfd { fd: cfd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 50) };
        let _ = server.pump();
        unsafe {
            while let Some(ev) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                None,
                NSDefaultRunLoopMode,
                true,
            ) {
                app.sendEvent(&ev);
            }
        }
    }

    let ok = server.presenter().dump_view_png(6, out);
    unsafe {
        libc::waitpid(pid, std::ptr::null_mut(), 0);
        libc::close(cfd);
        libc::close(lfd);
    }
    let _ = std::fs::remove_file(&sock);
    if ok {
        println!("selftest-cocoa: rendered the live NSView -> {out}");
        std::process::exit(0);
    } else {
        eprintln!("selftest-cocoa: FAILED to render/dump the view");
        std::process::exit(1);
    }
}

/// Run the Cocoa event loop, accepting one guest client and driving its surfaces into NSWindows.
///
/// For M1 this drives a single client on the main thread: we poll the client socket with a short timeout
/// inside the AppKit run loop so both the Wayland dispatch and NSWindow events progress. Multi-client + a
/// proper `CFRunLoopSource` marrying the two loops is M2 (see RENDERING_PLAN.md §4). `metal` selects the
/// hardware-accelerated `CAMetalLayer` present path over the `NSImageView` copy-blit.
pub fn run(lfd: RawFd, socket: String, metal: bool) -> ! {
    let mtm = MainThreadMarker::new().expect("dd-display must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    eprintln!("dd-display[cocoa]: waiting for a client on {socket} (metal={metal})");
    let cfd = loop {
        let fd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd >= 0 {
            break fd;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            eprintln!("dd-display[cocoa]: accept failed");
            std::process::exit(1);
        }
    };
    unsafe {
        let fl = libc::fcntl(cfd, libc::F_GETFL);
        libc::fcntl(cfd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    eprintln!("dd-display[cocoa]: client connected");
    unsafe { app.finishLaunching() };

    // Metal path if requested and a device exists; else the NSImageView copy-blit.
    if metal {
        crate::metal::start_gpu_bridge(); // GPU rung 2: receive guest IOSurface handles over mach
        if let Some(mp) = MetalPresenter::new(mtm) {
            return drive(app, cfd, Server::new(cfd, mp));
        }
        eprintln!("dd-display[cocoa]: no Metal device; falling back to NSImageView");
    }
    drive(app, cfd, Server::new(cfd, CocoaPresenter::new(mtm)))
}

/// Set by the `SIGUSR1` handler; the live loop dumps every window to `dump_dir()` when it sees this. A
/// headless driver (see `target-mac/live-window.sh`) sends `SIGUSR1` and Reads the PNG back, since the Mac
/// screen cannot be recorded.
static DUMP_REQ: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigusr1(_sig: i32) {
    DUMP_REQ.store(true, Ordering::SeqCst);
}

/// Where `SIGUSR1` dumps land: `DD_DISPLAY_DUMP` if set, else `/tmp/dd-display-live`.
fn dump_dir() -> String {
    std::env::var("DD_DISPLAY_DUMP").unwrap_or_else(|_| "/tmp/dd-display-live".into())
}

fn install_dump_handler() {
    unsafe { libc::signal(libc::SIGUSR1, on_sigusr1 as usize) };
}

/// If a dump was requested (SIGUSR1), write every window's current pixels to `dump_dir()`.
fn service_dump<P: Presenter>(servers: &mut [Server<P>]) {
    if !DUMP_REQ.swap(false, Ordering::SeqCst) {
        return;
    }
    let dir = dump_dir();
    let mut total = 0;
    for s in servers.iter_mut() {
        total += s.presenter_mut().dump_pngs(&dir);
    }
    eprintln!("dd-display[cocoa]: SIGUSR1 dumped {total} live window(s) -> {dir}/live-surface-*.png");
}

/// The shared main-thread event loop: pump the Wayland client + drain AppKit events (routing input into
/// the seat), forever. Single-client path (the proven default when neither `--png` nor `--window` picks
/// the multiplexed live loop).
fn drive<P: Presenter>(app: Retained<NSApplication>, cfd: RawFd, mut server: Server<P>) -> ! {
    install_dump_handler();
    loop {
        let mut pfd = libc::pollfd { fd: cfd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 8) };
        match server.pump() {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                eprintln!("dd-display[cocoa]: client gone");
                std::process::exit(0);
            }
        }
        service_dump(std::slice::from_mut(&mut server));
        // The focused surface's height flips Cocoa's bottom-left pointer coords into top-left surface space.
        let flip_h = server
            .focused_surface()
            .and_then(|sid| server.presenter().surface_size(sid))
            .map(|(_, h)| h);
        // Drain queued AppKit events: route input into the Wayland seat, then forward to AppKit so the
        // window chrome stays responsive.
        loop {
            let ev = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    None,
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            match ev {
                Some(ev) => {
                    inject_nsevent(&mut server, &ev, flip_h);
                    unsafe { app.sendEvent(&ev) };
                }
                None => break,
            }
        }
    }
}

/// Translate an `NSEvent` into `wl_seat` input on the compositor. Keyboard uses the `kVK_*`→evdev map
/// below; the client's xkbcommon (fed our keymap) turns the evdev code into a keysym. `flip_h` is the
/// focused surface's height: Cocoa's `locationInWindow` is bottom-left origin, so surface_y = h - y.
fn inject_nsevent<P: Presenter>(server: &mut Server<P>, ev: &NSEvent, flip_h: Option<i32>) {
    let ty = unsafe { ev.r#type() };
    if ty == NSEventType::MouseMoved || ty == NSEventType::LeftMouseDragged || ty == NSEventType::RightMouseDragged {
        let p = unsafe { ev.locationInWindow() };
        let (x, y) = flip_point(p, flip_h);
        server.pointer_motion(x, y);
    } else if ty == NSEventType::LeftMouseDown {
        server.pointer_button(0x110, true);
    } else if ty == NSEventType::LeftMouseUp {
        server.pointer_button(0x110, false);
    } else if ty == NSEventType::RightMouseDown {
        server.pointer_button(0x111, true);
    } else if ty == NSEventType::RightMouseUp {
        server.pointer_button(0x111, false);
    } else if ty == NSEventType::ScrollWheel {
        let dy = unsafe { ev.scrollingDeltaY() };
        let dx = unsafe { ev.scrollingDeltaX() };
        if dy != 0.0 {
            server.pointer_axis(0, -(dy as i32)); // vertical
        }
        if dx != 0.0 {
            server.pointer_axis(1, -(dx as i32)); // horizontal
        }
    } else if ty == NSEventType::KeyDown {
        if let Some(code) = kvk_to_evdev(unsafe { ev.keyCode() }) {
            server.key(code, true);
        }
    } else if ty == NSEventType::KeyUp {
        if let Some(code) = kvk_to_evdev(unsafe { ev.keyCode() }) {
            server.key(code, false);
        }
    } else if ty == NSEventType::FlagsChanged {
        let f = unsafe { ev.modifierFlags() };
        // xkb masks: Shift=1, Lock=2, Control=4, Mod1(Alt)=8, Mod4(Super)=64.
        let mut dep = 0u32;
        if f.contains(NSEventModifierFlags::NSEventModifierFlagShift) { dep |= 1; }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagCapsLock) { dep |= 2; }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagControl) { dep |= 4; }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagOption) { dep |= 8; }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagCommand) { dep |= 64; }
        server.modifiers(dep, 0, 0, 0);
    }
}

/// macOS virtual keycode (`kVK_*`) → Linux evdev `KEY_*`. Covers the alphanumerics + common keys; unmapped
/// keys are dropped. (Ported subset — the full XQuartz/SDL table is a follow-up.)
fn kvk_to_evdev(kvk: u16) -> Option<u32> {
    Some(match kvk {
        0 => 30, 1 => 31, 2 => 32, 3 => 33, 4 => 35, 5 => 34, 6 => 44, 7 => 45, 8 => 46, 9 => 47,
        11 => 48, 12 => 16, 13 => 17, 14 => 18, 15 => 19, 16 => 21, 17 => 20, 31 => 24, 32 => 22,
        34 => 23, 35 => 25, 37 => 38, 38 => 36, 40 => 37, 45 => 49, 46 => 50,
        18 => 2, 19 => 3, 20 => 4, 21 => 5, 22 => 7, 23 => 6, 25 => 10, 26 => 8, 28 => 9, 29 => 11,
        36 => 28,  // Return → KEY_ENTER
        48 => 15,  // Tab
        49 => 57,  // Space
        51 => 14,  // Delete → KEY_BACKSPACE
        53 => 1,   // Escape
        123 => 105, 124 => 106, 125 => 108, 126 => 103, // arrows: Left/Right/Down/Up
        _ => return None,
    })
}

/// Cocoa `locationInWindow` (bottom-left origin) → surface-local top-left pixel coords. `flip_h` is the
/// focused surface height; without it we pass the raw y (last-resort).
fn flip_point(p: NSPoint, flip_h: Option<i32>) -> (i32, i32) {
    let x = (p.x as i32).max(0);
    let y = match flip_h {
        Some(h) if h > 0 => (h - p.y as i32).clamp(0, h - 1),
        _ => (p.y as i32).max(0),
    };
    (x, y)
}

fn set_nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

// ================= Live multi-client window loop (the `--window` mode) =================

/// The first-class LIVE `--window` mode: a real, focusable on-screen `NSWindow` per guest surface,
/// composited via Metal (`CAMetalLayer`, `metal=true`) — GPU-accelerated, not a CPU blit — and the Mac's
/// mouse/keyboard `NSEvent`s routed into each client's `wl_seat`. Unlike the single-client [`run`], this
/// services MANY concurrent clients (a real toolkit app keeps several connections open: e.g. a GL shim
/// commits the rendered IOSurface on a second connection), so glmark2/Chrome-class apps work.
pub fn run_window(lfd: RawFd, socket: String, metal: bool) -> ! {
    let mtm = MainThreadMarker::new().expect("dd-display must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    unsafe { app.finishLaunching() };
    // Bring dd-display to the FOREGROUND. macOS Core Animation aggressively throttles a background /
    // non-foreground app's `CAMetalLayer` present (drawables are vended slowly, present coalesces) — which,
    // because the guest is paced ~1 frame ahead by the wl frame callback (present N acks → render N+1),
    // drags the whole guest→executor→present pipeline down to ~1-18 fps even though the raw pipeline
    // sustains thousands of fps offscreen. Activating the app makes its windows composite at the display
    // rate (60/120 Hz) so a real user's window renders smoothly instead of background-throttled.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    eprintln!("dd-display[window]: live NSWindow present, listening on {socket} (metal={metal})");

    if metal {
        crate::metal::start_gpu_bridge(); // GPU rung 2: receive guest IOSurface handles over mach
        // GPU rung 3: replay guest dd-gpu IR onto Metal into the resolved IOSurface.
        if let Some(p) = gpu_exec_sock(&socket) {
            std::thread::spawn(move || crate::metal_backend::run_executor(p));
        }
        if MetalPresenter::new(mtm).is_some() {
            return run_multi(app, lfd, "window-metal", move || MetalPresenter::new(mtm));
        }
        eprintln!("dd-display[window]: no Metal device; falling back to NSImageView copy-blit");
    }
    run_multi(app, lfd, "window", move || Some(CocoaPresenter::new(mtm)))
}

/// The dd-gpu IR executor socket beside the display socket (or `DD_GPU_EXEC_SOCK`).
fn gpu_exec_sock(disp: &str) -> Option<String> {
    if let Ok(p) = std::env::var("DD_GPU_EXEC_SOCK") {
        return Some(p);
    }
    let dir = std::path::Path::new(disp).parent()?;
    Some(dir.join("dd-gpu.sock").to_string_lossy().into_owned())
}

/// Accept + service MANY live clients on the main thread while draining AppKit: each iteration polls the
/// listen fd + every client fd (short timeout so AppKit stays responsive), pumps ready clients, then
/// drains queued `NSEvent`s — routing each to the client that owns the targeted window — and forwards them
/// to AppKit so window chrome (drag/resize/close) works.
fn run_multi<P: Presenter>(
    app: Retained<NSApplication>,
    lfd: RawFd,
    tag: &str,
    mut make: impl FnMut() -> Option<P>,
) -> ! {
    install_dump_handler();
    set_nonblock(lfd);
    let mut clients: Vec<Server<P>> = Vec::new();
    loop {
        let mut pfds: Vec<libc::pollfd> = Vec::with_capacity(clients.len() + 1);
        pfds.push(libc::pollfd { fd: lfd, events: libc::POLLIN, revents: 0 });
        for c in &clients {
            pfds.push(libc::pollfd { fd: c.raw_fd(), events: libc::POLLIN, revents: 0 });
        }
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, 8) };
        if n > 0 {
            let ready_fds: Vec<RawFd> = pfds[1..]
                .iter()
                .filter(|p| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
                .map(|p| p.fd)
                .collect();
            if pfds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                loop {
                    let cfd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
                    if cfd < 0 {
                        break;
                    }
                    set_nonblock(cfd);
                    match make() {
                        Some(p) => {
                            eprintln!("dd-display[{tag}]: client connected (fd {cfd}, {} live)", clients.len() + 1);
                            clients.push(Server::new(cfd, p));
                        }
                        None => {
                            eprintln!("dd-display[{tag}]: no presenter (no Metal device?)");
                            unsafe { libc::close(cfd) };
                        }
                    }
                }
            }
            for fd in ready_fds {
                let Some(idx) = clients.iter().position(|c| c.raw_fd() == fd) else { continue };
                if !matches!(clients[idx].pump(), Ok(true)) {
                    eprintln!("dd-display[{tag}]: client disconnected ({} frame(s))", clients[idx].presenter().frame_count());
                    // Release this client's per-IOSurface fences + cached surfaces BEFORE dropping the
                    // Server. Otherwise the departed compositor's stale fence (render_gen=N, present_ev only
                    // reached N-1) would deadlock a later client that reuses the same IOSurface id, and the
                    // MTLEvents/IOSurfaces would leak as clients churn.
                    for id in clients[idx].iosurface_ids() {
                        crate::metal::fence_drop(id);
                        crate::metal::gpu_surface_drop(id);
                    }
                    unsafe { libc::close(fd) };
                    clients.swap_remove(idx);
                }
            }
        }
        service_dump(&mut clients);
        // User window resize → xdg_toplevel.configure (debounced per client).
        for c in clients.iter_mut() {
            if let Some(sid) = c.focused_surface() {
                if let Some((w, h)) = c.presenter().window_content_size(sid) {
                    c.maybe_resize(w, h);
                }
            }
        }
        // Drain AppKit events; route input to the owning client, then let AppKit handle chrome.
        loop {
            let ev = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    None,
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            match ev {
                Some(ev) => {
                    route_input(&mut clients, &ev);
                    unsafe { app.sendEvent(&ev) };
                }
                None => break,
            }
        }
    }
}

/// Route an `NSEvent` to the client that owns the window it targeted (matched by `NSWindow*`). Falls back
/// to the sole client's focused surface for events whose window is nil (some key events) when unambiguous.
fn route_input<P: Presenter>(clients: &mut [Server<P>], ev: &NSEvent) {
    let mtm = MainThreadMarker::new().expect("route_input on main thread");
    let target = unsafe { ev.window(mtm) };
    if let Some(win) = target {
        let wp = Retained::as_ptr(&win) as *const std::ffi::c_void;
        for i in 0..clients.len() {
            if let Some(sid) = clients[i].presenter().window_ptr_to_sid(wp) {
                let flip_h = clients[i].presenter().surface_size(sid).map(|(_, h)| h);
                inject_nsevent(&mut clients[i], ev, flip_h);
                return;
            }
        }
    }
    if clients.len() == 1 {
        let flip_h = clients[0]
            .focused_surface()
            .and_then(|sid| clients[0].presenter().surface_size(sid))
            .map(|(_, h)| h);
        inject_nsevent(&mut clients[0], ev, flip_h);
    }
}

/// `selftest-input`: prove the FULL live input round-trip HEADLESSLY (no human, no hardware mouse). Opens a
/// real `NSWindow` for a forked guest client that binds `wl_seat`, SYNTHESIZES `NSEvent`s (pointer move,
/// button, key) into the same `inject_nsevent` the live loop uses, then verifies the guest logged the
/// delivered pointer/keyboard events. Also dumps the live view to PNGs. Exits 0 on PASS, 1 on FAIL.
pub fn selftest_input(out: &str) -> ! {
    let mtm = MainThreadMarker::new().expect("dd-display must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    unsafe { app.finishLaunching() };

    let sock = format!("/tmp/dd-display-input-{}.sock", unsafe { libc::getpid() });
    let results = format!("{out}.log");
    let _ = std::fs::remove_file(&results);
    let _ = std::fs::remove_file(&sock);
    let lfd = crate::listen_unix(&sock).expect("bind selftest-input socket");

    let (sock_c, res_c) = (sock.clone(), results.clone());
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        crate::selftest::input_client(&sock_c, &res_c, 4000);
        unsafe { libc::_exit(0) };
    }
    let cfd = loop {
        let fd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd >= 0 {
            break fd;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            eprintln!("selftest-input: accept failed");
            std::process::exit(1);
        }
    };
    set_nonblock(cfd);
    let mut server = Server::new(cfd, CocoaPresenter::new(mtm));

    // Pump until the client has mapped a window (⇒ pointer/keyboard/focus all established).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while server.presenter().surface_size(7).is_none() && std::time::Instant::now() < deadline {
        let mut pfd = libc::pollfd { fd: cfd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 30) };
        let _ = server.pump();
        drain_appkit(&app);
    }
    let mapped = server.presenter().surface_size(7);
    eprintln!("selftest-input: client window mapped = {mapped:?}");
    let flip_h = mapped.map(|(_, h)| h);

    // Synthesize NSEvents and feed them through the REAL inject_nsevent path.
    let none_ctx: Option<&NSGraphicsContext> = None;
    let empty = NSEventModifierFlags::empty();
    // Pointer move to surface (100,30): Cocoa y = h-30.
    let cy = flip_h.map(|h| (h - 30) as f64).unwrap_or(90.0);
    unsafe {
        if let Some(ev) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::MouseMoved, NSPoint::new(100.0, cy), empty, 0.0 as NSTimeInterval, 0 as NSInteger, none_ctx, 0 as NSInteger, 0 as NSInteger, 0.0 as c_float,
        ) { inject_nsevent(&mut server, &ev, flip_h); }
        pump_a_while(&mut server, &app, cfd, 6);

        if let Some(ev) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDown, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, 0 as NSInteger, 1 as NSInteger, 1.0 as c_float,
        ) { inject_nsevent(&mut server, &ev, flip_h); }
        pump_a_while(&mut server, &app, cfd, 6);

        if let Some(ev) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseUp, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, 0 as NSInteger, 1 as NSInteger, 0.0 as c_float,
        ) { inject_nsevent(&mut server, &ev, flip_h); }
        pump_a_while(&mut server, &app, cfd, 6);

        // Key 'a' (kVK_ANSI_A = 0) down + up.
        let a = NSString::from_str("a");
        if let Some(ev) = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, &a, &a, false, 0 as c_ushort,
        ) { inject_nsevent(&mut server, &ev, flip_h); }
        pump_a_while(&mut server, &app, cfd, 6);
        if let Some(ev) = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyUp, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, &a, &a, false, 0 as c_ushort,
        ) { inject_nsevent(&mut server, &ev, flip_h); }
        pump_a_while(&mut server, &app, cfd, 10);
    }

    // Dump the live view(s) so the driver can Read what's on screen.
    let dir = std::path::Path::new(out).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into());
    let dumped = server.presenter().dump_pngs(&dir);
    let _ = server.presenter().dump_view_png(7, out);

    unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0); libc::close(cfd); libc::close(lfd); }
    let _ = std::fs::remove_file(&sock);

    // Verify the guest client logged the delivered events.
    let log = std::fs::read_to_string(&results).unwrap_or_default();
    eprintln!("---- guest client input log ({results}) ----\n{log}----");
    let need = [
        ("pointer enter", "PTR_ENTER"),
        ("pointer motion", "PTR_MOTION"),
        ("pointer button press", "PTR_BUTTON button=272 state=1"),
        ("pointer button release", "PTR_BUTTON button=272 state=0"),
        ("keyboard enter", "KBD_ENTER"),
        ("key press (a→evdev 30)", "KBD_KEY key=30 state=1"),
        ("key release", "KBD_KEY key=30 state=0"),
    ];
    let mut ok = true;
    for (label, pat) in need {
        let hit = log.contains(pat);
        eprintln!("  [{}] {label}: {pat}", if hit { "PASS" } else { "MISS" });
        ok &= hit;
    }
    eprintln!("selftest-input: dumped {dumped} live window PNG(s) + view -> {out}");
    if ok {
        println!("selftest-input: PASS — NSEvent → wl_seat → guest round-trip verified");
        std::process::exit(0);
    } else {
        eprintln!("selftest-input: FAIL — not all synthesized events reached the guest");
        std::process::exit(1);
    }
}

/// Drain all currently-queued AppKit events without dispatching input (used by the self-test setup).
fn drain_appkit(app: &NSApplication) {
    unsafe {
        while let Some(ev) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any, None, NSDefaultRunLoopMode, true,
        ) {
            app.sendEvent(&ev);
        }
    }
}

/// Pump the client socket + AppKit for `iters` short turns so a just-injected event flushes to the guest
/// and any reply is read.
fn pump_a_while<P: Presenter>(server: &mut Server<P>, app: &NSApplication, cfd: RawFd, iters: u32) {
    for _ in 0..iters {
        let mut pfd = libc::pollfd { fd: cfd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 20) };
        let _ = server.pump();
        drain_appkit(app);
    }
}
