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

use crate::metal::MetalCtx;
use crate::present::{Presenter, SurfaceBuffer};
use crate::server::{ExternalLogicalCrop, Server};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSColor, NSCursor,
    NSDeviceRGBColorSpace, NSEvent, NSEventMask, NSEventModifierFlags, NSEventType, NSGraphicsContext,
    NSImage, NSImageView, NSScreen, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSDefaultRunLoopMode, NSDictionary, NSInteger, NSPoint, NSRect, NSSize,
    NSString, NSTimeInterval,
};
use objc2_metal::{
    MTLBlitCommandEncoder, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLDevice, MTLLibrary, MTLLoadAction, MTLPixelFormat, MTLPrimitiveType,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLStoreAction, MTLTexture,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use std::cell::RefCell;
use std::collections::HashMap;
use std::os::raw::{c_float, c_ushort};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

// ---- Native window-close → xdg_toplevel.close ---------------------------------------------------
// AppKit's title-bar close button (or Cmd-W) fires `windowShouldClose:` on the window's delegate. Without
// a delegate the window would just orderOut, silently dropping the guest client's toplevel while it keeps
// running (its Wayland surface stays "mapped" but the host window is gone). Instead we install a shared
// delegate that REFUSES the AppKit close (returns NO — the guest owns the surface lifecycle) and queues the
// window pointer; the live presenter loop translates each queued window to its owning surface and sends
// `xdg_toplevel.close`, so the client (Chrome, a GTK app, …) exits or prompts exactly as on real Wayland.

/// Window pointers (`*const NSWindow as usize`) whose native close button was clicked, awaiting
/// translation to `xdg_toplevel.close` by the presenter loop. Main-thread only in practice; a Mutex keeps
/// it trivially sound.
fn pending_window_closes() -> &'static Mutex<Vec<usize>> {
    static Q: OnceLock<Mutex<Vec<usize>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(Vec::new()))
}

declare_class!(
    struct WindowCloseDelegate;

    unsafe impl ClassType for WindowCloseDelegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "DDWindowCloseDelegate";
    }

    impl DeclaredClass for WindowCloseDelegate {}

    unsafe impl NSObjectProtocol for WindowCloseDelegate {}

    unsafe impl NSWindowDelegate for WindowCloseDelegate {
        // Return NO so AppKit does NOT destroy the native window: the guest client decides whether to close
        // in response to xdg_toplevel.close. We just record which window was asked to close.
        #[method(windowShouldClose:)]
        fn window_should_close(&self, sender: &NSWindow) -> bool {
            let wp = sender as *const NSWindow as usize;
            if let Ok(mut q) = pending_window_closes().lock() {
                if !q.contains(&wp) {
                    q.push(wp);
                }
            }
            false
        }
    }
);

/// The process-wide close delegate (created lazily on the main thread, kept alive for the process so the
/// window's weak `delegate` pointer never dangles). Every window shares one — it holds no per-window state.
fn window_close_delegate(mtm: MainThreadMarker) -> Retained<WindowCloseDelegate> {
    thread_local! {
        static DELEGATE: RefCell<Option<Retained<WindowCloseDelegate>>> = const { RefCell::new(None) };
    }
    DELEGATE.with(|d| {
        d.borrow_mut()
            .get_or_insert_with(|| {
                let this = mtm.alloc::<WindowCloseDelegate>();
                unsafe { msg_send_id![this, init] }
            })
            .clone()
    })
}

/// Install the shared close delegate on a freshly created window so its native close button routes to
/// `xdg_toplevel.close` instead of silently orphaning the guest surface.
fn install_close_delegate(window: &NSWindow, mtm: MainThreadMarker) {
    let delegate = window_close_delegate(mtm);
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
}

// ---- Focusable content window ------------------------------------------------------------------
// AppKit ties keyboard focus and much of mouse dispatch to a window's *key*/*main* status. A plain
// `NSWindow` created with the `Borderless` style mask (the Metal presenter's default, so the guest's own
// chrome shows through instead of an AppKit title bar) returns NO from `canBecomeKeyWindow`/
// `canBecomeMainWindow` — so `makeKeyAndOrderFront:` never actually makes it key, the window never takes
// keyboard focus, and clicks are swallowed as mere activation clicks. That is exactly "input is dead": no
// key events ever reach us to route into `wl_keyboard`, and pointer handling is degraded. Overriding both
// to YES makes a borderless content window behave like a normal top-level window for input, which is what
// a compositor surface must be. (Titled windows already return YES; the override is harmless there.)

declare_class!(
    struct ContentWindow;

    unsafe impl ClassType for ContentWindow {
        type Super = NSWindow;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "DDContentWindow";
    }

    impl DeclaredClass for ContentWindow {}

    unsafe impl ContentWindow {
        #[method(canBecomeKeyWindow)]
        fn can_become_key_window(&self) -> bool {
            true
        }
        #[method(canBecomeMainWindow)]
        fn can_become_main_window(&self) -> bool {
            true
        }
    }
);

// ---- First-responder content view ---------------------------------------------------------------
// The window's content `NSView` is where mouse hit-testing lands and where keyboard focus (first
// responder) must live. A stock `NSView` returns NO from both `acceptsFirstResponder` and
// `acceptsFirstMouse:`. The `acceptsFirstMouse:` NO is the direct cause of "pointer MOTION works but
// CLICKS do nothing": when the click lands on a window that is not the active app's key window, AppKit
// SWALLOWS that first `mouseDown` to merely activate the app — the `NSEvent` never reaches our
// `nextEventMatchingMask` drain, so `inject_nsevent` never sees it and no `wl_pointer.button` is sent.
// Motion needs no activation, so hover (→ `wl_pointer.set_cursor`) kept working while buttons vanished.
// Returning YES from `acceptsFirstMouse:` makes AppKit DELIVER that click (activate AND dispatch), and
// YES from `acceptsFirstResponder` lets the view hold keyboard focus so key events route into wl_keyboard.
// `isOpaque` YES lets AppKit skip compositing anything behind this fully-covered layer-backed view.

declare_class!(
    struct ContentView;

    unsafe impl ClassType for ContentView {
        type Super = NSView;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "DDContentView";
    }

    impl DeclaredClass for ContentView {}

    unsafe impl ContentView {
        #[method(acceptsFirstResponder)]
        fn accepts_first_responder(&self) -> bool {
            true
        }
        // The parameter is the triggering NSEvent; we accept regardless of where/what it is.
        #[method(acceptsFirstMouse:)]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }
        #[method(isOpaque)]
        fn is_opaque(&self) -> bool {
            true
        }
    }
);

