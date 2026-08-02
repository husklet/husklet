use super::*;
use hl_compositor::scene::port::{CompletionOutcome, PresentationCompletion, PresentationId};

fn two_role_compositor() -> (Compositor<FakePresenter, FakeClock>, SurfaceId) {
    let mut compositor = compositor();
    let root = map_toplevel(&mut compositor.scene, 800, 600);
    let child = compositor.scene.create_surface();
    compositor
        .scene
        .set_role(child, popup_role(root, Rect::new(10, 10, 64, 64)));
    commit_surface(&mut compositor.scene, child, Commit::attach(shm(64, 64)));
    (compositor, root)
}

fn script_two(compositor: &mut Compositor<FakePresenter, FakeClock>, first: u64, second: u64) {
    // FakePresenter pops its script; enqueue in reverse presentation order.
    compositor.presenter_mut().script(PresentOutcome::Pending {
        id: PresentationId(second),
    });
    compositor.presenter_mut().script(PresentOutcome::Pending {
        id: PresentationId(first),
    });
}

fn delivered(id: u64) -> PresentationCompletion {
    PresentationCompletion {
        id: PresentationId(id),
        outcome: CompletionOutcome::Delivered {
            serial: id,
            timing: None,
        },
    }
}

#[test]
fn multilayer_frame_settles_only_after_out_of_order_completions() {
    let (mut compositor, root) = two_role_compositor();
    compositor.apply_commit(root, Commit::frame_callback_only());
    script_two(&mut compositor, 11, 12);

    let pending = compositor.present_root(root);
    assert_eq!(pending.pacing, FramePacing::Pending);
    assert_eq!(
        pending.submissions,
        vec![PresentationId(11), PresentationId(12)]
    );
    assert!(compositor.complete_presentation(delivered(12)).is_none());

    let (_, completed) = compositor
        .complete_presentation(delivered(11))
        .expect("last layer completes the atomic frame");
    assert_eq!(completed.pacing, FramePacing::Presented);
    assert_eq!(completed.callbacks_fired, 1);
}

#[test]
fn one_layer_failure_terminates_atomic_frame_after_all_receipts_retire() {
    let (mut compositor, root) = two_role_compositor();
    compositor.apply_commit(root, Commit::frame_callback_only());
    script_two(&mut compositor, 21, 22);
    assert_eq!(compositor.present_root(root).pacing, FramePacing::Pending);

    assert!(
        compositor
            .complete_presentation(PresentationCompletion {
                id: PresentationId(21),
                outcome: CompletionOutcome::TerminalFailure,
            })
            .is_none()
    );
    let (_, completed) = compositor
        .complete_presentation(delivered(22))
        .expect("remaining submission retired");
    assert_eq!(completed.pacing, FramePacing::TerminalFailure);
    assert_eq!(completed.callbacks_fired, 1);
}

#[test]
fn retryable_retains_callbacks_and_terminal_releases_them() {
    // A drawable the host never displayed is not a dead frame. Retaining its callbacks is what lets the
    // next accepted present fire them; dropping them leaves the client waiting forever on a callback that
    // will never arrive, which is indistinguishable to the user from a frozen window.
    let (mut compositor, root) = two_role_compositor();
    compositor.apply_commit(root, Commit::frame_callback_only());
    script_two(&mut compositor, 91, 92);
    assert_eq!(compositor.present_root(root).pacing, FramePacing::Pending);

    assert!(
        compositor
            .complete_presentation(PresentationCompletion {
                id: PresentationId(91),
                outcome: CompletionOutcome::RetryableFailure,
            })
            .is_none()
    );
    let (_, completed) = compositor
        .complete_presentation(delivered(92))
        .expect("remaining submission retired");
    assert_eq!(completed.pacing, FramePacing::RetryableFailure);
    assert_eq!(completed.callbacks_fired, 0, "retained, not fired");
    assert_eq!(
        compositor.retained_callbacks(root),
        1,
        "the callback survives for the next accepted present"
    );

    // Terminal beats retryable when both appear in one atomic frame.
    let refresh = compositor.scene.primary_output().unwrap().refresh_nanos();
    compositor.clock().set(refresh);
    compositor.apply_commit(
        root,
        Commit::frame_callback_only().with_damage(Rect::new(0, 0, 2, 2)),
    );
    script_two(&mut compositor, 93, 94);
    assert_eq!(compositor.present_root(root).pacing, FramePacing::Pending);
    assert!(
        compositor
            .complete_presentation(PresentationCompletion {
                id: PresentationId(93),
                outcome: CompletionOutcome::RetryableFailure,
            })
            .is_none()
    );
    let (_, mixed) = compositor
        .complete_presentation(PresentationCompletion {
            id: PresentationId(94),
            outcome: CompletionOutcome::TerminalFailure,
        })
        .expect("remaining submission retired");
    assert_eq!(mixed.pacing, FramePacing::TerminalFailure);
    assert_eq!(
        mixed.callbacks_fired, 2,
        "terminal recovery releases both the retained and current callbacks"
    );
    assert_eq!(compositor.retained_callbacks(root), 0);
}

