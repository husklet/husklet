use super::*;

// ---- 3. popup placement math -------------------------------------------------------------------

#[test]
fn popup_placement_anchors_and_applies_gravity() {
    // Anchor at the bottom-left of a 100×20 widget at (40, 30); gravity bottom-right ⇒ the popup's
    // top-left sits at the anchor point (40, 50).
    let p = Positioner {
        anchor_rect: Rect::new(40, 30, 100, 20),
        size: (150, 200),
        anchor: Anchor::BottomLeft,
        gravity: Gravity::BottomRight,
        constraint_adjustment: ConstraintAdjustment::NONE,
        offset: (0, 0),
    };
    let geo = p.place(2000, 2000);
    assert_eq!(geo, Rect::new(40, 50, 150, 200));

    // The extra offset shifts the result.
    let p2 = Positioner {
        offset: (5, -7),
        ..p
    };
    assert_eq!(p2.place(2000, 2000), Rect::new(45, 43, 150, 200));
}

#[test]
fn popup_placement_flips_then_slides_to_stay_on_screen() {
    // A menu anchored at the right edge with gravity right would overflow; flip_y/flip_x mirror it.
    // Anchor bottom of a widget near the bottom edge; gravity bottom would run off the bottom, so
    // flip_y flips to gravity top (popup grows upward and fits).
    let p = Positioner {
        anchor_rect: Rect::new(10, 380, 40, 20), // widget bottom at y=400 in a 400-tall area
        size: (100, 150),
        anchor: Anchor::Bottom,
        gravity: Gravity::Bottom, // unflipped: top-left y = 400 → bottom = 550 > 400 (overflow)
        constraint_adjustment: ConstraintAdjustment {
            flip_y: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    // Flipped to gravity top about the flipped anchor (top, y=380): top-left y = 380 - 150 = 230.
    assert_eq!(geo.y, 230, "flip_y mirrors the popup back on-screen");
    assert!(geo.bottom() <= 400, "flipped popup fits within the output");

    // With only slide_x, an x-overflow is slid in from the right edge instead of flipped.
    let slide = Positioner {
        anchor_rect: Rect::new(460, 10, 20, 20),
        size: (100, 50),
        anchor: Anchor::Right,
        gravity: Gravity::Right, // top-left x = 480 → right = 580 > 500
        constraint_adjustment: ConstraintAdjustment {
            slide_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = slide.place(500, 400);
    assert_eq!(
        geo.right(),
        500,
        "slide_x pushes the popup flush to the right edge"
    );
    assert_eq!(geo.w, 100, "slide keeps the popup size");
}

#[test]
fn popup_placement_resizes_when_it_cannot_fit() {
    // A popup wider than the whole target, allowed only to resize, is clamped to the target width.
    let p = Positioner {
        anchor_rect: Rect::new(0, 0, 10, 10),
        size: (900, 100),
        anchor: Anchor::Right,
        gravity: Gravity::Right,
        constraint_adjustment: ConstraintAdjustment {
            resize_x: true,
            ..ConstraintAdjustment::NONE
        },
        offset: (0, 0),
    };
    let geo = p.place(500, 400);
    assert!(
        geo.right() <= 500 && geo.x >= 0,
        "resized popup fits the target"
    );
    assert!(geo.w < 900, "resize clamps the width");
}
