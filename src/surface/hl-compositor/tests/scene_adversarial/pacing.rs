use super::*;
#[test]
fn should_present_treats_backward_clock_as_not_due() {
    let refresh = 16_666_666u64;
    // now < last (clock anomaly): not due (avoids a spurious present).
    assert!(!schedule::should_present(100, Some(1_000_000), refresh));
    // Exactly one interval: due.
    assert!(schedule::should_present(refresh, Some(0), refresh));
    // First present (last None): always due, even mid-interval.
    assert!(schedule::should_present(5, None, refresh));
}

#[test]
fn outcome_to_pacing_mapping_is_total() {
    use FramePacing::*;
    assert_eq!(
        FramePacing::from(PresentOutcome::Delivered {
            serial: 1,
            timing: None
        }),
        Presented
    );
    assert_eq!(
        FramePacing::from(PresentOutcome::Offscreen),
        RetryableFailure
    );
    assert_eq!(
        FramePacing::from(PresentOutcome::RetryableFailure),
        RetryableFailure
    );
    assert_eq!(
        FramePacing::from(PresentOutcome::TerminalFailure),
        TerminalFailure
    );
}

#[test]
fn fallback_timing_sets_vsync_only_with_a_known_refresh() {
    let t = PresentTiming::fallback(1_000, 16_666_666);
    assert!(t.vsync && t.refresh_ns == 16_666_666 && t.present_ns == 1_000);
    let t0 = PresentTiming::fallback(1_000, 0);
    assert!(!t0.vsync, "unknown refresh => no vsync claim");
}

#[test]
fn no_output_root_is_a_terminal_failure() {
    // A scene with NO output: present cannot target anything.
    let mut c = Compositor::new(FakePresenter::new(), FakeClock::new(0));
    let top = map_toplevel(&mut c.scene, 100, 100);
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::TerminalFailure);
    assert_eq!(c.presenter().count(), 0);
    assert!(out.presented.is_empty());
}

#[test]
fn unknown_refresh_output_presents_every_dirty_commit() {
    // refresh_mhz = 0 => no throttle: each dirty commit presents.
    let mut c = Compositor::new(FakePresenter::new(), FakeClock::new(0));
    c.scene
        .add_output(Output::new(OutputId(1), "vrr", 800, 600, 0));
    let top = map_toplevel(&mut c.scene, 800, 600);
    c.present_root(top);
    assert_eq!(c.presenter().count(), 1);
    // Same clock time, new damage: still presents (no interval to wait for).
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 5, 5)),
    );
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::Presented);
    assert_eq!(
        c.presenter().count(),
        2,
        "unknown-refresh output never throttles"
    );
}

#[test]
fn next_present_due_is_none_until_a_frame_ships_then_last_plus_refresh() {
    let mut c = compositor();
    let refresh = c.scene.primary_output().unwrap().refresh_nanos();
    let top = map_toplevel(&mut c.scene, 800, 600);

    // Never presented yet: no boundary to arm a repaint against.
    assert_eq!(
        c.next_present_due_ns(top),
        None,
        "no due time before the first present"
    );

    // Present at t=1_000_000: the next boundary is exactly that present + one refresh interval.
    c.clock().set(1_000_000);
    assert_eq!(c.present_root(top).pacing, FramePacing::Presented);
    assert_eq!(
        c.next_present_due_ns(top),
        Some(1_000_000 + refresh),
        "due time = last present + refresh interval"
    );

    // An unknown surface has no due time.
    assert_eq!(c.next_present_due_ns(SurfaceId(9999)), None);
}