/// Create the layer-backed content view for a compositor window. Instantiates the `ContentView` subclass so
/// the first click on an unfocused window is delivered (not swallowed for activation) and keyboard focus can
/// land on it. Returned as a plain `NSView` (the subclass adds no state callers need).
fn make_content_view(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSView> {
    let view: Retained<ContentView> =
        unsafe { msg_send_id![mtm.alloc::<ContentView>(), initWithFrame: frame] };
    Retained::into_super(view)
}

/// Create a compositor content window that can take keyboard focus even when borderless. Mirrors
/// `NSWindow::initWithContentRect_styleMask_backing_defer` but instantiates the `ContentWindow` subclass so
/// `canBecomeKeyWindow`/`canBecomeMainWindow` are YES. Returned as a plain `NSWindow` (the subclass adds no
/// state the callers need). Also enables mouse-moved delivery so guest hover state tracks the cursor.
fn make_focusable_window(
    mtm: MainThreadMarker,
    content: NSRect,
    style: NSWindowStyleMask,
) -> Retained<NSWindow> {
    let window: Retained<ContentWindow> = unsafe {
        msg_send_id![
            mtm.alloc::<ContentWindow>(),
            initWithContentRect: content,
            styleMask: style,
            backing: NSBackingStoreType::NSBackingStoreBuffered,
            defer: false,
        ]
    };
    let window: Retained<NSWindow> = Retained::into_super(window);
    window.setAcceptsMouseMovedEvents(true);
    window
}

/// Integer `wl_output.scale` to advertise, derived from the Mac's backing store. Defaults to the main
/// screen's `backingScaleFactor` (2 on Retina) so Chrome/GTK commit a native-resolution HiDPI buffer that
/// the Metal present path shows pixel-for-pixel (`contentsScale = backingScaleFactor`, drawable sized in
/// device pixels, window content sized in points) — crisp text instead of a 1x buffer upscaled over the
/// backing store. Opt out with `DD_DISPLAY_HIDPI=0` (forces scale 1) to reproduce the legacy 1x path.
fn host_output_scale(mtm: MainThreadMarker) -> i32 {
    if hidpi_disabled() {
        return 1;
    }
    NSScreen::mainScreen(mtm)
        .map(|s| s.backingScaleFactor().round() as i32)
        .unwrap_or(1)
        .max(1)
}

/// `DD_DISPLAY_HIDPI=0`/`off`/`false` disables the retina present path (advertise scale 1, present 1x).
fn hidpi_disabled() -> bool {
    matches!(
        std::env::var("DD_DISPLAY_HIDPI").ok().as_deref(),
        Some("0") | Some("off") | Some("false") | Some("no")
    )
}

/// Device-pixel size of a `w`x`h` logical (point) surface at backing `scale`. The composite texture, the
/// `CAMetalLayer` drawable, and every readback are sized in these pixels so the retina path is 1:1.
fn pixel_size(w: u32, h: u32, scale: f64) -> (u32, u32) {
    let s = scale.max(1.0);
    (
        ((w as f64 * s).round() as u32).max(1),
        ((h as f64 * s).round() as u32).max(1),
    )
}

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
        CocoaPresenter {
            mtm,
            wins: HashMap::new(),
        }
    }

    /// Render the ACTUAL on-screen NSView for `sid` (the content view AppKit draws) into a PNG. This
    /// proves the presenter's on-screen path renders — not just the compositor's framebuffer. Uses
    /// `cacheDisplayInRect:` (the same synchronous view-drawing AppKit uses for the window), so it works
    /// against the live backing store whether or not a human is looking. Returns true on success.
    pub fn dump_view_png(&self, sid: u32, out: &str) -> bool {
        let Some(win) = self.wins.get(&sid) else {
            return false;
        };
        let view = &win.image_view;
        unsafe {
            let bounds = view.bounds();
            // `cacheDisplayInRect:` draws the view synchronously into the rep against the live backing
            // store — no need to mark dirty or run the display loop first.
            let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(bounds) else {
                return false;
            };
            view.cacheDisplayInRect_toBitmapImageRep(bounds, &rep);
            let empty = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
            let Some(data) =
                rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &empty)
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
        let window = make_focusable_window(mtm, content, style);
        let t = if title.is_empty() {
            format!("dd surface {sid}")
        } else {
            title.to_string()
        };
        window.setTitle(&NSString::from_str(&t));
        install_close_delegate(&window, mtm);

        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(w as f64, h as f64));
        let image_view = unsafe { NSImageView::initWithFrame(mtm.alloc(), frame) };
        window.setContentView(Some(&image_view));
        // Guest owns the pointer shape (wp_cursor_shape_v1); keep AppKit from resetting it to the arrow.
        unsafe { window.disableCursorRects() };
        window.makeKeyAndOrderFront(None);
        Win {
            window,
            image_view,
            size: (w, h),
        }
    }
}

impl Presenter for CocoaPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) -> bool {
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

        let image =
            unsafe { NSImage::initWithSize(NSImage::alloc(), NSSize::new(w as f64, h as f64)) };
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
                let size = NSSize::new(w as f64, h as f64);
                win.window.setContentSize(size);
                win.image_view.setFrameSize(size);
            }
            win.size = (w, h);
        }
        unsafe { win.image_view.setImage(Some(&image)) };
        true
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

    fn output_scale(&self) -> i32 {
        host_output_scale(self.mtm)
    }

    fn begin_interactive_move(&self, sid: u32) {
        if let Some(w) = self.wins.get(&sid) {
            perform_window_drag(self.mtm, &w.window);
        }
    }

    fn set_cursor_shape(&self, shape: u32) {
        apply_cursor_shape(shape);
    }

    fn drop_window(&mut self, sid: u32) {
        if let Some(w) = self.wins.remove(&sid) {
            w.window.close(); // orderOut + release: the tiny cursor-image window disappears
        }
    }
}

// ---- Hardware-accelerated presenter: one NSWindow + CAMetalLayer per surface ----

