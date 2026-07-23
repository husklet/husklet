use super::*;
#[test]
fn popup_anchor_center_and_gravity_center() {
    let p = Positioner {
        anchor_rect: Rect::new(100, 100, 40, 20),
        size: (10, 10),
        anchor: Anchor::None,   // center of the anchor rect => (120, 110)
        gravity: Gravity::None, // popup centered on the anchor point
        constraint_adjustment: ConstraintAdjustment::NONE,
        offset: (0, 0),
    };
    // top-left = anchor(120,110) - half size(5,5) = (115, 105)
    assert_eq!(p.place(2000, 2000), Rect::new(115, 105, 10, 10));
}

#[test]
fn popup_all_gravities_grow_away_from_anchor() {
    let base = Positioner {
        anchor_rect: Rect::new(500, 500, 0, 0), // a point anchor at (500,500)
        size: (100, 60),
        anchor: Anchor::None,
        gravity: Gravity::None,
        constraint_adjustment: ConstraintAdjustment::NONE,
        offset: (0, 0),
    };
    let at = |g: Gravity| {
        let geo = Positioner { gravity: g, ..base }.place(4000, 4000);
        (geo.x, geo.y)
    };
    assert_eq!(
        at(Gravity::BottomRight),
        (500, 500),
        "grows down-right from the anchor"
    );
    assert_eq!(
        at(Gravity::TopLeft),
        (400, 440),
        "grows up-left (origin shifts by -w,-h)"
    );
    assert_eq!(at(Gravity::Top), (450, 440), "up: centered x, origin y - h");
    assert_eq!(at(Gravity::Left), (400, 470));
    assert_eq!(at(Gravity::BottomLeft), (400, 500));
    assert_eq!(at(Gravity::TopRight), (500, 440));
}

