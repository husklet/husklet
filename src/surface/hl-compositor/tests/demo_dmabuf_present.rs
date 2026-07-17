//! DEMO — `dmabuf_present` (the ACCELERATED present path: `zwp_linux_dmabuf_v1`).
//!
//! Real toolkits (GTK/Qt EGL) and Chrome do NOT present through `wl_shm` — they render into a GPU
//! buffer and hand the compositor a `zwp_linux_dmabuf_v1` (dmabuf) `wl_buffer`. This demo proves the
//! adapter's dmabuf story end to end, HONESTLY, on this headless SOFTWARE backend:
//!
//!  1. **Advertisement is truthful.** The software adapter advertises `zwp_linux_dmabuf_v1` v3, which
//!     does not claim a DRM main device it does not own. This demo asserts the exact
//!     format table it must expose so a probing toolkit gets a correct answer: `ARGB8888` + `XRGB8888`,
//!     both with the `LINEAR` modifier.
//!
//!  2. **Real dmabuf fd import works — exact pixels.** There is no GPU here, but a `LINEAR` dmabuf is
//!     plain byte-linear CPU memory: the client backs the plane with a real file fd, and the compositor
//!     `pread`s that fd and unpacks the pixels (`read_dmabuf_rgba`). This is a GENUINE fd import (the
//!     bytes come off the client's plane fd, not a fabricated copy), so the composited frame matches the
//!     dmabuf's pixels EXACTLY — asserted here pixel-for-pixel, including the honoured alpha byte, exactly
//!     like the `wl_shm` demos.
//!
//! What this demo does NOT claim: it does not fake a GPU/zero-copy present. The import is a real CPU copy
//! of a real LINEAR buffer — the truthful capability of a software presenter. A client that offers a
//! tiled/GPU-modifier buffer (which a no-GPU backend cannot detile) is rejected at import and falls back
//! to `wl_shm`; that rejection path is out of scope for this positive demo.

mod common;
use common::*;

use std::io::Write as _;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{Flags, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::{self, ZwpLinuxDmabufV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

// DRM fourcc codes carried on the dmabuf wire (little-endian 4CC), and the LINEAR modifier.
const DRM_FORMAT_ARGB8888: u32 = 0x3432_5241; // 'AR24'
const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258; // 'XR24'
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

const W: i32 = 32;
const H: i32 = 24;

// Four quadrant colors (RGBA). Distinct channels catch a wrong swizzle; the bottom-right's non-opaque
// alpha (0x44) catches an x-vs-a mishandling (ARGB honours the 4th byte).
const TL: [u8; 4] = [0xFF, 0x00, 0x00, 0xFF]; // top-left    red
const TR: [u8; 4] = [0x00, 0xFF, 0x00, 0xFF]; // top-right   green
const BL: [u8; 4] = [0x00, 0x00, 0xFF, 0xFF]; // bottom-left blue
const BR: [u8; 4] = [0x11, 0x22, 0x33, 0x44]; // bottom-right distinct + translucent

/// The expected composited RGBA at `(x, y)` — the quadrant color (alpha honoured, since ARGB8888).
fn expected(x: i32, y: i32) -> [u8; 4] {
    match (x < W / 2, y < H / 2) {
        (true, true) => TL,
        (false, true) => TR,
        (true, false) => BL,
        (false, false) => BR,
    }
}

/// A tight `W`×`H` ARGB8888 canvas (memory order `[B, G, R, A]`) painted with the four quadrants.
fn argb_quadrants() -> Vec<u8> {
    let mut px = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let [r, g, b, a] = expected(x, y);
            let i = ((y * W + x) * 4) as usize;
            px[i] = b;
            px[i + 1] = g;
            px[i + 2] = r;
            px[i + 3] = a;
        }
    }
    px
}

struct App {
    surface: WlSurface,
    buffer: Option<WlBuffer>,
    configured: bool,
    /// `(fourcc, modifier)` pairs received from the v3 `modifier` advertisement events.
    formats_v3: Vec<(u32, u64)>,
}