struct MetalWin {
    window: Retained<NSWindow>,
    layer: Retained<CAMetalLayer>,
    /// Retina PRESENT scale: device pixels per point (`backingScaleFactor`, 1 when HiDPI is off). The layer
    /// drawable + composite texture are sized `size * scale` (device pixels); the window content stays
    /// `size` (points). Input stays point-space (see `surface_scale` → 1.0), matching `locationInWindow`.
    scale: f64,
    /// Logical surface size in POINTS (what the guest renders as; drives window content size + input flip).
    size: (u32, u32),
    /// Opaque compositor output for the current size. Chrome commits ARGB surfaces and expects the
    /// compositor to blend them over the window background; raw-blitting transparent pixels makes the
    /// native window show black margins.
    composite_tex: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    /// The most recently composited texture. Kept so `SIGUSR1` can read it back to a PNG — the on-screen
    /// `CAMetalLayer` drawable itself isn't readable after present.
    last_tex: Option<Retained<ProtocolObject<dyn MTLTexture>>>,
    debug_last: Option<PresentDebugSnapshot>,
    debug_present_seen: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentDebugMode {
    Off,
    Changes,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PresentDebugSnapshot {
    surf_size: (u32, u32),
    texture_size: (u32, u32),
    uv_rect: [f32; 4],
    iosurface_id: Option<u32>,
    content_bounds: Option<(i32, i32, i32, i32)>,
    layer_drawable_size: (i32, i32),
    drawable_texture_size: Option<(u64, u64)>,
}

/// Presents each committed `wl_shm` buffer via Metal: upload → GPU blit into the `CAMetalLayer`'s
/// drawable → present. This is the accelerated replacement for the `NSImageView` copy-blit. The shared
/// [`MetalCtx`] (device + queue) is the same one `dd-gpu`'s executor targets.
pub struct MetalPresenter {
    mtm: MainThreadMarker,
    ctx: MetalCtx,
    composite_pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    wins: HashMap<u32, MetalWin>,
    frames: u32,
    /// `DD_DISPLAY_DUMP_EVERY`: when set to N>0, read back + PNG-dump every Nth composited frame to
    /// `DD_DISPLAY_DUMP` — a headless way to capture what a short-lived app actually put on screen
    /// (the window itself is torn down when the client exits, faster than a human/SIGUSR1 can look).
    dump_every: u32,
    dump_dir: String,
    present_debug: PresentDebugMode,
    /// Backing pixels per point for the retina present path (`backingScaleFactor`, 1 when HiDPI is off).
    /// The composite texture + `CAMetalLayer` drawable are sized in device pixels (`logical * present_scale`)
    /// while the NSWindow content size stays in points, so a HiDPI buffer is shown pixel-for-pixel.
    present_scale: f64,
}

impl MetalPresenter {
    pub fn new(mtm: MainThreadMarker) -> Option<MetalPresenter> {
        let dump_every = std::env::var("DD_DISPLAY_DUMP_EVERY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let dump_dir =
            std::env::var("DD_DISPLAY_DUMP").unwrap_or_else(|_| "/tmp/dd-display-live".into());
        let present_debug = Self::present_debug_mode();
        let ctx = MetalCtx::new()?;
        let composite_pipeline = Self::make_composite_pipeline(&ctx)?;
        let present_scale = host_output_scale(mtm).max(1) as f64;
        Some(MetalPresenter {
            mtm,
            ctx,
            composite_pipeline,
            wins: HashMap::new(),
            frames: 0,
            dump_every,
            dump_dir,
            present_debug,
            present_scale,
        })
    }

    fn present_debug_mode() -> PresentDebugMode {
        match std::env::var("DD_DISPLAY_PRESENT_DEBUG") {
            Ok(v) => match v.to_ascii_lowercase().as_str() {
                "" | "0" | "false" | "off" | "no" => PresentDebugMode::Off,
                "changes" | "change" | "summary" => PresentDebugMode::Changes,
                _ => PresentDebugMode::All,
            },
            Err(_) => PresentDebugMode::Off,
        }
    }

    fn present_debug_snapshot(
        surf: &SurfaceBuffer,
        win: &MetalWin,
        drawable_texture_size: Option<(u64, u64)>,
    ) -> PresentDebugSnapshot {
        let content_bounds = win.window.contentView().map(|view| {
            let b = view.bounds();
            (
                b.origin.x.round() as i32,
                b.origin.y.round() as i32,
                b.size.width.round() as i32,
                b.size.height.round() as i32,
            )
        });
        let layer_size = unsafe { win.layer.drawableSize() };
        PresentDebugSnapshot {
            surf_size: (surf.width as u32, surf.height as u32),
            texture_size: (surf.texture_width as u32, surf.texture_height as u32),
            uv_rect: surf.uv_rect,
            iosurface_id: surf.iosurface_id,
            content_bounds,
            layer_drawable_size: (
                layer_size.width.round() as i32,
                layer_size.height.round() as i32,
            ),
            drawable_texture_size,
        }
    }

    fn log_present_debug(
        mode: PresentDebugMode,
        event: &str,
        frame: u32,
        sid: u32,
        surf: &SurfaceBuffer,
        win: &mut MetalWin,
        drawable_texture_size: Option<(u64, u64)>,
    ) {
        if mode == PresentDebugMode::Off {
            return;
        }
        let snapshot = Self::present_debug_snapshot(surf, win, drawable_texture_size);
        let changed = win.debug_last != Some(snapshot);
        let first_present_frames = event == "present" && win.debug_present_seen < 16;
        let should_log = match mode {
            PresentDebugMode::Off => false,
            PresentDebugMode::All => true,
            PresentDebugMode::Changes => event != "present" || first_present_frames || changed,
        };
        if should_log {
            let bounds = snapshot
                .content_bounds
                .map(|(x, y, w, h)| format!("x={x} y={y} w={w} h={h}"))
                .unwrap_or_else(|| "none".to_string());
            let drawable_tex = snapshot
                .drawable_texture_size
                .map(|(w, h)| format!("{w}x{h}"))
                .unwrap_or_else(|| "none".to_string());
            eprintln!(
                "dd-display[metal][present-debug]: event={event} frame={frame} sid={sid} \
surf={}x{} texture={}x{} uv=[{:.6},{:.6},{:.6},{:.6}] iosurface={} \
content_bounds=({bounds}) layer_drawable={}x{} drawable_tex={} clear=white rgba=(1,1,1,1)",
                snapshot.surf_size.0,
                snapshot.surf_size.1,
                snapshot.texture_size.0,
                snapshot.texture_size.1,
                snapshot.uv_rect[0],
                snapshot.uv_rect[1],
                snapshot.uv_rect[2],
                snapshot.uv_rect[3],
                snapshot
                    .iosurface_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                snapshot.layer_drawable_size.0,
                snapshot.layer_drawable_size.1,
                drawable_tex,
            );
        }
        if event == "present" {
            win.debug_present_seen = win.debug_present_seen.saturating_add(1);
        }
        win.debug_last = Some(snapshot);
    }

    fn make_composite_pipeline(
        ctx: &MetalCtx,
    ) -> Option<Retained<ProtocolObject<dyn MTLRenderPipelineState>>> {
        const SRC: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VOut {
    float4 pos [[position]];
    float2 uv;
};

vertex VOut vmain(uint vid [[vertex_id]]) {
    float2 pos[3] = {
        float2(-1.0, -1.0),
        float2( 3.0, -1.0),
        float2(-1.0,  3.0)
    };
    float2 uv[3] = {
        float2(0.0, 1.0),
        float2(2.0, 1.0),
        float2(0.0, -1.0)
    };
    VOut out;
    out.pos = float4(pos[vid], 0.0, 1.0);
    out.uv = uv[vid];
    return out;
}

fragment float4 fmain(VOut in [[stage_in]], texture2d<float> src [[texture(0)]],
                      constant float4& uv_rect [[buffer(0)]]) {
    constexpr sampler smp(address::clamp_to_edge, filter::nearest);
    float2 uv = float2(mix(uv_rect.x, uv_rect.z, in.uv.x),
                       mix(uv_rect.y, uv_rect.w, in.uv.y));
    float4 c = src.sample(smp, uv);
    float3 bg = float3(1.0, 1.0, 1.0);
    return float4(min(c.rgb + bg * (1.0 - c.a), 1.0), 1.0);
}
"#;
        let lib = match ctx
            .device
            .newLibraryWithSource_options_error(&NSString::from_str(SRC), None)
        {
            Ok(lib) => lib,
            Err(err) => {
                eprintln!("dd-display[metal]: composite MSL compile failed: {err:?}");
                return None;
            }
        };
        let vfn = lib.newFunctionWithName(&NSString::from_str("vmain"))?;
        let ffn = lib.newFunctionWithName(&NSString::from_str("fmain"))?;
        let pdesc = MTLRenderPipelineDescriptor::new();
        pdesc.setVertexFunction(Some(&vfn));
        pdesc.setFragmentFunction(Some(&ffn));
        unsafe {
            pdesc
                .colorAttachments()
                .objectAtIndexedSubscript(0)
                .setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        }
        match ctx
            .device
            .newRenderPipelineStateWithDescriptor_error(&pdesc)
        {
            Ok(pipeline) => Some(pipeline),
            Err(err) => {
                eprintln!("dd-display[metal]: composite pipeline creation failed: {err:?}");
                None
            }
        }
    }

    fn make_window(
        mtm: MainThreadMarker,
        ctx: &MetalCtx,
        sid: u32,
        w: u32,
        h: u32,
        present_scale: f64,
        title: &str,
    ) -> MetalWin {
        let content = NSRect::new(
            NSPoint::new(140.0 + sid as f64 * 24.0, 140.0),
            NSSize::new(w as f64, h as f64),
        );
        let borderless = std::env::var_os("DD_DISPLAY_WINDOW_CHROME").is_none();
        let style = if borderless {
            NSWindowStyleMask::Borderless
        } else {
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::Miniaturizable
        };
        let window = make_focusable_window(mtm, content, style);
        window.setOpaque(true);
        window.setMovable(false);
        window.setMovableByWindowBackground(false);
        // Borderless Chrome draws its own client-side chrome; the guest surface fills the whole window and
        // is opaque. Drop the macOS drop shadow (the "ugly halo/background behind Chrome" the user sees —
        // a borderless window still gets the system shadow) and clear the window's default gray background
        // color so nothing dd-side can bleed at the edges/corners behind the content. The titled variant
        // keeps its normal shadow. (The light-blue titlebar, if any, is Chrome's OWN CSD — not ours.)
        if borderless {
            window.setHasShadow(false);
            unsafe { window.setBackgroundColor(Some(&NSColor::clearColor())) };
        }
        let t = if title.is_empty() {
            format!("dd surface {sid} (metal)")
        } else {
            title.to_string()
        };
        window.setTitle(&NSString::from_str(&t));
        install_close_delegate(&window, mtm);

        let layer = unsafe { CAMetalLayer::new() };
        unsafe {
            layer.setDevice(Some(&ctx.device));
            layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            layer.setFramebufferOnly(false);
            layer.setOpaque(true);
        }
        // Retina present path. `w`/`h` are the guest's LOGICAL surface size (points): the NSWindow content
        // size stays in points, so a 1600x1200-pixel HiDPI buffer shows as an 800x600-point window on a 2x
        // display. The CAMetalLayer, however, is sized in DEVICE PIXELS — `contentsScale = backingScaleFactor`
        // and `drawableSize = logical * scale` — so the composited HiDPI buffer is presented pixel-for-pixel
        // (crisp text) rather than a 1x buffer upscaled by Core Animation. Do NOT feed the pixel size back
        // into xdg resize: `window_content_size` reads the point-space view bounds, keeping the guest logical.
        let scale = present_scale.max(1.0);
        layer.setContentsScale(scale);
        unsafe {
            layer.setDrawableSize(NSSize::new(w as f64 * scale, h as f64 * scale));
        }
        let point_size = NSSize::new(w as f64, h as f64);
        window.setContentSize(point_size);
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), point_size);
        let view = make_content_view(mtm, frame);
        unsafe {
            view.setAutoresizingMask(
                NSAutoresizingMaskOptions::NSViewWidthSizable
                    | NSAutoresizingMaskOptions::NSViewHeightSizable,
            );
            view.setWantsLayer(true);
            view.setLayer(Some(&layer));
        }
        window.setContentView(Some(&view));
        // Make the content view the window's first responder so keyboard events have a home and clicks that
        // AppKit routes by responder chain reach it; combined with the view's acceptsFirstMouse:YES, the
        // first click on this (often non-key) borderless window is delivered rather than swallowed.
        window.setInitialFirstResponder(Some(&view));
        // Disable AppKit's automatic cursor-rect management: the guest owns the pointer shape (via
        // wp_cursor_shape_v1 → apply_cursor_shape), so a shape we set must stick over the content instead of
        // being reset to the arrow on the next mouse-moved / cursorUpdate.
        unsafe { window.disableCursorRects() };
        window.makeKeyAndOrderFront(None);
        let _ = window.makeFirstResponder(Some(&view));
        // Force the new window in front of every space/app and re-assert app foreground, so Core Animation
        // composites this CAMetalLayer at the display rate rather than background-throttling its present
        // (which would pace the frame-callback-driven guest down to ~1 fps). See run_window() for why.
        unsafe { window.orderFrontRegardless() };
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        MetalWin {
            window,
            layer,
            scale,
            size: (w, h),
            composite_tex: None,
            last_tex: None,
            debug_last: None,
            debug_present_seen: 0,
        }
    }
}

impl Presenter for MetalPresenter {
    fn present(&mut self, surf: &SurfaceBuffer) -> bool {
        if surf.width <= 0 || surf.height <= 0 || surf.texture_width <= 0 || surf.texture_height <= 0 {
            return false;
        }
        let (w, h) = (surf.width as u32, surf.height as u32);
        let (tex_w, tex_h) = (surf.texture_width as u32, surf.texture_height as u32);
        // GPU rung 2: if the buffer is an IOSurface (dmabuf), wrap it directly (ZERO copy/upload);
        // otherwise upload the shm bytes. `CFRelease` the looked-up surface after wrapping.
        let src = match surf.iosurface_id {
            Some(id) => {
                let surface = unsafe { crate::metal::resolve_iosurface(id) };
                if surface.is_null() {
                    eprintln!("dd-display[metal]: IOSurface id {id} not found; skipping frame");
                    return false;
                }
                let tex = self.ctx.texture_from_iosurface(surface, tex_w, tex_h);
                if surf.gpu_render && std::env::var_os("DD_DISPLAY_TEST_TRIANGLE").is_some() {
                    self.ctx.render_triangle_into(&tex); // rung 3: host GPU renders into the guest IOSurface
                }
                unsafe { crate::metal::cfrelease(surface) };
                tex
            }
            None => self.ctx.upload_bgra(&surf.bgra, tex_w, tex_h),
        };

        let mtm = self.mtm;
        let title = surf.title.clone();
        let sid = surf.sid;
        let ctx = &self.ctx;
        let present_scale = self.present_scale;
        let created = !self.wins.contains_key(&sid);
        let win = self
            .wins
            .entry(sid)
            .or_insert_with(|| MetalPresenter::make_window(mtm, ctx, sid, w, h, present_scale, &title));
        if created {
            MetalPresenter::log_present_debug(
                self.present_debug,
                "create",
                self.frames + 1,
                sid,
                surf,
                win,
                None,
            );
        }
        if win.size != (w, h) {
            unsafe {
                let scale = present_scale.max(1.0);
                win.scale = scale;
                // Content size in points; drawable in device pixels (see make_window).
                let size = NSSize::new(w as f64, h as f64);
                win.window.setContentSize(size);
                if let Some(view) = win.window.contentView() {
                    view.setFrameSize(size);
                }
                win.layer.setContentsScale(scale);
                win.layer.setDrawableSize(NSSize::new(w as f64 * scale, h as f64 * scale));
            }
            win.size = (w, h);
            win.composite_tex = None;
            MetalPresenter::log_present_debug(
                self.present_debug,
                "resize",
                self.frames + 1,
                sid,
                surf,
                win,
                None,
            );
        }
        // The composite texture is the presented image: size it in DEVICE PIXELS (logical * present_scale)
        // to match the drawable, so a HiDPI guest buffer is composited at full resolution and blitted 1:1.
        let (px_w, px_h) = pixel_size(w, h, win.scale);
        if win.composite_tex.is_none() {
            win.composite_tex = Some(self.ctx.new_bgra_texture(px_w, px_h));
        }
        let composite = win
            .composite_tex
            .as_ref()
            .expect("composite texture")
            .clone();
        // A drawable to show on the visible window, IF dd-display is the FOREGROUND app. When it is not, Core
        // Animation throttles drawable vending to this layer and `nextDrawable` BLOCKS the single-threaded
        // present loop ~1s per frame (then returns nil) -- which stalls buffer releases, wl frame callbacks
        // AND the GPU executor, freezing the frame-callback-paced guest for seconds while backgrounded (the
        // "renders in bursts then stalls / takes an hour to propagate" bug). So we only ask for a drawable
        // while active; when inactive we skip the visible blit. The compositor pass below still renders into
        // our OWN `composite_tex` (no drawable needed), so the guest keeps producing frames at full rate, wl
        // frame pacing keeps advancing, and `last_tex` stays current for SIGUSR1 readback -- the on-screen
        // window catches up the instant it is refocused. Withholding the frame (the old `return false` on a
        // nil drawable) is what coupled the guest to CA's background throttle.
        let app_active = unsafe { NSApplication::sharedApplication(mtm).isActive() };
        let drawable = if app_active {
            unsafe { win.layer.nextDrawable() }
        } else {
            None
        };
        let drawable_texture_size = drawable
            .as_ref()
            .map(|d| unsafe { (d.texture().width() as u64, d.texture().height() as u64) });
        if drawable.is_none() {
            MetalPresenter::log_present_debug(
                self.present_debug,
                "no-drawable",
                self.frames + 1,
                sid,
                surf,
                win,
                None,
            );
        }
        // Composite (always) + blit into the drawable (only when one was vended), in one command buffer. L4:
        // if the source is a guest IOSurface the executor renders into asynchronously, guard the read with the
        // cross-queue tearing fence (wait for render-complete, signal read-complete) so a partly-rendered
        // surface is never sampled. The signal MUST be paired with the wait even when there is no drawable, or
        // the executor deadlocks awaiting a completion that never comes.
        let fence = match surf.iosurface_id {
            Some(id) if crate::metal::async_on() => crate::metal::fence_begin_present(id),
            _ => None,
        };
        let cmd = self.ctx.queue.commandBuffer().expect("commandBuffer");
        if let Some((render_ev, _p, gen)) = &fence {
            cmd.encodeWaitForEvent_value(render_ev, *gen);
        }
        let pass = unsafe { MTLRenderPassDescriptor::renderPassDescriptor() };
        let ca = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        ca.setTexture(Some(&composite));
        ca.setLoadAction(MTLLoadAction::Clear);
        ca.setClearColor(MTLClearColor {
            red: 1.0,
            green: 1.0,
            blue: 1.0,
            alpha: 1.0,
        });
        ca.setStoreAction(MTLStoreAction::Store);
        let enc = cmd
            .renderCommandEncoderWithDescriptor(&pass)
            .expect("render encoder");
        enc.setRenderPipelineState(&self.composite_pipeline);
        unsafe {
            enc.setFragmentTexture_atIndex(Some(&src), 0);
            let uv = std::ptr::NonNull::new(surf.uv_rect.as_ptr() as *mut std::ffi::c_void)
                .expect("uv rect pointer");
            enc.setFragmentBytes_length_atIndex(
                uv,
                std::mem::size_of_val(&surf.uv_rect),
                0,
            );
            enc.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::Triangle, 0, 3);
        }
        enc.endEncoding();
        if let Some(drawable) = &drawable {
            let dst = unsafe { drawable.texture() };
            let blit = cmd.blitCommandEncoder().expect("blit");
            unsafe { blit.copyFromTexture_toTexture(&composite, &dst) };
            blit.endEncoding();
        }
        if let Some((_r, present_ev, gen)) = &fence {
            cmd.encodeSignalEvent_value(present_ev, *gen);
        }
        if let Some(drawable) = &drawable {
            cmd.presentDrawable(objc2::runtime::ProtocolObject::from_ref(&**drawable));
        }
        cmd.commit();
        win.last_tex = Some(composite); // keep opaque compositor output for SIGUSR1 readback

