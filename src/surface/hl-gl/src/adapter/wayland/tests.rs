use super::*;

fn blank() -> Wayland {
    Wayland {
        fd: -1,
        tx: Vec::new(),
        rx: Vec::new(),
        ready: false,
        globals: Vec::new(),
        sync_done: false,
        configure_serial: None,
        frame_done: false,
    }
}

fn global_event(name: u32, interface: &str, version: u32) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&name.to_le_bytes());
    body.extend_from_slice(&((interface.len() + 1) as u32).to_le_bytes());
    let mut s = interface.as_bytes().to_vec();
    s.push(0);
    while !s.len().is_multiple_of(4) {
        s.push(0);
    }
    body.extend_from_slice(&s);
    body.extend_from_slice(&version.to_le_bytes());
    let size = (8 + body.len()) as u32;
    let mut msg = Vec::new();
    msg.extend_from_slice(&OBJ_REGISTRY.to_le_bytes());
    msg.extend_from_slice(&(size << 16).to_le_bytes());
    msg.extend_from_slice(&body);
    msg
}

#[test]
fn platform_recognition_and_extensions() {
    assert!(WaylandPlatform::contains(EGL_PLATFORM_WAYLAND_KHR));
    assert!(WaylandPlatform::contains(EGL_PLATFORM_WAYLAND_EXT));
    assert!(!WaylandPlatform::contains(EGL_PLATFORM_GBM_KHR));
    assert!(egl_client_extensions().contains("EGL_EXT_platform_wayland"));
    assert!(egl_client_extensions().contains("EGL_KHR_platform_wayland"));
    assert!(egl_display_extensions().contains("EGL_KHR_platform_wayland"));
    assert!(egl_display_extensions().contains("EGL_KHR_fence_sync"));
    assert!(egl_display_extensions().contains("EGL_KHR_create_context_no_error"));
    assert!(
        egl_display_extensions().contains("EGL_EXT_create_context_robustness"),
        "Dawn requires robust EGL context creation"
    );
    assert!(egl_display_extensions().contains("EGL_KHR_image_base"));
    // The device family GDK/epoxy require to find eglQueryDisplayAttribEXT — advertised on both
    // the client (EGL_NO_DISPLAY) and per-display strings, matching real Mesa.
    assert!(egl_client_extensions().contains("EGL_EXT_device_base"));
    assert!(egl_client_extensions().contains("EGL_EXT_device_query"));
    assert!(egl_display_extensions().contains("EGL_EXT_device_base"));
    assert!(egl_display_extensions().contains("EGL_EXT_device_query"));
}

/// A GPU process with no window-system connection asks for the surfaceless platform and reads the
/// client extension string before it calls the driver, so the advertisement and the answer must agree.
#[test]
fn the_surfaceless_platform_is_advertised_and_backed_but_gbm_and_x11_are_refused() {
    assert!(egl_client_extensions().contains("EGL_MESA_platform_surfaceless"));
    assert!(egl_display_extensions().contains("EGL_MESA_platform_surfaceless"));
    assert!(SupportedPlatform::contains(EGL_PLATFORM_SURFACELESS_MESA));
    assert!(SupportedPlatform::contains(EGL_PLATFORM_WAYLAND_KHR));
    assert!(!SupportedPlatform::contains(EGL_PLATFORM_GBM_KHR));
    assert!(!egl_client_extensions().contains("EGL_KHR_platform_gbm"));
    assert!(!egl_client_extensions().contains("EGL_EXT_platform_x11"));
}

/// The `wl_egl_window` ABI struct is the exact 64-byte C layout the staged `libwayland-egl` allocates.
#[test]
fn wl_egl_window_layout_is_the_c_abi() {
    assert_eq!(
        core::mem::size_of::<WlEglWindow>(),
        64,
        "wl_egl_window is 64 bytes on LP64"
    );
    assert_eq!(core::mem::align_of::<WlEglWindow>(), 8);
    let s = 0xABCD_1234usize as *mut c_void;
    let w = WlEglWindow::new(s, 800, 600);
    assert_eq!(w.version, HL_WL_EGL_MAGIC);
    assert_eq!((w.width, w.height), (800, 600));
    assert_eq!(w.attached_size(), (800, 600));
}

