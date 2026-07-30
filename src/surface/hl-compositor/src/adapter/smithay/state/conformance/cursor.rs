//! Cursor conformance: both mechanisms the compositor advertises must reach the host cursor port.
//!
//! `wp_cursor_shape_device_v1.set_shape` names a themed shape; `wl_pointer.set_cursor` hands over a
//! surface plus a hotspot and expects its pixels drawn. Recording the shape NAME is not honouring either:
//! what the user sees depends on the request reaching [`Windows::set_cursor`], which is what these assert.

use super::*;
use crate::scene::port::{CursorShape, HostCursor};

/// A mapped 100×80 toplevel with pointer focus, so the client owns a valid `wl_pointer.enter` serial —
/// the token both `set_cursor` and `set_shape` must present.
struct Focused {
    fixture: Fixture,
    compositor: WlCompositor,
    shm: wl_shm::WlShm,
    seat: wl_seat::WlSeat,
    pointer: wl_pointer::WlPointer,
}

impl Focused {
    fn new() -> Focused {
        let mut fixture = Fixture::new();
        let compositor: WlCompositor = fixture.bind(4);
        let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(5);
        let shm: wl_shm::WlShm = fixture.bind(1);
        let seat: wl_seat::WlSeat = fixture.bind(8);
        fixture.pump();
        let pointer = seat.get_pointer(&fixture.qh, ());
        let surface = compositor.create_surface(&fixture.qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &fixture.qh, ());
        let _toplevel = xdg.get_toplevel(&fixture.qh, ());
        surface.commit();
        fixture.pump();
        let buffer = fixture.buffer(&shm, 100, 80);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, 100, 80);
        surface.commit();
        fixture.pump();
        fixture.state.inject_pointer_motion(20.0, 30.0);
        fixture.pump();
        Focused {
            fixture,
            compositor,
            shm,
            seat,
            pointer,
        }
    }

    fn enter_serial(&self) -> u32 {
        self.fixture
            .app
            .last_enter_serial
            .expect("the client never received a wl_pointer.enter serial")
    }

    fn host_cursor(&self) -> Option<HostCursor> {
        self.fixture
            .state
            .observations
            .lock()
            .unwrap()
            .host_cursor
            .clone()
    }
}

#[test]
fn a_named_shape_reaches_the_host_as_that_shape() {
    // Chrome/Ozone and modern GTK/Qt set the cursor exclusively this way. Decoding the name into
    // `Observations` and stopping there leaves the host pointer an arrow over every text field.
    let mut focused = Focused::new();
    let manager: wp_cursor_shape_manager_v1::WpCursorShapeManagerV1 = focused.fixture.bind(1);
    let device = manager.get_pointer(&focused.pointer, &focused.fixture.qh, ());
    let serial = focused.enter_serial();

    device.set_shape(serial, wp_cursor_shape_device_v1::Shape::Text);
    focused.fixture.pump();

    assert_eq!(
        focused.host_cursor(),
        Some(HostCursor::Shape(CursorShape::Text)),
        "the themed shape never reached the host cursor port"
    );
}

#[test]
fn a_client_cursor_surface_reaches_the_host_as_its_pixels_and_hotspot() {
    // `wl_pointer.set_cursor` promises the client's OWN image is drawn. The request and the cursor
    // surface's buffer are separate events; neither alone is a drawable cursor, and the request arrives
    // first here — the order a toolkit that pre-creates its cursor surface uses.
    let mut focused = Focused::new();
    let cursor = focused.compositor.create_surface(&focused.fixture.qh, ());
    let serial = focused.enter_serial();

    focused.pointer.set_cursor(serial, Some(&cursor), 4, 6);
    focused.fixture.pump();
    assert_eq!(
        focused.host_cursor(),
        None,
        "a cursor surface with no committed buffer is not yet a drawable cursor"
    );

    let buffer = focused.fixture.buffer(&focused.shm, 24, 24);
    cursor.attach(Some(&buffer), 0, 0);
    cursor.damage(0, 0, 24, 24);
    cursor.commit();
    focused.fixture.pump();

    let Some(HostCursor::Image(image)) = focused.host_cursor() else {
        panic!(
            "the client's cursor pixels never reached the host: {:?}",
            focused.host_cursor()
        );
    };
    assert_eq!((image.width, image.height), (24, 24));
    assert_eq!(image.hotspot, (4, 6));
    assert_eq!(image.scale, 1);
    // The fixture's buffer is a uniform 0x40 in every channel, so this asserts the whole plane arrived
    // (channel order is asserted by the deposit path's own tests).
    assert_eq!(image.rgba.len(), 24 * 24 * 4);
    assert!(image.rgba.iter().all(|&byte| byte == 0x40));

    // Withdrawing the image withdraws the cursor rather than stranding the last picture on screen.
    cursor.attach(None, 0, 0);
    cursor.commit();
    focused.fixture.pump();
    assert_eq!(focused.host_cursor(), Some(HostCursor::Hidden));
    drop(focused.seat);
}

#[test]
fn hiding_the_cursor_reaches_the_host() {
    // `wl_pointer.set_cursor` with a null surface — what a full-screen video player or a game does.
    let mut focused = Focused::new();
    let serial = focused.enter_serial();
    focused.pointer.set_cursor(serial, None, 0, 0);
    focused.fixture.pump();
    assert_eq!(focused.host_cursor(), Some(HostCursor::Hidden));
}