        // Headless capture of the LIVE composited frame (opt-in), so a short-lived app's on-screen pixels
        // can be Read back even after its window is gone.
        self.frames += 1;
        MetalPresenter::log_present_debug(
            self.present_debug,
            "present",
            self.frames,
            sid,
            surf,
            win,
            drawable_texture_size,
        );
        if self.dump_every > 0 && self.frames % self.dump_every == 0 {
            if let Some(w) = self.wins.get(&sid) {
                if let Some(tex) = w.last_tex.as_ref() {
                    let (pw, ph) = pixel_size(w.size.0, w.size.1, w.scale);
                    let bgra = self.ctx.readback_bgra(tex, pw, ph);
                    let mut rgba = vec![0u8; bgra.len()];
                    for i in (0..bgra.len()).step_by(4) {
                        rgba[i] = bgra[i + 2];
                        rgba[i + 1] = bgra[i + 1];
                        rgba[i + 2] = bgra[i];
                        rgba[i + 3] = 0xff;
                    }
                    let png = dd_term_core::png::encode_rgba(pw, ph, &rgba);
                    let _ = std::fs::create_dir_all(&self.dump_dir);
                    let path = format!(
                        "{}/live-surface-{sid}-{:04}.png",
                        self.dump_dir, self.frames
                    );
                    if std::fs::write(&path, png).is_ok() {
                        eprintln!(
                            "dd-display[metal]: live frame {} dumped -> {path}",
                            self.frames
                        );
                    }
                }
            }
        }
        true
    }

