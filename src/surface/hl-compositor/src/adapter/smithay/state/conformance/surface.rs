//! `wl_surface` conformance: the viewport crop contract, cursor surfaces, and input-region hit-testing.

use super::*;

#[test]
fn a_client_provided_cursor_surface_is_never_presented_as_a_window() {
    // `wl_pointer.set_cursor` hands the compositor a surface to draw as the cursor. Left roleless it
    // becomes its own window root: every cursor update composes a stray frame, and because a roleless
    // surface has no window to arm a repaint against, its frame callbacks are never released — an
    // animated cursor stalls.
    use smithay::input::pointer::CursorImageStatus;
    use smithay::input::SeatHandler;

    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let _cursor = compositor.create_surface(&fixture.qh, ());
    fixture.pump();

    let sid = crate::scene::model::SurfaceId(1);
    let wl = fixture
        .state
        .surfaces_by_id
        .get(&sid)
        .cloned()
        .expect("the client's surface reached the scene");
    let seat = fixture.state.seat.clone();
    fixture
        .state
        .cursor_image(&seat, CursorImageStatus::Surface(wl));

    assert_eq!(
        fixture.state.engine.scene.get(sid).map(|s| s.role.clone()),
        Some(crate::scene::model::SurfaceRole::Cursor)
    );
    assert!(
        fixture
            .state
            .engine
            .complete_commit(sid, true)
            .frame
            .is_none(),
        "a cursor surface must not drive a window present"
    );
}

#[test]
fn a_viewport_source_outside_the_buffer_raises_out_of_buffer() {
    // Unenforced, the presenter clamps the sample to the buffer edge and the client silently renders
    // smeared pixels. The protocol requires the error on the wp_viewport object instead.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let shm: wl_shm::WlShm = fixture.bind(1);
    let viewporter: wp_viewporter::WpViewporter = fixture.bind(1);
    let surface = compositor.create_surface(&fixture.qh, ());
    let viewport = viewporter.get_viewport(&surface, &fixture.qh, ());
    let buffer = fixture.buffer(&shm, 4, 4);

    viewport.set_source(0.0, 0.0, 8.0, 8.0);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 4, 4);
    surface.commit();
    fixture.pump();

    let error = fixture
        .protocol_error()
        .expect("an out-of-buffer source crop must raise a protocol error");
    assert_eq!(error.object_interface, "wp_viewport");
    assert_eq!(error.code, wp_viewport::Error::OutOfBuffer as u32);
}

#[test]
fn a_viewport_source_inside_the_buffer_is_honoured() {
    // The control for the check above: a legal crop+scale must survive untouched, and the scene must
    // resolve the destination size the client asked for.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let shm: wl_shm::WlShm = fixture.bind(1);
    let viewporter: wp_viewporter::WpViewporter = fixture.bind(1);
    let surface = compositor.create_surface(&fixture.qh, ());
    let viewport = viewporter.get_viewport(&surface, &fixture.qh, ());
    let buffer = fixture.buffer(&shm, 8, 8);

    viewport.set_source(2.0, 2.0, 4.0, 4.0);
    viewport.set_destination(16, 16);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 8, 8);
    surface.commit();
    fixture.pump();

    assert!(fixture.protocol_error().is_none());
    let sid = crate::scene::model::SurfaceId(1);
    let logical = fixture
        .state
        .engine
        .scene
        .get(sid)
        .and_then(|surface| surface.logical_size());
    assert_eq!(logical, Some((16, 16)));
}

#[test]
fn a_press_inside_an_input_region_hole_never_reaches_the_client() {
    // A shaped surface (rounded corners, an overlay with a cut-out) subtracts from its input region.
    // Collapsing the region to its bounding box would accept the click and steal it from whatever is
    // behind. Pointer focus must follow the region exactly.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(5);
    let shm: wl_shm::WlShm = fixture.bind(1);
    let seat: wl_seat::WlSeat = fixture.bind(5);
    fixture.pump();
    let pointer = seat.get_pointer(&fixture.qh, ());

    let surface = compositor.create_surface(&fixture.qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &fixture.qh, ());
    let _toplevel = xdg.get_toplevel(&fixture.qh, ());
    surface.commit();
    fixture.pump();

    let region = compositor.create_region(&fixture.qh, ());
    region.add(0, 0, 100, 100);
    region.subtract(40, 40, 20, 20);
    surface.set_input_region(Some(&region));
    let buffer = fixture.buffer(&shm, 100, 100);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 100, 100);
    surface.commit();
    fixture.pump();

    // Inside the subtracted hole: the surface declined input here.
    fixture.state.inject_pointer_motion(45.0, 45.0);
    fixture.pump();
    assert!(
        fixture.app.pointer_enters.is_empty(),
        "a motion inside the hole delivered wl_pointer.enter: {:?}",
        fixture.app.pointer_enters
    );

    // Outside the hole, inside the region: the client is entered at the exact surface-local point.
    fixture.state.inject_pointer_motion(10.0, 12.0);
    fixture.pump();
    assert_eq!(fixture.app.pointer_enters, vec![(10.0, 12.0)]);
    pointer.release();
}
