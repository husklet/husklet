use super::*;
#[test]
fn commit_to_unknown_surface_is_a_no_op() {
    let mut scene = scene_with_output();
    let changed = commit_surface(&mut scene, SurfaceId(999), Commit::attach(shm(10, 10)));
    assert!(
        !changed,
        "committing to a non-existent surface reports no change"
    );
    assert!(!scene.any_dirty());
}

#[test]
fn damage_before_any_buffer_does_not_dirty() {
    // Out-of-order: damage arrives before the first attach. Nothing is visible yet.
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 10, 10)),
    );
    assert!(!changed, "damage with no buffer is not a content change");
    assert!(!scene.is_dirty(id));
    assert!(
        scene.get(id).unwrap().damage.is_empty(),
        "no damage accumulates without a buffer"
    );
}

#[test]
fn detach_without_prior_buffer_reports_no_change() {
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Removed,
            ..Commit::default()
        },
    );
    assert!(
        !changed,
        "detaching when nothing was attached is not a change"
    );
}

#[test]
fn empty_damage_rects_do_not_accumulate_but_new_buffer_still_dirties() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    scene.clear_dirty(id);
    scene.get_mut(id).unwrap().damage.clear();
    // Keep-buffer commit whose only damage rects are empty: no visible change.
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 0, 0))
        .with_damage(Rect::new(5, 5, -1, 10)),
    );
    assert!(
        !changed,
        "only-empty damage on a live buffer is not a change"
    );
    assert!(scene.get(id).unwrap().damage.is_empty());

    // Re-attaching a buffer always dirties even if the damage list is empty.
    let changed = commit_surface(&mut scene, id, Commit::attach(shm(100, 100)));
    assert!(changed, "a fresh attach is always a content change");
    assert!(scene.is_dirty(id));
}

#[test]
fn region_and_title_only_commit_updates_state_without_dirtying() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    scene.clear_dirty(id);
    let changed = commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Keep,
            opaque_region: Some(Some(Rect::new(0, 0, 50, 50))),
            input_region: Some(Some(Region::from_spans(vec![Span::Add(Rect::new(
                10, 10, 20, 20,
            ))]))),
            title: Some("hello".into()),
            ..Commit::default()
        },
    );
    assert!(!changed, "region/title-only commit is not a content change");
    assert!(!scene.is_dirty(id));
    let s = scene.get(id).unwrap();
    assert_eq!(s.opaque_region, Some(Rect::new(0, 0, 50, 50)));
    assert_eq!(
        s.input_region.as_ref().and_then(Region::bounding_box),
        Some(Rect::new(10, 10, 20, 20))
    );
    assert_eq!(s.title, "hello");
}

#[test]
fn opaque_region_can_be_explicitly_cleared() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    commit_surface(
        &mut scene,
        id,
        Commit {
            opaque_region: Some(Some(Rect::new(0, 0, 100, 100))),
            ..Commit::default()
        },
    );
    assert!(scene.get(id).unwrap().opaque_region.is_some());
    // Some(None): the client set an empty opaque region — clears it.
    commit_surface(
        &mut scene,
        id,
        Commit {
            opaque_region: Some(None),
            ..Commit::default()
        },
    );
    assert_eq!(scene.get(id).unwrap().opaque_region, None);
}

#[test]
fn detach_clears_accumulated_damage() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 10, 10)),
    );
    assert!(!scene.get(id).unwrap().damage.is_empty());
    commit_surface(
        &mut scene,
        id,
        Commit {
            buffer: BufferChange::Removed,
            ..Commit::default()
        },
    );
    assert!(
        scene.get(id).unwrap().damage.is_empty(),
        "detach clears damage"
    );
    assert!(scene.get(id).unwrap().buffer.is_none());
}

