//! Live-socket proof for `xdg_popup` + `wl_subsurface` placement, buffer routing, and popup dismiss.
//!
//! `wayland_live_socket` proved a real client can map a toplevel and composite its buffer. This test
//! closes the gap for the two child-surface roles a real GUI toolkit relies on — the menus / dropdowns /
//! tooltips / combo-box lists that are `xdg_popup`s, and the sub-windows that are `wl_subsurface`s — end
//! to end over a real socket:
//!
//!   1. A real `wayland-client` discovers the compositor via `$WAYLAND_DISPLAY`, maps an `xdg_toplevel`,
//!      and composites a base-color buffer.
//!   2. It creates an `xdg_popup` via `xdg_surface.get_popup` with an `xdg_positioner`
//!      (anchor rect + anchor edge + gravity + offset), acks the popup's `configure`, takes an
//!      `xdg_popup.grab`, and commits a DISTINCT-color buffer. We assert the popup composited to the
//!      `PngPresenter` at the EXACT positioner-resolved placement, in its own color.
//!   3. It creates a `wl_subsurface` at a `set_position` offset (desynchronized) with ANOTHER distinct
//!      color and commits. We assert it composited at `parent + offset`, in its own color.
//!   4. The compositor is run with a host INPUT channel; a pointer press injected OUTSIDE the popup
//!      dismisses the grab — the client receives `xdg_popup.popup_done`.
//!
//! Fully headless — real socket, real wire, real composite — no DRM, no GPU, no display. Its own test
//! binary because it mutates process-global `$XDG_RUNTIME_DIR` / `$WAYLAND_DISPLAY`.

use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_compositor::adapter::smithay::{
    self, input_channel, CapturedFrame, InputCommand, PngPresenter,
};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_registry::WlRegistry,
    wl_seat::WlSeat,
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::{self, XdgPositioner},
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::XdgToplevel,
    xdg_wm_base::{self, XdgWmBase},
};

// ---- geometry / colors of the three surfaces (all distinct so a capture can be told apart) ----------
// `wl_shm` Argb8888 is 32-bit little-endian → memory bytes `[B, G, R, A]`.
const TL_W: i32 = 300;
const TL_H: i32 = 200;
const TL: [u8; 4] = [0x20, 0x20, 0xC0, 0xFF]; // R,G,B,A (blue)

const POP_W: i32 = 40;
const POP_H: i32 = 30;
const POP: [u8; 4] = [0xE0, 0x10, 0x10, 0xFF]; // red

const SUB_W: i32 = 50;
const SUB_H: i32 = 24;
const SUB: [u8; 4] = [0x10, 0xD0, 0x20, 0xFF]; // green

// Positioner: anchor rect (100,100,20,20), anchor BottomLeft → point (100,120); gravity BottomRight →
// grows down-right (no origin shift); offset (7,9). Unconstrained (well within 1920x1080), so the
// resolved geometry origin is (100+0+7, 120+0+9) = (107,129). This is where the popup MUST composite.
const ANCHOR: (i32, i32, i32, i32) = (100, 100, 20, 20);
const OFFSET: (i32, i32) = (7, 9);
const EXPECT_POP: (i32, i32) = (107, 129);

// Subsurface offset from the toplevel origin (which is (0,0)); this is where it MUST composite.
const SUB_POS: (i32, i32) = (48, 66);

/// Which `xdg_surface` a shared `Dispatch` callback belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Toplevel,
    Popup,
}

struct AppData {
    // toplevel
    tl_surface: WlSurface,
    tl_buffer: WlBuffer,
    tl_drawn: bool,
    tl_released: bool,
    tl_frame_done: bool,
    // popup
    pop_surface: WlSurface,
    pop_buffer: WlBuffer,
    pop_drawn: bool,
    pop_done: bool,
}

