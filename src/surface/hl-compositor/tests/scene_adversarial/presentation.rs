use super::*;
#[test]
fn cursor_commit_never_presents() {
    let mut c = compositor();
    let cursor = c.scene.create_surface();
    c.scene.set_role(cursor, SurfaceRole::Cursor);
    let out = c.commit(cursor, Commit::attach(shm(24, 24)));
    assert!(out.changed, "the cursor buffer is a content change");
    assert!(
        out.frame.is_none(),
        "a cursor is never presented as a window"
    );
    assert_eq!(c.presenter().count(), 0);
}

#[test]
fn synchronized_subsurface_commit_does_not_present_on_its_own() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 200, 200);
    c.present_root(top);
    let n = c.presenter().count();
    let child = c.scene.create_surface();
    c.scene.set_role(
        child,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 0,
            y: 0,
            sync: true,
        }),
    );
    let out = c.commit(child, Commit::attach(shm(50, 50)));
    assert!(
        out.frame.is_none(),
        "a synchronized subsurface presents with its parent, not alone"
    );
    assert_eq!(
        c.presenter().count(),
        n,
        "no present triggered by the sync-subsurface commit"
    );
}

#[test]
fn desync_subsurface_commit_presents_the_toplevel_tree() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 200, 200);
    c.present_root(top);
    c.clock().set(1_000_000_000);
    let child = c.scene.create_surface();
    c.scene.set_role(child, sub(top, 10, 10)); // desync
    let out = c
        .commit(child, Commit::attach(shm(50, 50)))
        .frame
        .expect("a desync child presents");
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(
        out.presented,
        vec![top, child],
        "the whole toplevel tree presents"
    );
}

#[test]
fn present_carries_coalesced_damage_bounding_box() {
    // Two damage rects within one interval coalesce; the present carries their bounding box.
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 400, 400);
    c.present_root(top); // clears the fresh-attach damage
    let refresh = c.scene.primary_output().unwrap().refresh_nanos();
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(10, 10, 20, 20)),
    );
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(100, 100, 5, 5)),
    );
    c.clock().set(refresh);
    c.present_root(top);
    let call = c.presenter().calls().pop().unwrap();
    assert_eq!(
        call.damage,
        vec![Rect::new(10, 10, 95, 95)],
        "coalesced damage bounding box presented"
    );
}

#[test]
fn present_records_output_and_logical_dimensions() {
    let mut c = compositor();
    let top = scene_with_output_toplevel(&mut c);
    c.present_root(top);
    let call = &c.presenter().calls()[0];
    assert_eq!(call.output, OutputId(1));
    assert_eq!((call.width, call.height), (640, 480));
}

fn scene_with_output_toplevel(c: &mut Compositor<FakePresenter, FakeClock>) -> SurfaceId {
    map_toplevel(&mut c.scene, 640, 480)
}

#[test]
fn minimized_then_visible_resumes_presenting() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 200, 200);
    c.scene.set_visibility(top, Visibility::Occluded);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::RetryableFailure);
    assert_eq!(
        c.presenter().count(),
        0,
        "an occluded root never reaches the presenter"
    );
    // Reveal it: the retained dirty frame now presents.
    c.scene.set_visibility(top, Visibility::Visible);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(c.presenter().count(), 1);
}

#[test]
fn visibility_allows_present_only_when_visible() {
    assert!(Visibility::Visible.allows_present());
    assert!(!Visibility::Occluded.allows_present());
    assert!(!Visibility::Minimized.allows_present());
}