#[test]
fn frame_callbacks_accumulate_across_commits() {
    let mut scene = scene_with_output();
    let id = map_toplevel(&mut scene, 100, 100);
    for _ in 0..5 {
        commit_surface(&mut scene, id, Commit::frame_callback_only());
    }
    assert_eq!(scene.get(id).unwrap().pending_callbacks, 5);
}

// =================================================================================================
// 6. Scene tree structure: linkage, removal, id allocation
// =================================================================================================

#[test]
fn set_role_registers_subsurface_child_once_and_in_order() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let a = scene.create_surface();
    let b = scene.create_surface();
    scene.set_role(a, sub(top, 0, 0));
    scene.set_role(b, sub(top, 0, 0));
    // Re-assigning the same role must not double-register the child.
    scene.set_role(a, sub(top, 5, 5));
    assert_eq!(
        scene.subsurface_children(top),
        &[a, b],
        "registration order = z-order, no dup"
    );
}

#[test]
fn remove_surface_unlinks_from_parent_and_clears_focus() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, 10, 10));
    commit_surface(&mut scene, child, Commit::attach(shm(50, 50)));
    scene.focus(child);
    focus::update_pointer(&mut scene, top, 15.0, 15.0);
    assert_eq!(scene.seat().pointer_focus, Some(child));

    scene.remove_surface(child);
    assert!(!scene.contains(child));
    assert_eq!(
        scene.subsurface_children(top),
        &[] as &[SurfaceId],
        "child unlinked from parent"
    );
    assert_eq!(scene.seat().keyboard_focus, None, "keyboard focus cleared");
    assert_eq!(scene.seat().pointer_focus, None, "pointer focus cleared");
    // Idempotent.
    scene.remove_surface(child);
}

#[test]
fn removing_a_popup_drops_it_from_the_popup_registry() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(top, Rect::new(10, 10, 100, 100)));
    commit_surface(&mut scene, popup, Commit::attach(shm(100, 100)));
    assert_eq!(scene.collect_popups_for_root(top).len(), 1);
    scene.remove_surface(popup);
    assert_eq!(
        scene.collect_popups_for_root(top).len(),
        0,
        "popup gone from registry"
    );
    assert_eq!(scene.popup_ids().count(), 0);
}

#[test]
fn insert_surface_advances_the_id_counter_past_it() {
    let mut scene = scene_with_output();
    scene.insert_surface(Surface::new(SurfaceId(50)));
    let next = scene.create_surface();
    assert!(
        next.0 > 50,
        "create_surface never re-mints an inserted id (got {next:?})"
    );
}

#[test]
fn window_root_climbs_subsurface_then_popup_chain() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let subsurf = scene.create_surface();
    scene.set_role(subsurf, sub(top, 10, 10));
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(subsurf, Rect::new(5, 5, 50, 50)));
    let nested = scene.create_surface();
    scene.set_role(nested, sub(popup, 1, 1));

    assert_eq!(
        scene.window_root(nested),
        Some(top),
        "climb sub->popup->sub->top to the toplevel"
    );
    // present_root STOPS at the popup (its own native window), not the toplevel.
    assert_eq!(scene.present_root(nested), Some(popup));
    assert_eq!(
        scene.present_root(subsurf),
        Some(top),
        "a subsurface resolves to its toplevel"
    );
}

#[test]
fn window_root_survives_a_self_referential_cycle() {
    // A pathological subsurface whose parent is itself must not hang (depth guard).
    let mut scene = scene_with_output();
    let a = scene.create_surface();
    scene.set_role(
        a,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: a,
            x: 0,
            y: 0,
            sync: false,
        }),
    );
    // Bounded walk returns *something* rather than looping forever.
    let _ = scene.window_root(a);
    let mut out = Vec::new();
    scene.collect_tree_surfaces(a, &mut out); // self-child guarded against infinite recursion
    assert_eq!(out, vec![a]);
}

// =================================================================================================
// 7. compose_frame: geometry, damage translation, empty layers, z-order
// =================================================================================================
