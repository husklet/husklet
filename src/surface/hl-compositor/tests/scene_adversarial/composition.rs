use super::*;
#[test]
fn compose_returns_none_when_root_has_no_buffer() {
    let mut scene = scene_with_output();
    let id = scene.create_surface();
    scene.set_role(id, SurfaceRole::Toplevel);
    assert!(
        scene.compose_frame(id).is_none(),
        "no root buffer => nothing to present"
    );
}

#[test]
fn compose_skips_a_subsurface_with_no_buffer() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 200, 200);
    let bufferless = scene.create_surface();
    scene.set_role(bufferless, sub(top, 10, 10)); // never attaches a buffer
    let real = scene.create_surface();
    scene.set_role(real, sub(top, 20, 20));
    commit_surface(&mut scene, real, Commit::attach(shm(50, 50)));

    let frame = scene.compose_frame(top).unwrap();
    assert_eq!(
        frame.present_order(),
        vec![top, real],
        "the bufferless child contributes no layer"
    );
}

#[test]
fn compose_damage_translates_partial_damage_into_root_space() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 300);
    // First present would clear dirty; here we assert compose damage directly.
    let child = scene.create_surface();
    scene.set_role(child, sub(top, 100, 50));
    commit_surface(&mut scene, child, Commit::attach(shm(80, 80)));
    // Clear the fresh-attach full damage and set a specific sub-rect on the child.
    scene.get_mut(child).unwrap().damage.clear();
    commit_surface(
        &mut scene,
        child,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(5, 5, 10, 10)),
    );

    let frame = scene.compose_frame(top).unwrap();
    let child_item = frame
        .items
        .iter()
        .find(|i| i.image.surface == child)
        .unwrap();
    assert_eq!(
        child_item.damage,
        vec![Rect::new(105, 55, 10, 10)],
        "child-local damage (5,5,10,10) lifts to root space by the (100,50) offset"
    );
}

#[test]
fn compose_dirty_layer_without_damage_rects_damages_its_whole_rect() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 300);
    // Fresh attach: dirty, but the damage list is empty => whole-rect damage.
    let frame = scene.compose_frame(top).unwrap();
    let item = &frame.items[0];
    assert_eq!(
        item.damage,
        vec![Rect::new(0, 0, 400, 300)],
        "no explicit damage => full surface"
    );
}

#[test]
fn compose_popup_image_carries_native_placement_and_offset() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 800, 600);
    let popup = scene.create_surface();
    scene.set_role(popup, popup_role(top, Rect::new(120, 90, 200, 150)));
    commit_surface(&mut scene, popup, Commit::attach(shm(200, 150)));

    let frame = scene.compose_frame(top).unwrap();
    let item = frame
        .items
        .iter()
        .find(|i| i.image.surface == popup)
        .unwrap();
    assert_eq!((item.x, item.y), (120, 90));
    let placement = item.image.popup.expect("popup carries placement");
    assert_eq!((placement.parent, placement.x, placement.y), (top, 120, 90));
    // A plain toplevel layer never carries popup placement.
    assert_eq!(frame.items[0].image.popup, None);
}

#[test]
fn compose_image_format_reduces_to_opaque_or_alpha() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel);
    commit_surface(&mut scene, top, Commit::attach(xrgb(100, 100)));
    let frame = scene.compose_frame(top).unwrap();
    assert_eq!(frame.items[0].image.format, Format::Xrgb8888);
    assert!(!frame.items[0].image.gpu);
}

#[test]
fn compose_carries_the_gpu_flag_through() {
    let mut scene = scene_with_output();
    let top = scene.create_surface();
    scene.set_role(top, SurfaceRole::Toplevel);
    commit_surface(
        &mut scene,
        top,
        Commit::attach(BufferState {
            gpu: true,
            ..shm(100, 100)
        }),
    );
    let frame = scene.compose_frame(top).unwrap();
    assert!(
        frame.items[0].image.gpu,
        "the zero-copy GPU flag reaches the presentable image"
    );
}

#[test]
fn deeply_nested_subsurface_offsets_accumulate() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 500, 500);
    let a = scene.create_surface();
    scene.set_role(a, sub(top, 10, 10));
    commit_surface(&mut scene, a, Commit::attach(shm(50, 50)));
    let b = scene.create_surface();
    scene.set_role(b, sub(a, 20, 30));
    commit_surface(&mut scene, b, Commit::attach(shm(20, 20)));
    let c = scene.create_surface();
    scene.set_role(c, sub(b, 5, 7));
    commit_surface(&mut scene, c, Commit::attach(shm(10, 10)));

    let frame = scene.compose_frame(top).unwrap();
    assert_eq!(
        frame.present_order(),
        vec![top, a, b, c],
        "depth-first, bottom->top"
    );
    let item = |sid| frame.items.iter().find(|i| i.image.surface == sid).unwrap();
    assert_eq!((item(b).x, item(b).y), (30, 40));
    assert_eq!(
        (item(c).x, item(c).y),
        (35, 47),
        "c = top+a+b+c offsets summed"
    );
}

