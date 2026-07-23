use super::*;

// ---- 6. focus on window raise / close ----------------------------------------------------------

#[test]
fn focus_follows_window_map_raise_and_close() {
    let mut scene = scene_with_output();
    let a = map_toplevel(&mut scene, 800, 600);
    let b = map_toplevel(&mut scene, 800, 600);

    // Map/focus A, then B.
    let ch = scene.focus(a);
    assert_eq!((ch.previous, ch.current), (None, Some(a)));
    assert!(scene.seat().has_keyboard_focus(a));

    let ch = scene.activate(b);
    assert!(ch.changed());
    assert_eq!((ch.previous, ch.current), (Some(a), Some(b)));
    assert!(scene.seat().has_keyboard_focus(b));

    // Closing B (the focused window) drops focus to nothing (no auto-refocus).
    let ch = scene.on_window_gone(b);
    assert_eq!((ch.previous, ch.current), (Some(b), None));
    assert_eq!(scene.seat().keyboard_focus, None);

    // Closing an UNFOCUSED window does not touch focus.
    scene.focus(a);
    let ch = scene.on_window_gone(b);
    assert!(!ch.changed());
    assert_eq!(scene.seat().keyboard_focus, Some(a));
}

#[test]
fn pointer_hit_test_returns_topmost_input_surface() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 1000, 700);
    let child = scene.create_surface();
    scene.set_role(
        child,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 100,
            y: 100,
            sync: false,
        }),
    );
    commit_surface(&mut scene, child, Commit::attach(shm(200, 200)));

    // Inside the child (topmost) region → hits the child.
    let hit = focus::update_pointer(&mut scene, top, 150.0, 150.0);
    assert_eq!(hit, Some(child));
    assert_eq!(scene.seat().pointer_focus, Some(child));

    // Outside the child but inside the root → hits the root.
    let hit = focus::update_pointer(&mut scene, top, 10.0, 10.0);
    assert_eq!(hit, Some(top));
}
