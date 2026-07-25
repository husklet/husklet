use super::*;

#[test]
fn window_state_is_an_authoritative_snapshot() {
    let mut scene = scene_with_output();
    let parent = map_toplevel(&mut scene, 800, 600);
    let dialog = map_toplevel(&mut scene, 420, 180);
    let surface = scene.get_mut(dialog).unwrap();
    surface.title = "Confirm".into();
    surface.transient_parent = Some(parent);
    surface.window_geometry = Some(Rect::new(12, 8, 396, 156));
    surface.min_size = (Some(320), Some(120));
    surface.max_size = (Some(1600), None);
    scene.set_visibility(dialog, Visibility::Minimized);

    let window = scene.window_state(dialog).expect("toplevel window state");
    assert_eq!(window.surface, dialog);
    assert_eq!(
        window.kind,
        WindowKind::Toplevel {
            parent: Some(parent)
        }
    );
    assert_eq!(window.title, "Confirm");
    assert_eq!(window.logical_size, Some((420, 180)));
    assert_eq!(window.geometry, Some(Rect::new(12, 8, 396, 156)));
    assert_eq!(window.min_size, (Some(320), Some(120)));
    assert_eq!(window.max_size, (Some(1600), None));
    assert_eq!(window.visibility, Visibility::Minimized);
}

#[test]
fn native_window_visibility_follows_wayland_mapping() {
    let mut scene = scene_with_output();
    let surface = scene.create_surface();
    scene.set_role(surface, SurfaceRole::Toplevel);
    assert_eq!(
        scene.window_state(surface).unwrap().visibility,
        Visibility::Occluded,
        "an xdg role without a buffer is not mapped"
    );

    commit_surface(&mut scene, surface, Commit::attach(shm(320, 200)));
    assert_eq!(
        scene.window_state(surface).unwrap().visibility,
        Visibility::Visible
    );

    commit_surface(
        &mut scene,
        surface,
        Commit {
            buffer: BufferChange::Removed,
            ..Commit::default()
        },
    );
    assert_eq!(
        scene.window_state(surface).unwrap().visibility,
        Visibility::Occluded,
        "a null-buffer commit unmaps without destroying the xdg role"
    );
}
