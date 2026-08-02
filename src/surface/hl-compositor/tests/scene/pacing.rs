use super::*;

// ---- 4. frame scheduling / pacing across scripted clock times ----------------------------------

#[test]
fn vsync_throttle_coalesces_commits_within_one_refresh_interval() {
    // Pure schedule decision: 60 Hz ⇒ ~16.67 ms interval.
    let refresh = Output::new(OutputId(1), "o", 2560, 1440, 60_000).refresh_nanos();
    assert!(
        schedule::should_present(0, None, refresh),
        "first frame always due"
    );
    assert!(
        !schedule::should_present(refresh / 2, Some(0), refresh),
        "half an interval later: not due"
    );
    assert!(
        schedule::should_present(refresh, Some(0), refresh),
        "a full interval later: due"
    );
    assert!(
        schedule::should_present(1_000, Some(0), 0),
        "unknown refresh: always due"
    );
}

#[test]
fn compositor_paces_presents_across_scripted_clock_times() {
    let mut c = compositor();
    let refresh = c.scene.primary_output().unwrap().refresh_nanos();
    let top = map_toplevel(&mut c.scene, 800, 600);

    // t=0: first commit presents (delivered).
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(c.presenter().count(), 1);
    assert_eq!(out.serial, Some(1));

    // A new commit half an interval later is throttled — no present, frame retained.
    c.clock().set(refresh / 2);
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 10, 10)),
    );
    let out = c.present_root(top);
    assert!(
        out.throttled,
        "a commit within one refresh interval is throttled"
    );
    assert_eq!(c.presenter().count(), 1, "no new present while throttled");
    assert!(
        c.scene.is_dirty(top),
        "the throttled frame stays dirty for the next tick"
    );

    // A full interval later the retained frame presents.
    c.clock().set(refresh);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(
        c.presenter().count(),
        2,
        "the coalesced frame presents once the interval elapses"
    );
}

#[test]
fn clean_tree_skips_present_but_still_fires_frame_callbacks() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 800, 600);
    c.present_root(top); // delivers, clears dirty
    assert_eq!(c.presenter().count(), 1);

    // A frame-callback-only commit on a clean tree: no present, but the callback fires (no stall).
    let out = c.commit(top, Commit::frame_callback_only()).frame.unwrap();
    assert_eq!(out.pacing, FramePacing::Skipped);
    assert_eq!(c.presenter().count(), 1, "a clean tree does not re-present");
    assert_eq!(
        out.callbacks_fired, 1,
        "the frame-callback-only commit's callback fires"
    );
}

#[test]
fn failed_present_retains_callbacks_until_a_later_delivery() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 800, 600);

    // Script a retryable failure for the first present; its frame callback is retained, not fired.
    c.presenter_mut().script(PresentOutcome::RetryableFailure);
    let out = c
        .commit(top, Commit::attach(shm(800, 600)).into_frame_callback())
        .frame
        .unwrap();
    assert_eq!(out.pacing, FramePacing::RetryableFailure);
    assert_eq!(
        out.callbacks_fired, 0,
        "a failed present fires no callbacks"
    );
    assert_eq!(
        c.retained_callbacks(top),
        1,
        "the callback is retained for retry"
    );
    assert!(
        c.scene.is_dirty(top),
        "a failed present keeps the tree dirty"
    );

    // The next present delivers: the retained callback fires now.
    c.clock().set(1_000_000_000);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(
        out.callbacks_fired, 1,
        "the retained callback fires on the next delivery"
    );
    assert_eq!(c.retained_callbacks(top), 0);
}

#[test]
fn invisible_root_does_not_present() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 800, 600);
    c.scene.set_visibility(top, Visibility::Minimized);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::RetryableFailure);
    assert_eq!(
        c.presenter().count(),
        0,
        "a minimized root never reaches the presenter"
    );
}

#[test]
fn pacing_policy_matches_the_ported_state_machine() {
    assert!(FramePacing::Presented.policy().complete_callbacks);
    assert!(FramePacing::Presented.policy().present_feedback);
    assert!(FramePacing::Skipped.policy().complete_callbacks);
    assert!(!FramePacing::Skipped.policy().present_feedback);
    assert!(FramePacing::RetryableFailure.policy().retain);
    assert!(FramePacing::TerminalFailure.policy().complete_callbacks);
    assert!(FramePacing::TerminalFailure.policy().terminal_cleanup);
    // Outcome → pacing mapping: Offscreen is a retryable failure, not a delivery.
    assert_eq!(
        FramePacing::from(PresentOutcome::Offscreen),
        FramePacing::RetryableFailure
    );
    assert_eq!(
        FramePacing::from(PresentOutcome::Delivered {
            serial: 9,
            timing: None
        }),
        FramePacing::Presented
    );
}

// ---- 5. compose skip via conservative occlusion ------------------------------------------------

#[test]
fn fully_occluded_surface_does_not_force_a_present() {
    // The ported `tree_dirty` skips a present only when a dirty surface's WHOLE rectangle is provably
    // covered by an opaque surface composited above it (not merely a damage sub-region).
    let mut scene = scene_with_output();
    let top = map_toplevel(&mut scene, 1000, 700);

    // A small background subsurface, then a larger OPAQUE foreground fully covering it (registered
    // after ⇒ composited above).
    let bg = scene.create_surface();
    scene.set_role(
        bg,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 0,
            y: 0,
            sync: false,
        }),
    );
    commit_surface(&mut scene, bg, Commit::attach(shm(100, 100)));
    let fg = scene.create_surface();
    scene.set_role(
        fg,
        SurfaceRole::Subsurface(SubsurfaceState {
            parent: top,
            x: 0,
            y: 0,
            sync: false,
        }),
    );
    commit_surface(
        &mut scene,
        fg,
        Commit {
            buffer: BufferChange::New(BufferState {
                format: Format::Xrgb8888,
                ..shm(200, 200)
            }),
            opaque_region: Some(Some(Rect::new(0, 0, 200, 200))),
            ..Commit::default()
        },
    );

    let mut c = Compositor::with_scene(scene, FakePresenter::new(), FakeClock::new(0));
    c.present_root(top);
    assert!(
        !c.scene.any_dirty(),
        "the initial present clears the whole tree"
    );

    // Damage ONLY the fully-covered background subsurface: its whole rect is under the opaque fg, so
    // the change is not visible and no present is forced.
    c.apply_commit(
        bg,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(10, 10, 20, 20)),
    );
    assert!(
        !c.scene.is_tree_dirty(top),
        "a surface fully behind an opaque cover is not visible"
    );

    // Damaging the root (whose full 1000×700 rect the 200×200 cover cannot occlude) IS visible.
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(500, 500, 20, 20)),
    );
    assert!(
        c.scene.is_tree_dirty(top),
        "an un-occluded surface forces a present"
    );
}
