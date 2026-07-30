//! `xdg_popup` conformance: the placement bytes a menu is told, submenu nesting, the reposition token
//! handshake, and grab dismissal.
//!
//! Every assertion here reads the wire: `xdg_popup.configure(x, y, width, height)` is the ONLY thing a
//! toolkit learns about where its menu went, so a placement that is right internally but wrong on the wire
//! is a menu drawn in the wrong place. The composed frame origin is asserted alongside it, because that is
//! what the presenter actually routes the popup's drawable to.

use super::*;

use crate::scene::model::OutputId;
use crate::scene::port::PresentTiming;

const TL: (i32, i32) = (300, 200);

/// A mapped toplevel plus the objects needed to hang popups off it.
struct Menus {
    fixture: Fixture,
    wm_base: xdg_wm_base::XdgWmBase,
    compositor: WlCompositor,
    shm: wl_shm::WlShm,
    seat: wl_seat::WlSeat,
    root: xdg_surface::XdgSurface,
}

impl Menus {
    /// Bind the shell globals and map a `TL`-sized toplevel, leaving it committed with a buffer.
    fn open() -> Menus {
        let mut fixture = Fixture::new();
        let compositor: WlCompositor = fixture.bind(4);
        let wm_base: xdg_wm_base::XdgWmBase = fixture.bind(3);
        let shm: wl_shm::WlShm = fixture.bind(1);
        let seat: wl_seat::WlSeat = fixture.bind(5);
        let surface = compositor.create_surface(&fixture.qh, ());
        let root = wm_base.get_xdg_surface(&surface, &fixture.qh, ());
        let _toplevel = root.get_toplevel(&fixture.qh, ());
        surface.commit();
        fixture.pump();
        let buffer = fixture.buffer(&shm, TL.0, TL.1);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, TL.0, TL.1);
        surface.commit();
        fixture.pump();
        Menus {
            fixture,
            wm_base,
            compositor,
            shm,
            seat,
            root,
        }
    }

    /// An `xdg_positioner` with the full anchor/gravity/offset state a toolkit sets.
    fn positioner(
        &mut self,
        anchor_rect: (i32, i32, i32, i32),
        size: (i32, i32),
        anchor: xdg_positioner::Anchor,
        gravity: xdg_positioner::Gravity,
        offset: (i32, i32),
        adjustment: xdg_positioner::ConstraintAdjustment,
    ) -> xdg_positioner::XdgPositioner {
        let positioner = self.wm_base.create_positioner(&self.fixture.qh, ());
        positioner.set_size(size.0, size.1);
        positioner.set_anchor_rect(anchor_rect.0, anchor_rect.1, anchor_rect.2, anchor_rect.3);
        positioner.set_anchor(anchor);
        positioner.set_gravity(gravity);
        positioner.set_offset(offset.0, offset.1);
        positioner.set_constraint_adjustment(adjustment);
        positioner
    }

    /// Create a popup on `parent`, grab it, and commit a buffer of `size` so it maps and composites.
    fn popup(
        &mut self,
        parent: &xdg_surface::XdgSurface,
        positioner: &xdg_positioner::XdgPositioner,
        size: (i32, i32),
    ) -> (xdg_surface::XdgSurface, xdg_popup::XdgPopup) {
        let surface = self.compositor.create_surface(&self.fixture.qh, ());
        let xdg = self.wm_base.get_xdg_surface(&surface, &self.fixture.qh, ());
        let popup = xdg.get_popup(Some(parent), positioner, &self.fixture.qh, ());
        popup.grab(&self.seat, 0);
        surface.commit();
        self.fixture.pump();
        let buffer = self.fixture.buffer(&self.shm, size.0, size.1);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage(0, 0, size.0, size.1);
        surface.commit();
        self.fixture.pump();
        (xdg, popup)
    }

    /// Root-space origins of the frames the scene would submit for the toplevel's tree, keyed by role.
    fn frame_origins(&self) -> Vec<(u32, (i32, i32))> {
        let root = crate::scene::model::SurfaceId(1);
        self.fixture
            .state
            .engine
            .scene
            .compose_present_frames(root, OutputId(1), PresentTiming::fallback(0, 16_666_667))
            .unwrap_or_default()
            .into_iter()
            .map(|frame| (frame.role.0, frame.origin))
            .collect()
    }
}

