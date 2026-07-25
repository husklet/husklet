use super::*;
#[test]
fn hit_test_respects_input_region() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel);
    commit_surface(
        &mut scene,
        top,
        Commit {
            input_region: Some(Some(Rect::new(50, 50, 100, 100))),
            ..Commit::attach(shm(300, 300))
        },
    );
    // Inside the surface but OUTSIDE the input region => no hit.
    assert_eq!(
        focus::surface_at(&scene, top, 10, 10),
        None,
        "outside the input region rejects input"
    );
    // Inside the input region => hit at the root offset.
    assert_eq!(focus::surface_at(&scene, top, 60, 60), Some((top, 0, 0)));
}

#[test]
fn hit_test_returns_topmost_of_overlapping_subsurfaces() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let lower = scene.create_surface();
    scene.set_role(lower, sub(top, 0, 0));
    commit_surface(&mut scene, lower, Commit::attach(shm(200, 200)));
    let upper = scene.create_surface();
    scene.set_role(upper, sub(top, 0, 0));
    commit_surface(&mut scene, upper, Commit::attach(shm(200, 200)));
    // Both cover (50,50); the later-registered (upper) is topmost.
    let hit = focus::surface_at(&scene, top, 50, 50).map(|(s, _, _)| s);
    assert_eq!(hit, Some(upper), "topmost overlapping subsurface wins");
}

#[test]
fn hit_test_returns_popup_before_toplevel_underneath() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 300);
    let popup = scene.create_surface();
    scene.set_role(
        popup,
        SurfaceRole::Popup(PopupState {
            parent: top,
            positioner: menu_positioner(),
            geometry: Rect::new(20, 40, 160, 90),
            grabbed: true,
        }),
    );
    commit_surface(&mut scene, popup, Commit::attach(shm(160, 90)));

    assert_eq!(
        focus::surface_at(&scene, top, 50, 70),
        Some((popup, 20, 40)),
        "popup above the toplevel receives menu-row input"
    );
}

#[test]
fn hit_test_misses_outside_the_whole_tree() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    assert_eq!(
        focus::surface_at(&scene, top, 500, 500),
        None,
        "point outside every surface => miss"
    );
}

#[test]
fn hit_test_bufferless_surface_accepts_nothing() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel); // no buffer
    assert_eq!(
        focus::surface_at(&scene, top, 0, 0),
        None,
        "no buffer => no input surface"
    );
}

#[test]
fn update_pointer_floors_fractional_coordinates() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, 50, 50));
    commit_surface(&mut scene, child, Commit::attach(shm(10, 10)));
    // 49.9 floors to 49 => outside the child (starts at 50) => hits root.
    assert_eq!(
        focus::update_pointer(&mut scene, top, 49.9, 49.9),
        Some(top)
    );
    assert_eq!(
        scene.seat().pointer_location,
        (49.9, 49.9),
        "the exact fractional location is recorded"
    );
    // 50.0 => inside the child.
    assert_eq!(
        focus::update_pointer(&mut scene, top, 50.0, 50.0),
        Some(child)
    );
}

#[test]
fn focus_clear_and_refocus_report_changes() {
    let mut scene = scene_with_output();
    let a = map_toplevel(&mut scene, 100, 100);
    scene.focus(a);
    let ch = scene.clear_focus();
    assert_eq!((ch.previous, ch.current), (Some(a), None));
    assert!(ch.changed());
    // Clearing again is a no-op change.
    let ch = scene.clear_focus();
    assert!(!ch.changed());
}

#[test]
fn refocusing_the_same_surface_is_not_a_change() {
    let mut scene = scene_with_output();
    let a = map_toplevel(&mut scene, 100, 100);
    scene.focus(a);
    let ch = scene.focus(a);
    assert!(
        !ch.changed(),
        "focusing the already-focused surface reports no change"
    );
}

// =================================================================================================
// 12. schedule / pacing across scripted clock anomalies
// =================================================================================================