#[test]
fn popup_and_subsurface_composite_at_placed_positions_and_grab_dismisses() {
    // ---- 1. Private XDG_RUNTIME_DIR + start the compositor with a host input channel ------------------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-popup-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET");

    let png_dir = runtime_dir.join("png");
    let stop = Arc::new(AtomicBool::new(false));
    let presenter = PngPresenter::with_png_dir(png_dir.clone());
    let captures = presenter.captures();
    let (name_tx, name_rx) = mpsc::channel::<std::ffi::OsString>();
    let (input_tx, input_rx) = input_channel();

    let stop_thread = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        smithay::run_auto_with_input(presenter, stop_thread, input_rx, move |name| {
            let _ = name_tx.send(name);
        })
        .expect("compositor serve loop (run_auto_with_input)");
    });

    let socket_name = name_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("run_auto_with_input never reported a bound socket name");
    let socket_path = runtime_dir.join(&socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(
            Instant::now() < deadline,
            "discovery socket {socket_path:?} never appeared"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 2. Connect a real client and bind globals (incl. wl_subcompositor) ---------------------------
    let conn = Connection::connect_to_env().expect("connect_to_env failed");
    let (globals, mut queue) = registry_queue_init::<AppData>(&conn).expect("registry init");
    let qh = queue.handle();

    let compositor: WlCompositor = globals.bind(&qh, 1..=6, ()).expect("wl_compositor global");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm global");
    let wm_base: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base global");
    let seat: WlSeat = globals.bind(&qh, 1..=9, ()).expect("wl_seat global");
    // wl_subcompositor MUST be advertised for a toolkit to build sub-windows; before this task the
    // compositor still stood it up (via CompositorState) but never routed the subsurface into the scene.
    let subcompositor: WlSubcompositor = globals
        .bind(&qh, 1..=1, ())
        .expect("wl_subcompositor global");

    // Three shm buffers of distinct color/size (own file+pool each).
    let tl_buffer = make_buffer(&shm, &qh, &runtime_dir, "tl", TL_W, TL_H, TL);
    let pop_buffer = make_buffer(&shm, &qh, &runtime_dir, "pop", POP_W, POP_H, POP);
    let sub_buffer = make_buffer(&shm, &qh, &runtime_dir, "sub", SUB_W, SUB_H, SUB);

    // ---- 3. Map the toplevel -------------------------------------------------------------------------
    let tl_surface = compositor.create_surface(&qh, ());
    let tl_xdg = wm_base.get_xdg_surface(&tl_surface, &qh, Role::Toplevel);
    let toplevel = tl_xdg.get_toplevel(&qh, ());
    toplevel.set_title("hl-wip-popup".into());
    tl_surface.commit(); // initial empty commit → first configure

    // Popup objects are created after the toplevel is mapped (step 5); pre-create the surface handle now
    // so `AppData` is fully populated. It is not turned into a popup until `get_popup` below.
    let pop_surface = compositor.create_surface(&qh, ());

    let mut app = AppData {
        tl_surface: tl_surface.clone(),
        tl_buffer: tl_buffer.clone(),
        tl_drawn: false,
        tl_released: false,
        tl_frame_done: false,
        pop_surface: pop_surface.clone(),
        pop_buffer: pop_buffer.clone(),
        pop_drawn: false,
        pop_done: false,
    };

    let deadline = Instant::now() + Duration::from_secs(5);
    while !(app.tl_drawn && app.tl_released && app.tl_frame_done) {
        assert!(
            Instant::now() < deadline,
            "toplevel map incomplete: drawn={} released={} frame={}",
            app.tl_drawn,
            app.tl_released,
            app.tl_frame_done,
        );
        // `roundtrip` is BOUNDED: the server always answers `wl_display.sync`, so the loop returns to its
        // deadline check every cycle. `blocking_dispatch` would park forever the moment the compositor has
        // nothing further to send (it legitimately withholds a frame callback in some pacing paths), which
        // turns a failing expectation into an unbounded hang.
        queue
            .roundtrip(&mut app)
            .expect("client roundtrip (map toplevel)");
    }
    assert!(
        wait_for(&captures, |f| f.width == TL_W
            && f.pixel_is(TL_W / 2, TL_H / 2, TL)),
        "toplevel never composited its base color",
    );

    // ---- 4. Create the xdg_popup with the positioner, grab it, and commit its buffer -----------------
    let positioner: XdgPositioner = wm_base.create_positioner(&qh, ());
    positioner.set_size(POP_W, POP_H);
    positioner.set_anchor_rect(ANCHOR.0, ANCHOR.1, ANCHOR.2, ANCHOR.3);
    positioner.set_anchor(xdg_positioner::Anchor::BottomLeft);
    positioner.set_gravity(xdg_positioner::Gravity::BottomRight);
    positioner.set_offset(OFFSET.0, OFFSET.1);

    let pop_xdg = wm_base.get_xdg_surface(&pop_surface, &qh, Role::Popup);
    let popup: XdgPopup = pop_xdg.get_popup(Some(&tl_xdg), &positioner, &qh, ());
    popup.grab(&seat, 0); // explicit grab: a press outside dismisses the chain
    pop_surface.commit(); // initial empty commit → popup configure (with the resolved geometry)

    // The popup's colored buffer is attached from the popup's configure handler (see Dispatch<XdgSurface>).
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut popup_frame: Option<CapturedFrame> = None;
    while popup_frame.is_none() {
        assert!(
            Instant::now() < deadline,
            "popup never composited at its placed position"
        );
        queue.roundtrip(&mut app).ok(); // bounded pump: see the map loop above
        popup_frame = captures
            .lock()
            .unwrap()
            .iter()
            .find(|f| {
                f.width == POP_W
                    && f.height == POP_H
                    && f.pixel_is(POP_W / 2, POP_H / 2, POP)
                    && (f.x, f.y) == EXPECT_POP
            })
            .cloned();
        std::thread::sleep(Duration::from_millis(10));
    }
    let popup_frame = popup_frame.unwrap();
    // Placement: the popup composited exactly at the positioner-resolved geometry origin, in ITS color —
    // distinct from the toplevel's base color at the same texel.
    assert_eq!(
        (popup_frame.x, popup_frame.y),
        EXPECT_POP,
        "popup composited at the resolved placement"
    );
    assert_eq!(
        popup_frame.pixel(POP_W / 2, POP_H / 2).unwrap(),
        POP,
        "popup is its own color"
    );
    assert_ne!(POP, TL, "popup color is distinct from the toplevel color");

    // ---- 5. Create the wl_subsurface at a set_position offset and commit its buffer ------------------
    let sub_surface = compositor.create_surface(&qh, ());
    let subsurface: WlSubsurface = subcompositor.get_subsurface(&sub_surface, &tl_surface, &qh, ());
    subsurface.set_position(SUB_POS.0, SUB_POS.1);
    subsurface.set_desync(); // apply on the subsurface's own commit, not the parent's
    sub_surface.attach(Some(&sub_buffer), 0, 0);
    sub_surface.damage(0, 0, SUB_W, SUB_H);
    sub_surface.commit();
    tl_surface.commit(); // parent commit applies the subsurface tree state

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut sub_frame: Option<CapturedFrame> = None;
    while sub_frame.is_none() {
        assert!(
            Instant::now() < deadline,
            "subsurface never composited at parent+offset"
        );
        queue.roundtrip(&mut app).ok(); // bounded pump: see the map loop above
        sub_frame = captures
            .lock()
            .unwrap()
            .iter()
            .find(|f| {
                f.width == SUB_W
                    && f.height == SUB_H
                    && f.pixel_is(SUB_W / 2, SUB_H / 2, SUB)
                    && (f.x, f.y) == SUB_POS
            })
            .cloned();
        std::thread::sleep(Duration::from_millis(10));
    }
    let sub_frame = sub_frame.unwrap();
    assert_eq!(
        (sub_frame.x, sub_frame.y),
        SUB_POS,
        "subsurface composited at parent + set_position"
    );
    assert_eq!(
        sub_frame.pixel(SUB_W / 2, SUB_H / 2).unwrap(),
        SUB,
        "subsurface is its own color"
    );
    assert_ne!(
        SUB, TL,
        "subsurface color is distinct from the toplevel color"
    );

    // ---- 6. Dismiss the popup grab: a press OUTSIDE the popup rectangle → xdg_popup.popup_done --------
    // The popup occupies (107,129)+(40,30); (5,5) is well outside. Move the pointer there, then press.
    input_tx
        .send(InputCommand::PointerMotion { x: 5.0, y: 5.0 })
        .expect("send motion outside popup");
    input_tx
        .send(InputCommand::PointerButton {
            button: 0x110,
            pressed: true,
        })
        .expect("send press");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.pop_done {
        assert!(
            Instant::now() < deadline,
            "popup grab was never dismissed (no popup_done)"
        );
        queue
            .roundtrip(&mut app)
            .expect("client dispatch (dismiss)");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        app.pop_done,
        "client received xdg_popup.popup_done on the outside press"
    );

    // ---- 7. Shut down --------------------------------------------------------------------------------
    stop.store(true, Ordering::Relaxed);
    let _ = handle.join();
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

// ---- helpers ----------------------------------------------------------------------------------------

/// Build a `wl_shm` buffer of `w`×`h` filled with `rgba` (stored little-endian ARGB), on its own pool.
fn make_buffer(
    shm: &WlShm,
    qh: &QueueHandle<AppData>,
    dir: &std::path::Path,
    tag: &str,
    w: i32,
    h: i32,
    rgba: [u8; 4],
) -> WlBuffer {
    let [r, g, b, a] = rgba;
    let stride = w * 4;
    let size = (stride * h) as usize;
    let mut pixels = Vec::with_capacity(size);
    for _ in 0..(w * h) {
        pixels.extend_from_slice(&[b, g, r, a]); // little-endian ARGB
    }
    let path = dir.join(format!("client-{tag}.shm"));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("shm file");
    file.write_all(&pixels).expect("write shm pixels");
    file.flush().unwrap();
    let _ = std::fs::remove_file(&path); // unlink; the fd + mapping stay valid
    let pool: WlShmPool = shm.create_pool(file.as_fd(), size as i32, qh, ());
    // Leak the file fd for the life of the test (the pool keeps the mapping); dropping it here is fine
    // because the kernel keeps the mapping alive as long as the pool holds the fd.
    std::mem::forget(file);
    pool.create_buffer(0, w, h, stride, wl_shm::Format::Argb8888, qh, ())
}

/// Poll the capture log until `pred` matches some frame or a 5s deadline passes.
fn wait_for(
    captures: &Arc<std::sync::Mutex<Vec<CapturedFrame>>>,
    pred: impl Fn(&CapturedFrame) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if captures.lock().unwrap().iter().any(&pred) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

trait PixelIs {
    fn pixel_is(&self, x: i32, y: i32, rgba: [u8; 4]) -> bool;
}
impl PixelIs for CapturedFrame {
    fn pixel_is(&self, x: i32, y: i32, rgba: [u8; 4]) -> bool {
        self.pixel(x, y) == Some(rgba)
    }
}

// ------------------------- wayland-client Dispatch plumbing (client side) -------------------------

#[path = "compositor/popup_subsurface.rs"]
mod dispatch;