#[test]
fn dmabuf_present() {
    let h = Harness::start("dmabuf_present");

    let conn = Connection::connect_to_env().expect("connect_to_env");
    let (globals, mut queue) = registry_queue_init::<App>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");

    // Bind at v3 so the server sends the `format`/`modifier` advertisement on bind (a v4+ binder receives
    // a feedback object instead). This SAME object also creates the dmabuf buffer below.
    let dmabuf_v3: ZwpLinuxDmabufV1 = globals
        .bind(&qh, 3..=3, ())
        .expect("zwp_linux_dmabuf_v1 (v3) — adapter must advertise it");

    // Back the dmabuf plane with a real, byte-linear file fd holding the ARGB quadrant pixels. A LINEAR
    // dmabuf is exactly this: CPU-readable linear memory. The kernel dups the fd across the socket to the
    // compositor, which `pread`s it at commit — a genuine fd import.
    let pixels = argb_quadrants();
    let stride = W * 4;
    let path = h.runtime_dir.join("client-dmabuf.bin");
    let mut backing = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("dmabuf backing file");
    backing.write_all(&pixels).expect("write dmabuf pixels");
    backing.flush().unwrap();
    let _ = std::fs::remove_file(&path); // unlink; the fd stays valid

    let params: ZwpLinuxBufferParamsV1 = dmabuf_v3.create_params(&qh, ());
    // Single plane, LINEAR (modifier hi/lo = 0), tight stride, offset 0.
    params.add(backing.as_fd(), 0, 0, stride as u32, 0, 0);
    let buffer: WlBuffer = params.create_immed(W, H, DRM_FORMAT_ARGB8888, Flags::empty(), &qh, ());

    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("demo-dmabuf-present".to_string());
    surface.commit();

    let mut app = App {
        surface: surface.clone(),
        buffer: Some(buffer.clone()),
        configured: false,
        formats_v3: Vec::new(),
    };

    // Drive the map handshake: on configure the dmabuf buffer is attached + committed.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.configured {
        assert!(Instant::now() < deadline, "toplevel never configured");
        queue
            .blocking_dispatch(&mut app)
            .expect("dispatch configure");
    }

    // Poll the presenter for the composited frame (this roundtrips repeatedly, so by the time it returns
    // the early advertisement + feedback events have all been delivered too).
    let frame = pump_until(&mut queue, &mut app, &h.captures, 5, |f| {
        f.width == W && f.height == H && f.pixel(1, 1).is_some()
    })
    .expect("dmabuf-backed frame never composited (real fd import failed?)");

    // ---- (1) advertisement is truthful: LINEAR ARGB8888 + XRGB8888 are exposed ----
    assert!(
        app.formats_v3
            .contains(&(DRM_FORMAT_ARGB8888, DRM_FORMAT_MOD_LINEAR)),
        "adapter must advertise ARGB8888 LINEAR; got {:?}",
        app.formats_v3
    );
    assert!(
        app.formats_v3
            .contains(&(DRM_FORMAT_XRGB8888, DRM_FORMAT_MOD_LINEAR)),
        "adapter must advertise XRGB8888 LINEAR; got {:?}",
        app.formats_v3
    );

    // ---- (2) real dmabuf fd import: the composited frame matches the buffer EXACTLY ----
    assert_eq!(frame.width, W);
    assert_eq!(frame.height, H);
    for y in 0..H {
        for x in 0..W {
            assert_eq!(
                frame.pixel(x, y).unwrap(),
                expected(x, y),
                "dmabuf pixel ({x},{y}) must match the imported buffer exactly"
            );
        }
    }
    save_frame("dmabuf_present", &frame);

    // Keep protocol objects alive until here (dropping mid-test would unmap the surface).
    let _ = (
        &params, &buffer, &toplevel, &xdg, &surface, &dmabuf_v3, &backing,
    );
    std::mem::forget(toplevel);
    std::mem::forget(xdg);

    h.shutdown();
}

// ---------- Dispatch plumbing ----------

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<XdgWmBase, ()> for App {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        e: <XdgWmBase as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for App {
    fn event(
        app: &mut Self,
        xdg: &XdgSurface,
        e: <XdgSurface as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            if !app.configured {
                if let Some(buffer) = &app.buffer {
                    app.surface.attach(Some(buffer), 0, 0);
                    app.surface.damage(0, 0, W, H);
                    app.surface.commit();
                }
                app.configured = true;
            }
        }
    }
}
impl Dispatch<ZwpLinuxDmabufV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwpLinuxDmabufV1,
        e: <ZwpLinuxDmabufV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match e {
            zwp_linux_dmabuf_v1::Event::Format { format } => {
                // v1 `format` (no modifier) — treat as an implicit LINEAR entry.
                app.formats_v3.push((format, DRM_FORMAT_MOD_LINEAR));
            }
            zwp_linux_dmabuf_v1::Event::Modifier {
                format,
                modifier_hi,
                modifier_lo,
            } => {
                let modifier = ((modifier_hi as u64) << 32) | modifier_lo as u64;
                app.formats_v3.push((format, modifier));
            }
            _ => {}
        }
    }
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for App {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(
    WlCompositor,
    WlSurface,
    WlBuffer,
    XdgToplevel,
    ZwpLinuxBufferParamsV1
);