#[test]
fn duplicate_stale_and_new_generation_completions_do_not_cross() {
    let (mut compositor, root) = two_role_compositor();
    compositor.apply_commit(root, Commit::frame_callback_only());
    script_two(&mut compositor, 31, 32);
    compositor.present_root(root);

    compositor.apply_commit(
        root,
        Commit::frame_callback_only().with_damage(Rect::new(0, 0, 1, 1)),
    );
    assert!(compositor.complete_presentation(delivered(31)).is_none());
    let (_, first) = compositor.complete_presentation(delivered(32)).unwrap();
    assert_eq!(first.callbacks_fired, 1);
    assert!(compositor.complete_presentation(delivered(31)).is_none());
    assert!(
        compositor
            .complete_presentation(delivered(u64::MAX))
            .is_none()
    );

    let refresh = compositor.scene.primary_output().unwrap().refresh_nanos();
    compositor.clock().set(refresh);
    script_two(&mut compositor, 41, 42);
    assert_eq!(compositor.present_root(root).pacing, FramePacing::Pending);
    assert!(compositor.complete_presentation(delivered(41)).is_none());
    let (_, second) = compositor.complete_presentation(delivered(42)).unwrap();
    assert_eq!(second.callbacks_fired, 1);
}

#[test]
fn commit_while_pending_stays_queued_for_the_next_frame() {
    let (mut compositor, root) = two_role_compositor();
    compositor.apply_commit(root, Commit::frame_callback_only());
    script_two(&mut compositor, 51, 52);
    assert_eq!(compositor.present_root(root).pacing, FramePacing::Pending);

    compositor.apply_commit(
        root,
        Commit::frame_callback_only().with_damage(Rect::new(0, 0, 2, 2)),
    );
    assert_eq!(compositor.present_root(root).pacing, FramePacing::Pending);
    assert!(compositor.complete_presentation(delivered(51)).is_none());
    let (_, first) = compositor.complete_presentation(delivered(52)).unwrap();
    assert_eq!(first.callbacks_fired, 1, "A owns only A's callback");

    compositor
        .clock()
        .set(compositor.scene.primary_output().unwrap().refresh_nanos());
    script_two(&mut compositor, 61, 62);
    compositor.present_root(root);
    assert!(compositor.complete_presentation(delivered(61)).is_none());
    let (_, second) = compositor.complete_presentation(delivered(62)).unwrap();
    assert_eq!(second.callbacks_fired, 1, "B remains queued");
}

#[test]
fn cancelling_root_detaches_every_submission_and_late_completion() {
    let (mut compositor, root) = two_role_compositor();
    compositor.apply_commit(root, Commit::frame_callback_only());
    script_two(&mut compositor, 71, 72);
    compositor.present_root(root);

    compositor.cancel_root(root);
    assert!(compositor.complete_presentation(delivered(71)).is_none());
    assert!(
        compositor
            .complete_presentation(PresentationCompletion {
                id: PresentationId(72),
                outcome: CompletionOutcome::TerminalFailure,
            })
            .is_none()
    );
}

#[test]
fn duplicate_submission_identity_is_rejected_atomically() {
    let (mut compositor, root) = two_role_compositor();
    compositor.presenter_mut().script(PresentOutcome::Pending {
        id: PresentationId(81),
    });
    compositor.presenter_mut().script(PresentOutcome::Pending {
        id: PresentationId(81),
    });
    let frame = compositor.present_root(root);
    assert_eq!(frame.pacing, FramePacing::TerminalFailure);
    assert!(frame.submissions.is_empty());
    assert!(compositor.complete_presentation(delivered(81)).is_none());
}

#[test]
fn submission_identity_reused_by_another_root_does_not_steal_completion() {
    let mut compositor = compositor();
    let first = map_toplevel(&mut compositor.scene, 800, 600);
    let second = map_toplevel(&mut compositor.scene, 640, 480);
    compositor.apply_commit(first, Commit::frame_callback_only());
    compositor.apply_commit(second, Commit::frame_callback_only());

    compositor.presenter_mut().script(PresentOutcome::Pending {
        id: PresentationId(91),
    });
    assert_eq!(compositor.present_root(first).pacing, FramePacing::Pending);

    compositor.presenter_mut().script(PresentOutcome::Pending {
        id: PresentationId(91),
    });
    let conflict = compositor.present_root(second);
    assert_eq!(conflict.pacing, FramePacing::TerminalFailure);
    assert!(conflict.submissions.is_empty());

    let (root, completed) = compositor
        .complete_presentation(delivered(91))
        .expect("the original submission retains its completion route");
    assert_eq!(root, first);
    assert_eq!(completed.pacing, FramePacing::Presented);
    assert_eq!(completed.callbacks_fired, 1);
}