#[test]
fn negative_subsurface_offset_is_preserved() {
    // A subsurface placed above/left of its parent (negative offset) is composed there, not clamped.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 200, 200);
    let child = scene.create_surface();
    scene.set_role(child, sub(top, -30, -40));
    commit_surface(&mut scene, child, Commit::attach(shm(50, 50)));
    let frame = scene.compose_frame(top).unwrap();
    let item = frame
        .items
        .iter()
        .find(|i| i.image.surface == child)
        .unwrap();
    assert_eq!((item.x, item.y), (-30, -40));
    assert_eq!(
        item.damage,
        vec![Rect::new(-30, -40, 50, 50)],
        "damage lifts into negative root space"
    );
}

// =================================================================================================
// 8. Occlusion / is_tree_dirty — including the bufferless-cover regression
// =================================================================================================

#[test]
fn clean_tree_is_not_dirty() {
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    scene.clear_dirty(top);
    assert!(!scene.is_tree_dirty(top));
}

#[test]
fn partial_opaque_cover_does_not_occlude() {
    // An opaque foreground that covers only PART of the dirty background cannot skip the present.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let bg = scene.create_surface();
    scene.set_role(bg, sub(top, 0, 0));
    commit_surface(&mut scene, bg, Commit::attach(shm(200, 200)));
    let fg = scene.create_surface();
    scene.set_role(fg, sub(top, 0, 0));
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(xrgb(100, 100)),
            opaque_region: Some(Some(Rect::new(0, 0, 100, 100))), // only covers a quarter of bg
            ..Commit::default()
        },
    );
    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);
    // Damage the bg where the fg does NOT cover it.
    c.apply_commit(
        bg,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(150, 150, 10, 10)),
    );
    assert!(
        c.scene.is_tree_dirty(top),
        "damage outside the opaque cover is visible"
    );
}

#[test]
fn dirty_surface_with_unknown_geometry_forces_present() {
    // A dirty surface with no buffer (unknown logical size) can never be proven occluded.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 100, 100);
    let ghost = scene.create_surface();
    scene.set_role(ghost, sub(top, 0, 0));
    // Mark it dirty without giving it a buffer.
    scene.mark_dirty(ghost);
    assert!(
        scene.is_tree_dirty(top),
        "a dirty surface of unknown size keeps the tree dirty"
    );
}

#[test]
fn bufferless_surface_with_stale_opaque_region_does_not_occlude() {
    // REGRESSION: a surface that set an opaque region and then DETACHED its buffer draws nothing, so
    // it must not be treated as an opaque cover. Before the fix, `opaque_covers` consulted the stale
    // opaque region regardless of buffer presence and wrongly skipped the present (stale frame on
    // screen). The module's own contract is "a present is never wrongly skipped".
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let bg = scene.create_surface();
    scene.set_role(bg, sub(top, 0, 0));
    commit_surface(&mut scene, bg, Commit::attach(shm(200, 200)));
    let fg = scene.create_surface();
    scene.set_role(fg, sub(top, 0, 0));
    // fg attaches an opaque buffer fully covering bg, then detaches the buffer (opaque region persists).
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(xrgb(200, 200)),
            opaque_region: Some(Some(Rect::new(0, 0, 200, 200))),
            ..Commit::default()
        },
    );
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::Removed,
            ..Commit::default()
        },
    );
    assert!(scene.get(fg).unwrap().buffer.is_none());
    assert!(
        scene.get(fg).unwrap().opaque_region.is_some(),
        "opaque region survives detach"
    );

    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);
    assert!(!c.scene.any_dirty(), "the initial present clears the tree");

    // Damage the (now un-covered) background: it IS visible because the cover draws nothing.
    c.apply_commit(
        bg,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(10, 10, 20, 20)),
    );
    assert!(
        c.scene.is_tree_dirty(top),
        "a detached (bufferless) surface must not occlude the damage below it"
    );
}

#[test]
fn occluded_then_uncovered_forces_a_repaint() {
    // When the opaque cover shrinks so it no longer contains the dirty rect, the present resumes.
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 400, 400);
    let bg = scene.create_surface();
    scene.set_role(bg, sub(top, 0, 0));
    commit_surface(&mut scene, bg, Commit::attach(shm(100, 100)));
    let fg = scene.create_surface();
    scene.set_role(fg, sub(top, 0, 0));
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(xrgb(100, 100)),
            opaque_region: Some(Some(Rect::new(0, 0, 100, 100))),
            ..Commit::default()
        },
    );
    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);

    // Fully covered damage: skipped.
    c.apply_commit(
        bg,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 50, 50)),
    );
    assert!(!c.scene.is_tree_dirty(top));

    // Shrink the opaque region; the same damage is now visible.
    c.apply_commit(
        fg,
        Commit {
            opaque_region: Some(Some(Rect::new(0, 0, 10, 10))),
            ..Commit::default()
        },
    );
    assert!(
        c.scene.is_tree_dirty(top),
        "shrunken cover no longer occludes the damage"
    );
}

// =================================================================================================
// 9. Popup placement math: corners the happy-path test doesn't reach
// =================================================================================================