#[test]
fn a_popup_is_configured_at_the_geometry_its_positioner_resolves_to() {
    // anchor rect (100,100,20,20), anchor BottomLeft → (100,120); gravity BottomRight → no origin shift;
    // offset (7,9) ⇒ (107,129). The client is told this and nothing else about its placement.
    let mut menus = Menus::open();
    let positioner = menus.positioner(
        (100, 100, 20, 20),
        (40, 30),
        xdg_positioner::Anchor::BottomLeft,
        xdg_positioner::Gravity::BottomRight,
        (7, 9),
        xdg_positioner::ConstraintAdjustment::None,
    );
    let root = menus.root.clone();
    menus.popup(&root, &positioner, (40, 30));

    assert_eq!(
        menus.fixture.app.popup_geometry(),
        Some((107, 129, 40, 30)),
        "xdg_popup.configure must carry the positioner-resolved rect; events={:?}",
        menus.fixture.app.popup_events
    );
    assert_eq!(
        menus.frame_origins(),
        vec![(1, (0, 0)), (2, (107, 129))],
        "the popup's own drawable is routed to its resolved root-space origin"
    );
}

#[test]
fn a_submenu_is_configured_relative_to_its_parent_popup_and_accumulates_in_root_space() {
    // A submenu's anchor rect is relative to its PARENT POPUP's window geometry, so its configure is
    // parent-relative while the frame it is composited into accumulates the whole chain.
    let mut menus = Menus::open();
    let first = menus.positioner(
        (100, 100, 20, 20),
        (40, 30),
        xdg_positioner::Anchor::BottomLeft,
        xdg_positioner::Gravity::BottomRight,
        (7, 9),
        xdg_positioner::ConstraintAdjustment::None,
    );
    let root = menus.root.clone();
    let (menu, _menu_popup) = menus.popup(&root, &first, (40, 30));

    let second = menus.positioner(
        (30, 20, 4, 4),
        (24, 18),
        xdg_positioner::Anchor::BottomRight,
        xdg_positioner::Gravity::BottomRight,
        (2, 3),
        xdg_positioner::ConstraintAdjustment::None,
    );
    menus.popup(&menu, &second, (24, 18));

    // (30+4, 20+4) + (2,3) = (36,27) relative to the first popup.
    assert_eq!(
        menus.fixture.app.popup_geometry(),
        Some((36, 27, 24, 18)),
        "a submenu's configure is relative to its parent popup, not to the toplevel"
    );
    assert_eq!(
        menus.frame_origins(),
        vec![(1, (0, 0)), (2, (107, 129)), (3, (143, 156))],
        "the submenu accumulates its parent chain: (107,129)+(36,27)"
    );
}

#[test]
fn a_popup_that_would_overflow_is_flipped_back_inside_its_window() {
    // A dropdown anchored at the bottom of its field would extend past the window; `flip_y` mirrors the
    // anchor edge and gravity so it opens upward instead — the behaviour every combo box relies on.
    let mut menus = Menus::open();
    let positioner = menus.positioner(
        (40, 150, 60, 20),
        (80, 120),
        xdg_positioner::Anchor::Bottom,
        xdg_positioner::Gravity::Bottom,
        (0, 0),
        xdg_positioner::ConstraintAdjustment::FlipY,
    );
    let root = menus.root.clone();
    menus.popup(&root, &positioner, (80, 120));

    let (_, y, _, height) = menus
        .fixture
        .app
        .popup_geometry()
        .expect("the flipped popup was never configured");
    // Unflipped it would open at y = 170 and reach 290, past the 200-tall window. Flipped, the anchor
    // becomes the field's TOP edge (y = 150) with upward gravity: y = 150 - 120 = 30.
    assert_eq!(
        y, 30,
        "flip_y must mirror the anchor edge AND the gravity, not merely clamp"
    );
    assert!(
        y + height <= TL.1,
        "the flipped dropdown must fit: {y}+{height} > {}",
        TL.1
    );
}