    fn frame_count(&self) -> u32 {
        self.frames
    }

    fn surface_size(&self, sid: u32) -> Option<(i32, i32)> {
        self.wins
            .get(&sid)
            .map(|w| (w.size.0 as i32, w.size.1 as i32))
    }

    fn dump_pngs(&self, dir: &str) -> usize {
        let _ = std::fs::create_dir_all(dir);
        let mut n = 0;
        for (sid, w) in self.wins.iter() {
            let Some(tex) = w.last_tex.as_ref() else {
                continue;
            };
            let (pw, ph) = pixel_size(w.size.0, w.size.1, w.scale);
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
        Some((
            b.size.width.round() as i32,
            b.size.height.round() as i32,
        ))
    }

    fn surface_scale(&self, sid: u32) -> f64 {
        self.wins.get(&sid).map(|_| 1.0).unwrap_or(1.0)
    }

    fn output_scale(&self) -> i32 {
        host_output_scale(self.mtm)
    }

    fn begin_interactive_move(&self, sid: u32) {
        if let Some(w) = self.wins.get(&sid) {
            perform_window_drag(self.mtm, &w.window);
        }
    }

    fn set_cursor_shape(&self, shape: u32) {
        apply_cursor_shape(shape);
    }

    fn drop_window(&mut self, sid: u32) {
        if let Some(w) = self.wins.remove(&sid) {
            w.window.close(); // orderOut + release: the tiny cursor-image window disappears
        }
    }
}

/// Map a `wp_cursor_shape_device_v1.shape` enum to the closest `NSCursor` and set it as the host cursor.
/// Unmapped shapes fall back to the arrow. Called on the main thread (the presenter loop), where AppKit
/// cursor state lives. The windows disable AppKit cursor rects (see `make_window`) so a shape set here
/// sticks over the content instead of being reset to the arrow on the next mouse-moved.
fn apply_cursor_shape(shape: u32) {
    // wp_cursor_shape_device_v1.shape enum (cursor-shape-v1.xml).
    let cursor = match shape {
        4 => NSCursor::pointingHandCursor(),           // pointer (over a link)
        9 => NSCursor::IBeamCursor(),                  // text
        10 => NSCursor::IBeamCursorForVerticalLayout(), // vertical_text
        8 => NSCursor::crosshairCursor(),              // crosshair
        16 => NSCursor::openHandCursor(),              // grab
        13 | 17 => NSCursor::closedHandCursor(),        // move / grabbing
        18 | 25 | 26 | 30 => NSCursor::resizeLeftRightCursor(), // e/w/ew/col resize
        19 | 22 | 27 | 31 => NSCursor::resizeUpDownCursor(),    // n/s/ns/row resize
        14 | 15 => NSCursor::operationNotAllowedCursor(),       // no_drop / not_allowed
        11 | 12 => NSCursor::dragCopyCursor(),          // alias / copy
        _ => NSCursor::arrowCursor(),                   // default + anything without a good AppKit match
    };
    unsafe { cursor.set() };
}

/// Start a native, host-driven window drag for `window` in response to `xdg_toplevel.move`. AppKit's
/// `performWindowDragWithEvent:` re-uses the in-flight mouse-down (the physical button is still held, since
/// the client only issues `move` while dragging) to move the window — the precise, request-gated
/// alternative to `setMovableByWindowBackground(true)` (which would move the window on ANY background drag).
fn perform_window_drag(mtm: MainThreadMarker, window: &NSWindow) {
    let app = NSApplication::sharedApplication(mtm);
    if let Some(ev) = app.currentEvent() {
        window.performWindowDragWithEvent(&ev);
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
        let mut pfd = libc::pollfd {
            fd: cfd,
            events: libc::POLLIN,
            revents: 0,
        };
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
    eprintln!(
        "dd-display[cocoa]: SIGUSR1 dumped {total} live window(s) -> {dir}/live-surface-*.png"
    );
}

/// The shared main-thread event loop: pump the Wayland client + drain AppKit events (routing input into
/// the seat), forever. Single-client path (the proven default when neither `--png` nor `--window` picks
/// the multiplexed live loop).
fn drive<P: Presenter>(app: Retained<NSApplication>, cfd: RawFd, mut server: Server<P>) -> ! {
    install_dump_handler();
    loop {
        let mut pfd = libc::pollfd {
            fd: cfd,
            events: libc::POLLIN,
            revents: 0,
        };
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
                    inject_nsevent(&mut server, &ev, flip_h, 1.0);
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
fn inject_nsevent<P: Presenter>(server: &mut Server<P>, ev: &NSEvent, flip_h: Option<i32>, scale: f64) {
    let ty = unsafe { ev.r#type() };
    if ty == NSEventType::MouseMoved
        || ty == NSEventType::LeftMouseDragged
        || ty == NSEventType::RightMouseDragged
    {
        let p = unsafe { ev.locationInWindow() };
        let (x, y) = flip_point(p, flip_h, scale);
        server.pointer_motion(x, y);
    } else if ty == NSEventType::LeftMouseDown {
        let p = unsafe { ev.locationInWindow() };
        let (x, y) = flip_point(p, flip_h, scale);
        server.pointer_motion(x, y);
        server.pointer_button(0x110, true);
    } else if ty == NSEventType::LeftMouseUp {
        let p = unsafe { ev.locationInWindow() };
        let (x, y) = flip_point(p, flip_h, scale);
        server.pointer_motion(x, y);
        server.pointer_button(0x110, false);
    } else if ty == NSEventType::RightMouseDown {
        let p = unsafe { ev.locationInWindow() };
        let (x, y) = flip_point(p, flip_h, scale);
        server.pointer_motion(x, y);
        server.pointer_button(0x111, true);
    } else if ty == NSEventType::RightMouseUp {
        let p = unsafe { ev.locationInWindow() };
        let (x, y) = flip_point(p, flip_h, scale);
        server.pointer_motion(x, y);
        server.pointer_button(0x111, false);
    } else if ty == NSEventType::ScrollWheel {
        // Cocoa scroll deltas are content-follows-finger (natural): a positive scrollingDeltaY means the
        // content should move down, which in Wayland is a NEGATIVE axis value. Negate both to match the
        // Wayland axis convention (positive = scroll down/right). One NSEvent = one logical scroll, so both
        // axes are delivered in a single wl_pointer.frame group. `hasPreciseScrollingDeltas` distinguishes a
        // trackpad (smooth pixel/continuous source) from a stepped mouse wheel.
        let dy = unsafe { ev.scrollingDeltaY() };
        let dx = unsafe { ev.scrollingDeltaX() };
        let precise = unsafe { ev.hasPreciseScrollingDeltas() };
        let vy = -(dy.round() as i32);
        let vx = -(dx.round() as i32);
        if vx != 0 || vy != 0 {
            server.pointer_scroll(vx, vy, precise);
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
        if f.contains(NSEventModifierFlags::NSEventModifierFlagShift) {
            dep |= 1;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagCapsLock) {
            dep |= 2;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagControl) {
            dep |= 4;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagOption) {
            dep |= 8;
        }
        if f.contains(NSEventModifierFlags::NSEventModifierFlagCommand) {
            dep |= 64;
        }
        server.modifiers(dep, 0, 0, 0);
    }
}

/// macOS virtual keycode (`kVK_*`) → Linux evdev `KEY_*`. Covers the alphanumerics + common keys; unmapped
/// keys are dropped. (Ported subset — the full XQuartz/SDL table is a follow-up.)
fn kvk_to_evdev(kvk: u16) -> Option<u32> {
    Some(match kvk {
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
        31 => 24,
        32 => 22,
        34 => 23,
        35 => 25,
        37 => 38,
        38 => 36,
        40 => 37,
        45 => 49,
        46 => 50,
        18 => 2,
        19 => 3,
        20 => 4,
        21 => 5,
        22 => 7,
        23 => 6,
        25 => 10,
        26 => 8,
        28 => 9,
        29 => 11,
        36 => 28, // Return → KEY_ENTER
        48 => 15, // Tab
        49 => 57, // Space
        51 => 14, // Delete → KEY_BACKSPACE
        53 => 1,  // Escape
        // Punctuation (kVK_ANSI_* → KEY_*): needed so typed symbols reach the client's xkb layer.
        27 => 12, // Minus       → KEY_MINUS
        24 => 13, // Equal       → KEY_EQUAL
        33 => 26, // LeftBracket → KEY_LEFTBRACE
        30 => 27, // RightBracket→ KEY_RIGHTBRACE
        41 => 39, // Semicolon   → KEY_SEMICOLON
        39 => 40, // Quote       → KEY_APOSTROPHE
        50 => 41, // Grave       → KEY_GRAVE
        42 => 43, // Backslash   → KEY_BACKSLASH
        43 => 51, // Comma       → KEY_COMMA
        47 => 52, // Period      → KEY_DOT
        44 => 53, // Slash       → KEY_SLASH
        123 => 105,
        124 => 106,
        125 => 108,
        126 => 103, // arrows: Left/Right/Down/Up
        _ => return None,
    })
}

/// Cocoa `locationInWindow` (bottom-left origin) → surface-local top-left pixel coords. `flip_h` is the
/// focused surface height; without it we pass the raw y (last-resort).
fn flip_point(p: NSPoint, flip_h: Option<i32>, scale: f64) -> (i32, i32) {
    let scale = scale.max(1.0);
    let x = ((p.x * scale).round() as i32).max(0);
    let py = (p.y * scale).round() as i32;
    let y = match flip_h {
        Some(h) if h > 0 => (h - py).clamp(0, h - 1),
        _ => py.max(0),
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
        pfds.push(libc::pollfd {
            fd: lfd,
            events: libc::POLLIN,
            revents: 0,
        });
        for c in &clients {
            pfds.push(libc::pollfd {
                fd: c.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
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
                    let cfd =
                        unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
                    if cfd < 0 {
                        break;
                    }
                    set_nonblock(cfd);
                    match make() {
                        Some(p) => {
                            eprintln!(
                                "dd-display[{tag}]: client connected (fd {cfd}, {} live)",
                                clients.len() + 1
                            );
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
                let Some(idx) = clients.iter().position(|c| c.raw_fd() == fd) else {
                    continue;
                };
                let mirrored_crop = mirrored_input_geometry_crop(&clients, idx);
                clients[idx].set_external_logical_crop(mirrored_crop);
                let alive = matches!(clients[idx].pump(), Ok(true));
                clients[idx].set_external_logical_crop(None);
                // xdg_toplevel.move → start a HOST window drag for exactly the surface Chrome asked to move.
                if let Some(sid) = clients[idx].take_move_request() {
                    clients[idx].presenter().begin_interactive_move(sid);
                }
                if !alive {
                    eprintln!(
                        "dd-display[{tag}]: client disconnected ({} frame(s))",
                        clients[idx].presenter().frame_count()
                    );
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
            if !c.can_receive_input() {
                continue;
            }
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
        // Native close button → xdg_toplevel.close. The delegate refused AppKit's close and queued the
        // window pointer; translate each to its owning client+surface and ask the guest to close. The
        // window stays up until the client tears down its surface (or exits), matching Wayland semantics.
        let closes: Vec<usize> = pending_window_closes()
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for wp in closes {
            let ptr = wp as *const std::ffi::c_void;
            for c in clients.iter_mut() {
                if let Some(sid) = c.presenter().window_ptr_to_sid(ptr) {
                    c.send_close_request(sid);
                    break;
                }
            }
        }
    }
}

fn mirror_input_geometry_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("DD_DISPLAY_MIRROR_INPUT_GEOMETRY")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

fn mirrored_input_geometry_crop<P: Presenter>(
    clients: &[Server<P>],
    target_idx: usize,
) -> Option<ExternalLogicalCrop> {
    if !mirror_input_geometry_enabled() || clients.get(target_idx)?.can_receive_input() {
        return None;
    }
    let candidates = input_capable_candidates(clients);
    let [source_idx] = candidates.as_slice() else {
        return None;
    };
    let geom = clients[*source_idx].focused_logical_geometry()?;
    Some(ExternalLogicalCrop {
        source_client: *source_idx,
        source_surface: geom.surface,
        source: geom.source,
        x: geom.x,
        y: geom.y,
        w: geom.w,
        h: geom.h,
    })
}

/// Route an `NSEvent` to the client that owns the window it targeted (matched by `NSWindow*`). Falls back
/// to the sole client's focused surface for events whose window is nil (some key events) when unambiguous.
fn route_input<P: Presenter>(clients: &mut [Server<P>], ev: &NSEvent) {
    let mtm = MainThreadMarker::new().expect("route_input on main thread");
    let ty = unsafe { ev.r#type() };
    let target = unsafe { ev.window(mtm) };
    if let Some(win) = target {
        let wp = Retained::as_ptr(&win) as *const std::ffi::c_void;
        for i in 0..clients.len() {
            if let Some(sid) = clients[i].presenter().window_ptr_to_sid(wp) {
                let owner_size = clients[i].presenter().surface_size(sid);
                let flip_h = owner_size.map(|(_, h)| h);
                let scale = clients[i].presenter().surface_scale(sid);
                let owner_can_receive_input = clients[i].can_receive_input();
                if owner_can_receive_input {
                    input_debug_route(
                        ty,
                        Some(wp),
                        Some((i, sid, owner_size, scale, owner_can_receive_input)),
                        &[],
                        Some(i),
                        "deliver_owner",
                    );
                    inject_nsevent(&mut clients[i], ev, flip_h, scale);
                    return;
                }
                let mut forwarded_candidates = Vec::new();
                for (j, c) in clients.iter().enumerate() {
                    if j == i || !c.can_receive_input() {
                        continue;
                    }
                    forwarded_candidates.push(j);
                }
                match forwarded_candidates.as_slice() {
                    [j] => {
                        input_debug_route(
                            ty,
                            Some(wp),
                            Some((i, sid, owner_size, scale, owner_can_receive_input)),
                            &forwarded_candidates,
                            Some(*j),
                            "forward_owner_window_to_input_client",
                        );
                        inject_nsevent(&mut clients[*j], ev, flip_h, scale);
                    }
                    [] => input_debug_route(
                        ty,
                        Some(wp),
                        Some((i, sid, owner_size, scale, owner_can_receive_input)),
                        &forwarded_candidates,
                        None,
                        "drop_owner_cannot_receive_input_no_forward_candidate",
                    ),
                    _ => input_debug_route(
                        ty,
                        Some(wp),
                        Some((i, sid, owner_size, scale, owner_can_receive_input)),
                        &forwarded_candidates,
                        None,
                        "drop_owner_cannot_receive_input_ambiguous_forward_candidates",
                    ),
                }
                return;
            }
        }
        let candidates = input_capable_candidates(clients);
        input_debug_route(
            ty,
            Some(wp),
            None,
            &candidates,
            None,
            "drop_target_window_not_owned",
        );
        return;
    }
    let mut candidates = Vec::new();
    let mut candidate: Option<(usize, Option<i32>, f64)> = None;
    for (i, c) in clients.iter().enumerate() {
        if !c.can_receive_input() {
            continue;
        }
        let Some(sid) = c.focused_surface() else {
            continue;
        };
        let size = c.presenter().surface_size(sid);
        let flip_h = size.map(|(_, h)| h);
        let scale = c.presenter().surface_scale(sid);
        candidates.push(i);
        if candidate.is_some() {
            continue;
        }
        candidate = Some((i, flip_h, scale));
    }
    match candidates.as_slice() {
        [i] => {
            let (_, flip_h, scale) = candidate.expect("single candidate");
            input_debug_route(
                ty,
                None,
                None,
                &candidates,
                Some(*i),
                "deliver_nil_window_single_input_candidate",
            );
            inject_nsevent(&mut clients[*i], ev, flip_h, scale);
        }
        [] => input_debug_route(
            ty,
            None,
            None,
            &candidates,
            None,
            "drop_nil_window_no_input_candidate",
        ),
        _ => input_debug_route(
            ty,
            None,
            None,
            &candidates,
            None,
            "drop_nil_window_ambiguous_input_candidates",
        ),
    }
}

fn input_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        std::env::var_os("DD_DISPLAY_INPUT_DEBUG")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

fn input_capable_candidates<P: Presenter>(clients: &[Server<P>]) -> Vec<usize> {
    clients
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.can_receive_input().then_some(i))
        .collect()
}

fn input_debug_route(
    ty: NSEventType,
    target_window: Option<*const std::ffi::c_void>,
    owner: Option<(usize, u32, Option<(i32, i32)>, f64, bool)>,
    forwarded_candidates: &[usize],
    selected_client: Option<usize>,
    reason: &str,
) {
    if !input_debug_enabled() {
        return;
    }
    let target_window = target_window
        .map(|p| format!("{p:p}"))
        .unwrap_or_else(|| "nil".to_string());
    let owner = owner
        .map(|(idx, sid, size, scale, can_receive)| {
            let size = size
                .map(|(w, h)| format!("{w}x{h}"))
                .unwrap_or_else(|| "unknown".to_string());
            format!(
                "client={idx} sid={sid} size={size} scale={scale:.3} can_receive_input={can_receive}"
            )
        })
        .unwrap_or_else(|| "none".to_string());
    let selected = selected_client
        .map(|idx| idx.to_string())
        .unwrap_or_else(|| "none".to_string());
    eprintln!(
        "dd-display[input]: event={} target_window={} owner=[{}] forward_candidates={:?} selected_client={} reason={}",
        event_type_name(ty),
        target_window,
        owner,
        forwarded_candidates,
        selected,
        reason,
    );
}

fn event_type_name(ty: NSEventType) -> String {
    let name = if ty == NSEventType::MouseMoved {
        "MouseMoved"
    } else if ty == NSEventType::LeftMouseDragged {
        "LeftMouseDragged"
    } else if ty == NSEventType::RightMouseDragged {
        "RightMouseDragged"
    } else if ty == NSEventType::LeftMouseDown {
        "LeftMouseDown"
    } else if ty == NSEventType::LeftMouseUp {
        "LeftMouseUp"
    } else if ty == NSEventType::RightMouseDown {
        "RightMouseDown"
    } else if ty == NSEventType::RightMouseUp {
        "RightMouseUp"
    } else if ty == NSEventType::ScrollWheel {
        "ScrollWheel"
    } else if ty == NSEventType::KeyDown {
        "KeyDown"
    } else if ty == NSEventType::KeyUp {
        "KeyUp"
    } else if ty == NSEventType::FlagsChanged {
        "FlagsChanged"
    } else {
        "Other"
    };
    format!("{name}({})", ty.0)
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
        let mut pfd = libc::pollfd {
            fd: cfd,
            events: libc::POLLIN,
            revents: 0,
        };
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
        ) { inject_nsevent(&mut server, &ev, flip_h, 1.0); }
        pump_a_while(&mut server, &app, cfd, 6);

        if let Some(ev) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseDown, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, 0 as NSInteger, 1 as NSInteger, 1.0 as c_float,
        ) { inject_nsevent(&mut server, &ev, flip_h, 1.0); }
        pump_a_while(&mut server, &app, cfd, 6);

        if let Some(ev) = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            NSEventType::LeftMouseUp, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, 0 as NSInteger, 1 as NSInteger, 0.0 as c_float,
        ) { inject_nsevent(&mut server, &ev, flip_h, 1.0); }
        pump_a_while(&mut server, &app, cfd, 6);

        // Key 'a' (kVK_ANSI_A = 0) down + up.
        let a = NSString::from_str("a");
        if let Some(ev) = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyDown, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, &a, &a, false, 0 as c_ushort,
        ) { inject_nsevent(&mut server, &ev, flip_h, 1.0); }
        pump_a_while(&mut server, &app, cfd, 6);
        if let Some(ev) = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
            NSEventType::KeyUp, NSPoint::new(100.0, cy), empty, 0.0, 0 as NSInteger, none_ctx, &a, &a, false, 0 as c_ushort,
        ) { inject_nsevent(&mut server, &ev, flip_h, 1.0); }
        pump_a_while(&mut server, &app, cfd, 10);
    }

    // Dump the live view(s) so the driver can Read what's on screen.
    let dir = std::path::Path::new(out)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into());
    let dumped = server.presenter().dump_pngs(&dir);
    let _ = server.presenter().dump_view_png(7, out);

    unsafe {
        libc::waitpid(pid, std::ptr::null_mut(), 0);
        libc::close(cfd);
        libc::close(lfd);
    }
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
            NSEventMask::Any,
            None,
            NSDefaultRunLoopMode,
            true,
        ) {
            app.sendEvent(&ev);
        }
    }
}

/// Pump the client socket + AppKit for `iters` short turns so a just-injected event flushes to the guest
/// and any reply is read.
fn pump_a_while<P: Presenter>(server: &mut Server<P>, app: &NSApplication, cfd: RawFd, iters: u32) {
    for _ in 0..iters {
        let mut pfd = libc::pollfd {
            fd: cfd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut pfd, 1, 20) };
        let _ = server.pump();
        drain_appkit(app);
    }
}
