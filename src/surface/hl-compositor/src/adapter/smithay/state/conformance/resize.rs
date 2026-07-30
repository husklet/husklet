//! Interactive resize: `xdg_toplevel.resize` driven by a real pointer drag.
//!
//! A GTK4 or Qt client with client-side decorations resizes ONLY this way — it has no other request for
//! "make me bigger". The contract these assert is the one such a client depends on: each motion during the
//! drag produces an `xdg_toplevel.configure` carrying the new size AND the `resizing` state, and the drag's
//! release produces a final configure with `resizing` cleared.

use super::*;
use crate::adapter::smithay::InputCommand;

const BTN_LEFT: u32 = 0x110;

/// A mapped toplevel with a pointer over it, ready to start a resize drag.
struct Draggable {
    fixture: Fixture,
    seat: wl_seat::WlSeat,
    pointer: wl_pointer::WlPointer,
    toplevel: xdg_toplevel::XdgToplevel,
    surface: WlSurface,
}

impl Draggable {
    fn new() -> Draggable {
        let mut fixture = Fixture::new();
        let compositor: WlCompositor = fixture.bind(4);
        let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(5);
        let shm: wl_shm::WlShm = fixture.bind(1);
        let seat: wl_seat::WlSeat = fixture.bind(8);
        fixture.pump();
        let pointer = seat.get_pointer(&fixture.qh, ());
        let surface = compositor.create_surface(&fixture.qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &fixture.qh, ());
        let toplevel = xdg.get_toplevel(&fixture.qh, ());
        surface.commit();
        fixture.pump();
        let buffer = fixture.buffer(&shm, 100, 80);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, 100, 80);
        surface.commit();
        fixture.pump();
        Draggable {
            fixture,
            seat,
            pointer,
            toplevel,
            surface,
        }
    }

    /// Put the pointer down at `(x, y)` and ask the compositor to start a resize on `edges`.
    fn start(&mut self, x: f64, y: f64, edges: xdg_toplevel::ResizeEdge) {
        self.inject(InputCommand::PointerMotion { x, y });
        self.inject(InputCommand::PointerButton {
            button: BTN_LEFT,
            pressed: true,
        });
        let serial = self
            .fixture
            .app
            .last_button_serial
            .expect("no wl_pointer.button serial to anchor the resize to");
        self.fixture.app.toplevel_configures.clear();
        self.toplevel.resize(&self.seat, serial, edges);
        self.fixture.pump();
    }

    fn inject(&mut self, command: InputCommand) {
        self.fixture.state.apply_input(command);
        self.fixture.pump();
    }

    /// The most recent `xdg_toplevel.configure` size and whether it carried `resizing`.
    fn last_configure(&self) -> (i32, i32, bool) {
        let (width, height, states) = self
            .fixture
            .app
            .toplevel_configures
            .last()
            .expect("no xdg_toplevel.configure was sent");
        (
            *width,
            *height,
            states.contains(&(xdg_toplevel::State::Resizing as u32)),
        )
    }
}

#[test]
fn a_resize_drag_configures_the_size_the_pointer_reaches_and_clears_resizing_on_release() {
    let mut drag = Draggable::new();
    drag.start(99.0, 79.0, xdg_toplevel::ResizeEdge::BottomRight);

    let (width, height, resizing) = drag.last_configure();
    assert!(
        resizing,
        "starting an interactive resize must configure the `resizing` state"
    );
    let start = (width, height);

    drag.inject(InputCommand::PointerMotion { x: 149.0, y: 129.0 });
    let (width, height, resizing) = drag.last_configure();
    assert_eq!(
        (width, height),
        (start.0 + 50, start.1 + 50),
        "dragging the bottom-right edge by (50, 50) must configure a size 50×50 larger"
    );
    assert!(resizing, "a configure mid-drag must still carry `resizing`");

    drag.inject(InputCommand::PointerButton {
        button: BTN_LEFT,
        pressed: false,
    });
    let (width, height, resizing) = drag.last_configure();
    assert_eq!(
        (width, height),
        (start.0 + 50, start.1 + 50),
        "releasing the drag must keep the size it reached"
    );
    assert!(
        !resizing,
        "the drag ended, so the final configure must clear `resizing`"
    );
    drag.pointer.release();
}

#[test]
fn dragging_a_top_left_edge_grows_the_window_toward_the_pointer() {
    // The Left/Top edges invert the delta: moving the pointer up-left makes the window BIGGER. Getting the
    // sign wrong here is the classic interactive-resize bug (the window shrinks as you drag it open).
    let mut drag = Draggable::new();
    drag.start(1.0, 1.0, xdg_toplevel::ResizeEdge::TopLeft);
    let (start_w, start_h, _) = drag.last_configure();

    drag.inject(InputCommand::PointerMotion { x: -19.0, y: -9.0 });
    let (width, height, _) = drag.last_configure();
    assert_eq!(
        (width, height),
        (start_w + 20, start_h + 10),
        "dragging the top-left edge outward must grow the window"
    );
    drag.pointer.release();
}

#[test]
fn a_resize_drag_never_configures_below_the_size_the_client_declared_as_its_minimum() {
    // A configure smaller than the client's own `set_min_size` is one the client is entitled to ignore, so
    // the drag would appear stuck instead of clamping.
    let mut drag = Draggable::new();
    drag.toplevel.set_min_size(90, 70);
    drag.surface.commit(); // min/max size is double-buffered: it applies at the next commit
    drag.fixture.pump();
    drag.start(99.0, 79.0, xdg_toplevel::ResizeEdge::BottomRight);

    drag.inject(InputCommand::PointerMotion {
        x: -900.0,
        y: -900.0,
    });
    let (width, height, _) = drag.last_configure();
    assert_eq!(
        (width, height),
        (90, 70),
        "the configure ignored the client's declared minimum size"
    );
    drag.pointer.release();
}

#[test]
fn a_motion_during_a_resize_drag_belongs_to_the_grab_and_not_to_the_surface() {
    // A grab owns the pointer: delivering motion to the surface as well makes the client's own hover state
    // chase the resize.
    let mut drag = Draggable::new();
    drag.start(99.0, 79.0, xdg_toplevel::ResizeEdge::BottomRight);
    drag.fixture.app.pointer_events.clear();

    drag.inject(InputCommand::PointerMotion { x: 120.0, y: 100.0 });
    let motions: Vec<PointerWire> = drag
        .fixture
        .app
        .pointer_events
        .iter()
        .copied()
        .filter(|event| matches!(event, PointerWire::Motion(..)))
        .collect();
    assert!(
        motions.is_empty(),
        "motion during a resize grab reached the surface: {motions:?}"
    );
    drag.pointer.release();
}
