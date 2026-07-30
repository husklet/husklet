//! xdg-shell and seat conformance: what a toplevel is told about the compositor, and whether a mapped
//! window can actually receive keyboard input.

use super::*;

#[test]
fn a_toplevel_learns_the_window_management_operations_the_compositor_performs() {
    // GTK4/Qt/Chrome read `wm_capabilities` to decide which window controls to offer. Claiming an
    // operation the compositor drops (`window_menu`) gives the user a control that does nothing.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(5);
    let surface = compositor.create_surface(&fixture.qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &fixture.qh, ());
    let _toplevel = xdg.get_toplevel(&fixture.qh, ());
    surface.commit();
    fixture.pump();

    assert!(
        !fixture.app.configures.is_empty(),
        "the toplevel never received an xdg_surface.configure"
    );
    let capabilities = fixture
        .app
        .capabilities
        .clone()
        .expect("xdg_toplevel.wm_capabilities was never sent");
    for honoured in [
        xdg_toplevel::WmCapabilities::Maximize,
        xdg_toplevel::WmCapabilities::Minimize,
        xdg_toplevel::WmCapabilities::Fullscreen,
    ] {
        assert!(
            capabilities.contains(&(honoured as u32)),
            "{honoured:?} is honoured but was not advertised: {capabilities:?}"
        );
    }
    assert!(
        !capabilities.contains(&(xdg_toplevel::WmCapabilities::WindowMenu as u32)),
        "window_menu is a no-op and must not be advertised: {capabilities:?}"
    );
}

#[test]
fn a_mapped_toplevel_receives_the_keymap_and_keyboard_enter() {
    // Without `wl_keyboard.keymap` + `enter` a client cannot interpret or accept a single keystroke.
    // The compositor gives keyboard focus to a toplevel when its first buffer maps.
    let mut fixture = Fixture::new();
    let compositor: WlCompositor = fixture.bind(4);
    let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(5);
    let shm: wl_shm::WlShm = fixture.bind(1);
    let seat: wl_seat::WlSeat = fixture.bind(5);
    fixture.pump();
    let keyboard = seat.get_keyboard(&fixture.qh, ());

    let surface = compositor.create_surface(&fixture.qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &fixture.qh, ());
    let _toplevel = xdg.get_toplevel(&fixture.qh, ());
    surface.commit();
    fixture.pump();

    let buffer = fixture.buffer(&shm, 8, 8);
    surface.attach(Some(&buffer), 0, 0);
    surface.damage(0, 0, 8, 8);
    surface.commit();
    fixture.pump();

    assert_eq!(
        fixture.state.engine.scene.seat().keyboard_focus,
        Some(crate::scene::model::SurfaceId(1)),
        "the mapped toplevel never became the keyboard focus"
    );
    let capabilities = fixture
        .app
        .seat_capabilities
        .expect("wl_seat.capabilities was never sent");
    assert_eq!(
        capabilities & u32::from(wl_seat::Capability::Keyboard),
        u32::from(wl_seat::Capability::Keyboard),
        "the seat does not advertise a keyboard, so the keymap could not be built"
    );
    let (format, size) = fixture
        .app
        .keymap
        .expect("wl_keyboard.keymap was never sent");
    assert_eq!(format, u32::from(wl_keyboard::KeymapFormat::XkbV1));
    assert!(size > 0, "an empty keymap cannot be compiled by a client");
    assert_eq!(
        fixture.app.keyboard_enters,
        vec![surface.id().protocol_id()],
        "wl_keyboard.enter never named the mapped toplevel"
    );
    keyboard.release();
}