#[test]
fn wl_egl_window_resize_updates_size_and_offset() {
    let mut w = WlEglWindow::new(core::ptr::null_mut(), 100, 100);
    w.resize(640, 480, 3, 4);
    assert_eq!((w.width, w.height, w.dx, w.dy), (640, 480, 3, 4));
    assert_eq!(w.attached_size(), (640, 480));
}

/// `eglCreateWindowSurface` reads OUR magic `wl_egl_window` for size + the app's `wl_surface`.
#[test]
fn parse_native_window_reads_the_magic_wl_egl_window() {
    let surf = 0x7777_0000usize as *mut c_void;
    let win = WlEglWindow::new(surf, 1024, 768);
    let info = unsafe { WlWindowInfo::parse(&win as *const _ as *const c_void) };
    assert_eq!(
        info,
        WlWindowInfo {
            width: 1024,
            height: 768,
            wl_surface: 0x7777_0000
        }
    );
}

/// A REAL `libwayland-egl` `wl_egl_window` (the bundled/system one Chrome/ANGLE hands us): the stable
/// `wayland-egl-backend.h` ABI is `intptr_t version; int width, height, dx, dy; struct wl_surface*;`, so
/// the size lives at offset 8/12 and the wrapped surface at offset 24 — NOT at offset 0/4. The fallback
/// must read those real fields (a regression guard for the 3×256 Chrome window: `version`=3 must NOT be
/// read as width).
#[test]
fn parse_native_window_reads_the_real_libwayland_egl_window() {
    // Mirror the real ABI in a byte buffer: version(3)@0, width(800)@8, height(600)@12, dx@16, dy@20,
    // surface(ptr)@24. `#[repr(C)]` so the field offsets are exactly the C ABI's.
    #[repr(C)]
    struct MesaWlEglWindow {
        version: isize,
        width: i32,
        height: i32,
        dx: i32,
        dy: i32,
        surface: *mut c_void,
    }
    let win = MesaWlEglWindow {
        version: 3, // WL_EGL_WINDOW_VERSION — the value the old heuristic misread as width=3
        width: 800,
        height: 600,
        dx: 0,
        dy: 0,
        surface: 0x5150_0000usize as *mut c_void,
    };
    let info = unsafe { WlWindowInfo::parse(&win as *const _ as *const c_void) };
    assert_eq!(
        (info.width, info.height, info.wl_surface),
        (800, 600, 0x5150_0000)
    );
    // A null window is the clamped default.
    let d = unsafe { WlWindowInfo::parse(core::ptr::null()) };
    assert_eq!((d.width, d.height), (256, 256));
}

/// The readback→shm convert flips vertically and packs XRGB8888 little-endian ([B,G,R,X]).
#[test]
fn rgba_to_xrgb_flips_and_reorders() {
    // 1x2 image: bottom row red, top row green (GL bottom-left order: row0 = bottom).
    let rgba = [
        /*row0 bottom, red*/ 255, 0, 0, 255, /*row1 top, green*/ 0, 255, 0, 255,
    ];
    let out = rgba_to_xrgb8888(&rgba, 1, 2);
    // top-left output row0 is the GL TOP row (green) → [B,G,R,X] = [0,255,0,255].
    assert_eq!(&out[0..4], &[0, 255, 0, 255]);
    // output row1 is the GL BOTTOM row (red) → [0,0,255,255].
    assert_eq!(&out[4..8], &[0, 0, 255, 255]);
}

