//! `wl_pointer` conformance: enter, motion, button, and the whole axis family in wire order.
//!
//! The compositor advertises `wl_seat` v9, so it claims `wl_pointer` v8 — `axis_source`, `axis_value120`
//! and `axis_stop`. These assert the events a client actually reads, including their grouping inside a
//! `wl_pointer.frame`: a client applies everything between two frames as one atomic scroll update, so an
//! `axis` that lands in the wrong frame — or a source that never arrives — is a real scrolling bug.

use super::*;
use crate::adapter::smithay::InputCommand;

const BTN_LEFT: u32 = 0x110;

/// A mapped 100×80 toplevel plus a bound `wl_pointer` at v8.
struct Pointed {
    fixture: Fixture,
    pointer: wl_pointer::WlPointer,
}

impl Pointed {
    fn new() -> Pointed {
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
        drop(surface);
        Pointed { fixture, pointer }
    }

    /// Everything the client read since the last call, then reset — one scroll gesture per assertion.
    fn drain(&mut self) -> Vec<PointerWire> {
        self.fixture.pump();
        std::mem::take(&mut self.fixture.app.pointer_events)
    }
}

#[test]
fn a_pointer_enters_moves_and_presses_on_the_surface_under_the_cursor() {
    let mut pointed = Pointed::new();
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerMotion { x: 30.0, y: 40.0 });
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerMotion { x: 31.0, y: 42.0 });
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        });
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: false,
        });
    let wire = pointed.drain();

    // The toplevel roots its own tree at (0, 0), so root-local IS surface-local here.
    assert_eq!(
        wire.first(),
        Some(&PointerWire::Enter),
        "the pointer never entered the surface under it: {wire:?}"
    );
    assert_eq!(
        pointed.fixture.app.pointer_enters.first(),
        Some(&(30.0, 40.0)),
        "wl_pointer.enter carried the wrong surface-local coordinate"
    );
    assert!(
        wire.contains(&PointerWire::Motion(31.0, 42.0)),
        "the second move was never delivered as wl_pointer.motion: {wire:?}"
    );
    let pressed = u32::from(wl_pointer::ButtonState::Pressed);
    let released = u32::from(wl_pointer::ButtonState::Released);
    let buttons: Vec<PointerWire> = wire
        .iter()
        .copied()
        .filter(|event| matches!(event, PointerWire::Button(..)))
        .collect();
    assert_eq!(
        buttons,
        vec![
            PointerWire::Button(BTN_LEFT, pressed),
            PointerWire::Button(BTN_LEFT, released)
        ],
        "the press/release pair did not reach the client as BTN_LEFT: {wire:?}"
    );
    assert_eq!(
        wire.last(),
        Some(&PointerWire::Frame),
        "the last update was never closed with a wl_pointer.frame: {wire:?}"
    );
    pointed.pointer.release();
}

#[test]
fn a_wheel_scroll_carries_its_source_and_its_detents_inside_one_frame() {
    let mut pointed = Pointed::new();
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerMotion { x: 50.0, y: 50.0 });
    let _ = pointed.drain();
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerAxisDiscrete {
            horizontal: 0.0,
            vertical: 15.0,
            h120: 0,
            v120: 120,
        });
    let wire = pointed.drain();

    let vertical = u32::from(wl_pointer::Axis::VerticalScroll);
    assert_eq!(
        wire,
        vec![
            PointerWire::Source(u32::from(wl_pointer::AxisSource::Wheel)),
            PointerWire::Value120(vertical, 120),
            PointerWire::Axis(vertical, 15.0),
            PointerWire::Frame,
        ],
        "a wheel detent must arrive as source + value120 + value in ONE frame: {wire:?}"
    );
    pointed.pointer.release();
}

#[test]
fn a_finger_scroll_ends_with_an_axis_stop() {
    // `wl_pointer` requires a finger-source scroll to terminate with `axis_stop`; a client implementing
    // kinetic scrolling scrolls forever without it.
    let mut pointed = Pointed::new();
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerMotion { x: 50.0, y: 50.0 });
    let _ = pointed.drain();
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerAxisFinger {
            horizontal: 0.0,
            vertical: 7.5,
        });
    pointed
        .fixture
        .state
        .apply_input(InputCommand::PointerAxisStop {
            horizontal: false,
            vertical: true,
        });
    let wire = pointed.drain();

    let vertical = u32::from(wl_pointer::Axis::VerticalScroll);
    let finger = u32::from(wl_pointer::AxisSource::Finger);
    assert_eq!(
        wire,
        vec![
            PointerWire::Source(finger),
            PointerWire::Axis(vertical, 7.5),
            PointerWire::Frame,
            PointerWire::Source(finger),
            PointerWire::Stop(vertical),
            PointerWire::Frame,
        ],
        "a finger scroll must be tagged `finger` and terminated by axis_stop: {wire:?}"
    );
    assert!(
        !wire.contains(&PointerWire::Value120(vertical, 0)),
        "a trackpad scroll has no detents and must not claim value120: {wire:?}"
    );
    pointed.pointer.release();
}