#[test]
fn a_popup_that_cannot_flip_slides_until_it_fits() {
    // With only `slide_y` permitted the popup keeps its gravity and is pushed along the axis until its
    // edge meets the constraint — it must not be silently left overflowing.
    let mut menus = Menus::open();
    let positioner = menus.positioner(
        (40, 150, 60, 20),
        (80, 120),
        xdg_positioner::Anchor::Bottom,
        xdg_positioner::Gravity::Bottom,
        (0, 0),
        xdg_positioner::ConstraintAdjustment::SlideY,
    );
    let root = menus.root.clone();
    menus.popup(&root, &positioner, (80, 120));

    let (_, y, _, height) = menus
        .fixture
        .app
        .popup_geometry()
        .expect("the slid popup was never configured");
    assert_eq!(
        y + height,
        TL.1,
        "slide_y pushes the popup's bottom edge exactly onto the constraint"
    );
}

#[test]
fn a_reposition_answers_its_token_before_describing_the_new_placement() {
    // xdg-shell v3: `reposition(positioner, token)` must be answered with `repositioned(token)` and then a
    // fresh configure. A toolkit correlates the two; the wrong order (or a dropped token) leaves a menu
    // walking a menu bar stuck at its old anchor.
    let mut menus = Menus::open();
    let first = menus.positioner(
        (100, 100, 20, 20),
        (40, 30),
        xdg_positioner::Anchor::BottomLeft,
        xdg_positioner::Gravity::BottomRight,
        (7, 9),
        xdg_positioner::ConstraintAdjustment::None,
    );
    let root = menus.root.clone();
    let (_menu, popup) = menus.popup(&root, &first, (40, 30));
    let before = menus.fixture.app.popup_events.len();

    let moved = menus.positioner(
        (10, 20, 8, 8),
        (40, 30),
        xdg_positioner::Anchor::TopLeft,
        xdg_positioner::Gravity::BottomRight,
        (1, 2),
        xdg_positioner::ConstraintAdjustment::None,
    );
    popup.reposition(&moved, 4242);
    menus.fixture.pump();

    // (10,20) anchored TopLeft, BottomRight gravity, offset (1,2) ⇒ (11,22).
    assert_eq!(
        &menus.fixture.app.popup_events[before..],
        &[
            PopupWire::Repositioned(4242),
            PopupWire::Configure(11, 22, 40, 30)
        ],
        "the token must be acknowledged first, then the new geometry described"
    );
}

#[test]
fn a_press_outside_a_grabbed_popup_dismisses_it_on_the_wire() {
    // `xdg_popup.grab` asks the compositor to dismiss the menu when the user clicks away. Without the
    // `popup_done` event the client keeps the menu open forever and swallows the click.
    let mut menus = Menus::open();
    let positioner = menus.positioner(
        (100, 100, 20, 20),
        (40, 30),
        xdg_positioner::Anchor::BottomLeft,
        xdg_positioner::Gravity::BottomRight,
        (7, 9),
        xdg_positioner::ConstraintAdjustment::None,
    );
    let root = menus.root.clone();
    menus.popup(&root, &positioner, (40, 30));
    assert!(
        !menus.fixture.app.popup_events.contains(&PopupWire::Done),
        "the popup was dismissed before any click"
    );

    // (5,5) is far outside the popup's (107,129)+(40,30) rectangle.
    menus.fixture.state.inject_pointer_motion(5.0, 5.0);
    menus.fixture.state.inject_pointer_button(0x110, true);
    menus.fixture.pump();

    assert!(
        menus.fixture.app.popup_events.contains(&PopupWire::Done),
        "an outside press must dismiss the grab; events={:?}",
        menus.fixture.app.popup_events
    );
}

#[test]
fn a_press_inside_a_grabbed_popup_keeps_it_open() {
    // The mirror of the dismissal rule: a click ON the menu is a menu activation, not a dismissal. A
    // compositor that dismisses on every press makes every menu item unclickable.
    let mut menus = Menus::open();
    let positioner = menus.positioner(
        (100, 100, 20, 20),
        (40, 30),
        xdg_positioner::Anchor::BottomLeft,
        xdg_positioner::Gravity::BottomRight,
        (7, 9),
        xdg_positioner::ConstraintAdjustment::None,
    );
    let root = menus.root.clone();
    menus.popup(&root, &positioner, (40, 30));

    // (127,144) is the centre of the popup at (107,129) sized 40x30.
    menus.fixture.state.inject_pointer_motion(127.0, 144.0);
    menus.fixture.state.inject_pointer_button(0x110, true);
    menus.fixture.pump();

    assert!(
        !menus.fixture.app.popup_events.contains(&PopupWire::Done),
        "a press inside the popup must not dismiss it; events={:?}",
        menus.fixture.app.popup_events
    );
}