/// Binds use the DISCOVERED registry name (not an assumed constant), and require wl_shm.
#[test]
fn binds_use_discovered_registry_names() {
    let mut w = blank();
    w.rx.extend_from_slice(&global_event(7, "wl_compositor", 5));
    w.rx.extend_from_slice(&global_event(9, "wl_shm", 1));
    w.rx.extend_from_slice(&global_event(4, "xdg_wm_base", 2));
    w.dispatch_pending().unwrap();
    assert_eq!(w.globals.len(), 3);

    assert!(w.bind_discovered("wl_shm", 1, OBJ_SHM));
    assert_eq!(
        &w.tx[0..4],
        &OBJ_REGISTRY.to_le_bytes(),
        "bind targets wl_registry"
    );
    assert_eq!(
        &w.tx[8..12],
        &9u32.to_le_bytes(),
        "bind must use the discovered name"
    );

    assert!(
        !w.bind_discovered("wl_seat", 1, 99),
        "a missing interface is not bound"
    );
}

/// A compositor missing `wl_shm` fails discovery loudly (no fake present).
#[test]
fn missing_shm_is_a_missing_global() {
    let mut w = blank();
    w.rx.extend_from_slice(&global_event(1, "wl_compositor", 4));
    w.rx.extend_from_slice(&global_event(2, "xdg_wm_base", 1));
    w.dispatch_pending().unwrap();
    assert_eq!(
        w.discover_and_bind_after_sync(),
        Err(WlError::MissingGlobal)
    );
}

/// The configure ack echoes the RECEIVED serial (not an invented `1`).
#[test]
fn ack_configure_echoes_received_serial() {
    let mut w = blank();
    let serial = 4242u32;
    let mut msg = Vec::new();
    msg.extend_from_slice(&OBJ_XDG_SURFACE.to_le_bytes());
    msg.extend_from_slice(&(12u32 << 16).to_le_bytes());
    msg.extend_from_slice(&serial.to_le_bytes());
    w.rx.extend_from_slice(&msg);
    w.dispatch_pending().unwrap();
    assert_eq!(w.configure_serial, Some(serial));
    w.wmsg(OBJ_XDG_SURFACE, 4, &[w.configure_serial.unwrap()]);
    let n = w.tx.len();
    assert_eq!(
        &w.tx[n - 4..n],
        &serial.to_le_bytes(),
        "ack_configure must echo the received serial"
    );
}

/// `wl_display.error` is surfaced as a protocol failure, not swallowed.
#[test]
fn display_error_is_reported_as_protocol_failure() {
    let mut w = blank();
    let mut msg = Vec::new();
    msg.extend_from_slice(&OBJ_DISPLAY.to_le_bytes());
    msg.extend_from_slice(&(16u32 << 16).to_le_bytes());
    msg.extend_from_slice(&OBJ_WL_SURFACE.to_le_bytes());
    msg.extend_from_slice(&3u32.to_le_bytes());
    w.rx.extend_from_slice(&msg);
    assert_eq!(
        w.dispatch_pending(),
        Err(WlError::Protocol {
            object: OBJ_WL_SURFACE,
            code: 3
        })
    );
}

/// `commit` on a not-ready session is an honest disconnect, never a fake success.
#[test]
fn commit_without_handshake_fails() {
    let mut w = blank();
    let g = Geometry::backing(2, 2);
    let px = vec![0u8; 2 * 2 * 4];
    assert_eq!(w.commit(&px, &g), Err(WlError::Disconnected));
}

#[test]
fn geometry_full_size_is_not_sent() {
    let g = Geometry::backing(100, 100);
    assert!(!g.should_send());
    let g2 = Geometry {
        backing_w: 100,
        backing_h: 100,
        logical_w: 80,
        logical_h: 60,
        geom_x: 10,
        ..Default::default()
    };
    assert!(g2.should_send());
}

impl Wayland {
    /// Test helper: run only the bind half of `discover_and_bind` (globals already dispatched).
    fn discover_and_bind_after_sync(&mut self) -> WlResult<()> {
        if !self.bind_discovered("wl_compositor", 4, OBJ_COMPOSITOR)
            || !self.bind_discovered("wl_shm", 1, OBJ_SHM)
            || !self.bind_discovered("xdg_wm_base", 1, OBJ_XDG_WM_BASE)
        {
            return Err(WlError::MissingGlobal);
        }
        Ok(())
    }
}