#[test]
fn popup_flip_x_mirrors_when_it_helps() {
    // Anchored at the right edge growing right (overflow); flip_x mirrors to grow left and fits.
    let p = Positioner {
        anchor_rect: Rect::new(480, 10, 10, 10),
        size: (100, 50),
        anchor: Anchor::Right,   // anchor point x = 490
        gravity: Gravity::Right, // origin x = 490 -> right = 590 > 500
        constraint_adjustment: ConstraintAdjustment {
            flip_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    // Flipped: anchor Left (x=480), gravity Left (origin x = 480 - 100 = 380).
    assert_eq!(geo.x, 380, "flip_x mirrors the popup back on-screen");
    assert!(geo.x >= 0 && geo.right() <= 500);
}

#[test]
fn popup_flip_is_declined_when_it_would_not_help() {
    // Overflow on the right, but the flipped placement ALSO overflows (left of 0): flip is not applied,
    // and with no slide/resize the popup stays at its unconstrained (overflowing) position.
    let p = Positioner {
        anchor_rect: Rect::new(50, 10, 10, 10),
        size: (600, 50),         // wider than the 500 target on either side
        anchor: Anchor::Right,   // x = 60
        gravity: Gravity::Right, // origin 60, right 660 > 500 (overflow right)
        constraint_adjustment: ConstraintAdjustment {
            flip_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    // Flipped would put origin at 60 - 600 = -540 (overflow left), so flip is declined; origin stays 60.
    assert_eq!(geo.x, 60, "a flip that doesn't help is not applied");
}

#[test]
fn popup_slide_pushes_in_from_the_far_edge() {
    let p = Positioner {
        anchor_rect: Rect::new(10, 380, 20, 20),
        size: (100, 100),
        anchor: Anchor::Bottom,   // y = 400
        gravity: Gravity::Bottom, // origin y = 400, bottom = 500 > 400
        constraint_adjustment: ConstraintAdjustment {
            slide_y: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    assert_eq!(
        geo.bottom(),
        400,
        "slide_y flushes the popup to the bottom edge"
    );
    assert_eq!(geo.h, 100, "slide preserves size");
}

#[test]
fn popup_slide_of_oversized_popup_flushes_near_edge() {
    // A popup taller than the whole target, slid, ends flush with the NEAR (top) edge per the spec.
    let p = Positioner {
        anchor_rect: Rect::new(10, 300, 20, 20),
        size: (50, 900),
        anchor: Anchor::Bottom,   // y = 320
        gravity: Gravity::Bottom, // origin y = 320
        constraint_adjustment: ConstraintAdjustment {
            slide_y: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place_in(Rect::new(0, 0, 500, 400));
    assert_eq!(geo.y, 0, "oversized popup slides flush to the near edge");
    assert_eq!(geo.h, 900, "slide never resizes");
}

#[test]
fn popup_resize_clamps_both_origin_and_extent() {
    // Origin left of the target AND extent past the right: resize clips both sides.
    let p = Positioner {
        anchor_rect: Rect::new(0, 0, 0, 0),
        size: (200, 30),
        anchor: Anchor::None,
        gravity: Gravity::Right, // origin x = 0
        constraint_adjustment: ConstraintAdjustment {
            resize_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (-50, 0), // pushes origin to -50 (left overflow)
    };
    let geo = p.place_in(Rect::new(0, 0, 100, 400));
    assert_eq!(geo.x, 0, "origin clipped to the target left");
    assert!(geo.right() <= 100, "extent clipped to the target right");
    assert!(geo.w >= 1, "resize keeps a positive width");
}

#[test]
fn popup_resize_never_produces_zero_size() {
    // A popup entirely off the right edge, resize-only: width floors at 1 rather than going <= 0.
    let p = Positioner {
        anchor_rect: Rect::new(100, 0, 0, 0),
        size: (50, 50),
        anchor: Anchor::None,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment {
            resize_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place_in(Rect::new(0, 0, 100, 100));
    assert!(geo.w >= 1, "width never collapses to zero (got {})", geo.w);
}

#[test]
fn popup_flip_then_slide_ordering() {
    // flip is tried first; if it fixes the axis, slide is not applied. Here flip alone fits.
    let p = Positioner {
        anchor_rect: Rect::new(470, 10, 10, 10),
        size: (100, 50),
        anchor: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment {
            flip_x: true,
            slide_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    // Flip puts origin at 470 - 100 = 370 (fits). Slide would have flushed to right=500 (origin 400).
    assert_eq!(
        geo.x, 370,
        "flip wins over slide when it resolves the overflow"
    );
}

#[test]
fn constrain_popup_uses_scene_output_area() {
    let scene = scene_with_output(); // 2560x1440
    let p = Positioner {
        anchor_rect: Rect::new(2550, 10, 4, 4),
        size: (100, 50),
        anchor: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment {
            slide_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = scene.constrain_popup(&p);
    assert_eq!(
        geo.right(),
        2560,
        "constrained against the 2560-wide output"
    );
}

#[test]
fn popup_for_native_toplevel_is_constrained_to_window_content() {
    let mut scene = scene_with_output(); // desktop is deliberately much larger than the window
    let top = map_toplevel(&mut scene, 800, 600);
    let p = Positioner {
        anchor_rect: Rect::new(300, 560, 120, 30),
        size: (240, 180),
        anchor: Anchor::Bottom,
        gravity: Gravity::Bottom,
        constraint_adjustment: ConstraintAdjustment {
            flip_y: true,
            slide_y: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };

    let geo = scene.constrain_popup_for_parent(top, &p);
    assert!(
        geo.y < p.anchor_rect.y,
        "the dropdown flips above its field"
    );
    assert!(
        geo.bottom() <= 600,
        "the dropdown stays inside its native window, not merely inside the desktop"
    );
}

#[test]
fn nested_popup_constraints_are_translated_into_parent_coordinates() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 800, 600);
    let menu = scene.create_surface();
    scene.set_role(menu, popup_role(top, Rect::new(500, 200, 250, 300)));
    commit_surface(&mut scene, menu, Commit::attach(shm(250, 300)));
    let p = Positioner {
        anchor_rect: Rect::new(220, 250, 20, 20),
        size: (200, 100),
        anchor: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment {
            flip_x: true,
            slide_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };

    let geo = scene.constrain_popup_for_parent(menu, &p);
    assert!(
        500 + geo.right() <= 800,
        "submenu remains inside the owning native toplevel"
    );
}

// =================================================================================================
// 10. popup_placement / offset_to_toplevel corners
// =================================================================================================

#[test]
fn popup_placement_none_for_non_popup() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    assert_eq!(
        scene.popup_placement(top),
        None,
        "a toplevel has no popup placement"
    );
}

#[test]
fn popup_on_subsurface_resolves_to_the_toplevel() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let subsurf = scene.create_surface();
    scene.set_role(subsurf, sub(top, 40, 60));
    commit_surface(&mut scene, subsurf, Commit::attach(shm(100, 100)));
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(subsurf, Rect::new(5, 7, 30, 30)));
    commit_surface(&mut scene, popup, Commit::attach(shm(30, 30)));

    // popup_offset_to_toplevel resolves through the subsurface to the toplevel; the popup's own
    // geometry origin is relative to the parent's window geometry (the offset does not add the
    // subsurface position, matching the ported walk).
    let (tl, x, y, depth) = scene.popup_offset_to_toplevel(popup).unwrap();
    assert_eq!(tl, top);
    assert_eq!((x, y), (5, 7));
    assert_eq!(depth, 1);
    let popups = scene.collect_popups_for_root(top);
    assert_eq!(popups, vec![(popup, 5, 7)]);
}

#[test]
fn nested_submenu_depth_orders_parents_before_children() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 800, 600);
    let menu = scene.create_surface();
    scene.set_role(menu, popup_role(top, Rect::new(100, 50, 200, 300)));
    commit_surface(&mut scene, menu, Commit::attach(shm(200, 300)));
    let submenu = scene.create_surface();
    scene.set_role(submenu, popup_role(menu, Rect::new(60, 30, 150, 250)));
    commit_surface(&mut scene, submenu, Commit::attach(shm(150, 250)));

    let popups = scene.collect_popups_for_root(top);
    assert_eq!(
        popups[0].0, menu,
        "the menu (depth 1) is ordered before the submenu (depth 2)"
    );
    assert_eq!(
        popups[1],
        (submenu, 160, 80),
        "submenu offset = menu origin + submenu origin"
    );
}

// =================================================================================================
// 11. Focus + hit-testing edges
// =================================================================================================
