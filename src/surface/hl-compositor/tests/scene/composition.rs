use super::*;

// ---- 1. commit → scene + damage ----------------------------------------------------------------

#[test]
fn surface_commit_attaches_buffer_and_marks_damage() {
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);

    // A first buffer attach changes content and marks the surface dirty.
    let changed = commit_surface(&mut scene, id, Commit::attach(shm(800, 600)));
    assert!(changed, "first attach reports a content change");
    assert!(
        scene.is_dirty(id),
        "attaching a buffer marks the surface dirty"
    );
    assert_eq!(scene.get(id).unwrap().logical_size(), Some((800, 600)));

    // Clearing dirty then a frame-callback-only commit does NOT re-dirty (nothing new to show) but
    // records the pending callback.
    scene.clear_dirty(id);
    let changed = commit_surface(&mut scene, id, Commit::frame_callback_only());
    assert!(
        !changed,
        "a frame-callback-only commit reports no content change"
    );
    assert!(
        !scene.is_dirty(id),
        "a frame-callback-only commit does not dirty the surface"
    );
    assert_eq!(scene.get(id).unwrap().pending_callbacks, 1);

    // Damage against the existing buffer re-dirties and accumulates into the damage region.
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(10, 20, 30, 40)),
    );
    assert!(changed, "damage against a live buffer is a content change");
    assert!(scene.is_dirty(id));
    assert_eq!(
        scene.get(id).unwrap().damage.bounding_box(),
        Some(Rect::new(10, 20, 30, 40))
    );

    // Detach clears the buffer.
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Removed,
            ..Commit::default()
        },
    );
    assert!(changed, "detaching a live buffer is a change");
    assert!(scene.get(id).unwrap().buffer.is_none());
}

// ---- 2. compose walks the window + popup tree --------------------------------------------------

#[test]
fn compose_frame_walks_subsurfaces_and_popups_in_z_order() {
    let mut scene = scene_with_output();

    // Toplevel with two subsurfaces (bottom then top) and a popup (a menu) anchored on it.
    let top = map_toplevel(&mut scene, 1000, 700);
    let sub_bottom = scene.create_surface();
    scene.set_role(
        sub_bottom,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 10,
            y: 20,
            sync: false,
        }),
    );
    commit_surface(&mut scene, sub_bottom, Commit::attach(shm(100, 100)));
    let sub_top = scene.create_surface();
    scene.set_role(
        sub_top,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 30,
            y: 40,
            sync: false,
        }),
    );
    commit_surface(&mut scene, sub_top, Commit::attach(shm(100, 100)));

    // A popup anchored on the toplevel with resolved geometry origin (200, 150).
    let popup = scene.create_surface();
    scene.set_role(
        popup,
        SurfaceRole::Popup(hl_compositor::scene::model::PopupState {
            parent: top,
            positioner: menu_positioner(),
            geometry: Rect::new(200, 150, 180, 240),
            grabbed: true,
        }),
    );
    commit_surface(&mut scene, popup, Commit::attach(shm(180, 240)));

    let frame = scene.compose_frame(top).expect("root has a buffer");

    // Composite order (bottom → top): root, its subsurfaces (registration order = z-order), then the
    // popup on top.
    assert_eq!(frame.present_order(), vec![top, sub_bottom, sub_top, popup]);

    // Offsets: subsurfaces at their set_position; popup at its geometry origin relative to the root.
    let by_sid = |sid: SurfaceId| frame.items.iter().find(|i| i.image.surface == sid).unwrap();
    assert_eq!((by_sid(sub_bottom).x, by_sid(sub_bottom).y), (10, 20));
    assert_eq!((by_sid(sub_top).x, by_sid(sub_top).y), (30, 40));
    assert_eq!((by_sid(popup).x, by_sid(popup).y), (200, 150));

    // The popup layer carries its native placement (direct parent + geometry origin).
    let placement = by_sid(popup).image.popup.expect("popup carries placement");
    assert_eq!(
        (placement.parent, placement.x, placement.y),
        (top, 200, 150)
    );

    // Every layer is dirty (freshly attached), so the whole tree damages; each layer's damage is its
    // whole rect translated to root space.
    assert!(
        frame.damage().contains(&Rect::new(0, 0, 1000, 700)),
        "root full-surface damage"
    );
    assert!(
        frame.damage().contains(&Rect::new(200, 150, 180, 240)),
        "popup damage in root space"
    );
}

#[test]
fn compositor_presents_the_tree_layers_in_order_through_the_presenter() {
    // The FakePresenter records one present() per composed layer, in composite order — the exact seam a
    // real adapter drives. Assert against its recorded calls (not just the composed Frame).
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 1000, 700);
    let sub = c.scene.create_surface();
    c.scene.set_role(
        sub,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 30,
            y: 40,
            sync: false,
        }),
    );
    commit_surface(&mut c.scene, sub, Commit::attach(shm(100, 100)));
    let popup = c.scene.create_surface();
    c.scene
        .set_role(popup, popup_role(top, Rect::new(200, 150, 180, 240)));
    commit_surface(&mut c.scene, popup, Commit::attach(shm(180, 240)));

    let out = c.present_root(top);
    assert_eq!(out.presented, vec![top, sub, popup]);
    assert_eq!(
        c.presenter().present_order(),
        vec![top, sub, popup],
        "presenter recorded the z-order"
    );
    // The base layer's delivery timing carries the 60 Hz refresh interval.
    let base = &c.presenter().calls()[0];
    assert_eq!(base.output, OutputId(1));
    assert_eq!((base.width, base.height), (1000, 700));
    assert!(base.timing.vsync && base.timing.refresh_ns > 0);
}

#[test]
fn nested_popup_offsets_accumulate_up_the_chain() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 1000, 700);

    // Menu on the toplevel at (100, 50); submenu on the menu at (60, 30).
    let menu = scene.create_surface();
    scene.set_role(menu, popup_role(top, Rect::new(100, 50, 200, 300)));
    commit_surface(&mut scene, menu, Commit::attach(shm(200, 300)));
    let submenu = scene.create_surface();
    scene.set_role(submenu, popup_role(menu, Rect::new(60, 30, 150, 250)));
    commit_surface(&mut scene, submenu, Commit::attach(shm(150, 250)));

    let frame = scene.compose_frame(top).unwrap();
    // Parent-before-child order; submenu offset = menu origin + submenu origin.
    assert_eq!(frame.present_order(), vec![top, menu, submenu]);
    let submenu_item = frame
        .items
        .iter()
        .find(|i| i.image.surface == submenu)
        .unwrap();
    assert_eq!((submenu_item.x, submenu_item.y), (160, 80));
}