#[test]
fn throttled_frame_ships_when_redriven_at_its_due_boundary() {
    // The exact contract the adapter's repaint timer relies on: a commit throttled within one interval
    // is retained, and re-driving `present_root` at `next_present_due_ns` ships it — even with NO further
    // commit (the "client went idle" case). Without a re-drive it would never present.
    let mut c = compositor();
    let refresh = c.scene.primary_output().unwrap().refresh_nanos();
    let top = map_toplevel(&mut c.scene, 800, 600);

    // First frame ships at t=0.
    assert_eq!(c.present_root(top).pacing, FramePacing::Presented);
    assert_eq!(c.presenter().count(), 1);

    // A new frame half an interval later is throttled — retained, not shipped.
    c.clock().set(refresh / 2);
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 20, 20)),
    );
    let throttled = c.present_root(top);
    assert!(
        throttled.throttled,
        "commit within one interval is throttled"
    );
    assert_eq!(c.presenter().count(), 1, "nothing shipped while throttled");

    // The adapter would arm a repaint at exactly this boundary.
    let due = c
        .next_present_due_ns(top)
        .expect("a shipped frame has a due time");
    assert_eq!(
        due, refresh,
        "boundary is one interval after the t=0 present"
    );

    // Re-drive at the boundary with NO new commit: the retained frame now ships and clears dirty.
    c.clock().set(due);
    let shipped = c.present_root(top);
    assert_eq!(
        shipped.pacing,
        FramePacing::Presented,
        "the retained frame ships at its boundary"
    );
    assert_eq!(
        c.presenter().count(),
        2,
        "the coalesced frame presents exactly once, one interval later"
    );
    assert!(
        !c.scene.is_dirty(top),
        "shipping the retained frame clears its dirty state"
    );
}

#[test]
fn a_burst_of_throttled_commits_coalesces_to_one_shipped_frame() {
    // Many commits inside one interval must not each ship: they coalesce into a single present when the
    // boundary is finally re-driven — the adapter never double-presents a superseded frame.
    let mut c = compositor();
    let refresh = c.scene.primary_output().unwrap().refresh_nanos();
    let top = map_toplevel(&mut c.scene, 800, 600);
    assert_eq!(c.present_root(top).pacing, FramePacing::Presented);

    for i in 1..=5 {
        c.clock().set(refresh / 10 * i); // five commits, all strictly within one interval
        c.apply_commit(
            top,
            Commit {
                buffer: BufferChange::Keep,
                ..Commit::default()
            }
            .with_damage(Rect::new(0, 0, 5 + i as i32, 5)),
        );
        assert!(
            c.present_root(top).throttled,
            "each mid-interval commit is throttled"
        );
    }
    assert_eq!(
        c.presenter().count(),
        1,
        "the whole burst shipped nothing yet"
    );

    // One re-drive at the boundary ships exactly one coalesced frame for the whole burst.
    c.clock().set(refresh);
    assert_eq!(c.present_root(top).pacing, FramePacing::Presented);
    assert_eq!(
        c.presenter().count(),
        2,
        "the five coalesced commits ship as a single present"
    );
}

#[test]
fn terminal_failure_releases_retained_callbacks_for_recovery() {
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 100, 100);
    // Retain a callback via a retryable failure.
    c.presenter_mut().script(PresentOutcome::RetryableFailure);
    let mut attach = Commit::attach(shm(100, 100));
    attach.frame_callback = true;
    c.commit(top, attach);
    assert_eq!(c.retained_callbacks(top), 1);
    // A terminal failure discards presentation feedback, but a frame callback is only permission to draw
    // again. Releasing it is what lets the client replace the failed frame.
    c.clock().set(1_000_000_000);
    c.presenter_mut().script(PresentOutcome::TerminalFailure);
    c.apply_commit(
        top,
        Commit {
            buffer: BufferChange::Keep,
            ..Commit::default()
        }
        .with_damage(Rect::new(0, 0, 5, 5)),
    );
    let out = c.present_root(top);
    assert_eq!(out.pacing, FramePacing::TerminalFailure);
    assert_eq!(out.callbacks_fired, 1, "terminal failure permits recovery");
    assert_eq!(
        c.retained_callbacks(top),
        0,
        "terminal failure drops the retained queue"
    );
}

#[test]
fn retained_callbacks_are_bounded() {
    // A permanently-dead presenter must not grow the retained-callback queue without bound.
    let mut c = compositor();
    let top = map_toplevel(&mut c.scene, 100, 100);
    for i in 0..50u64 {
        c.presenter_mut().script(PresentOutcome::RetryableFailure);
        c.clock().set(i * 1_000_000_000);
        let mut attach = Commit::attach(shm(100, 100));
        attach.frame_callback = true;
        c.commit(top, attach);
    }
    assert!(
        c.retained_callbacks(top) <= 16,
        "retained callbacks capped at MAX_RETAINED_CALLBACKS"
    );
}
