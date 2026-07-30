mod tests {
    use std::cell::Cell;
    use std::collections::HashMap;

    use super::{repaint_deadline, take_due_repaints};
    use crate::scene::model::{
        BufferState, Format, Output, OutputId, SurfaceId, SurfaceRole,
    };
    use crate::scene::port::{
        Clock, PresentFrame, PresentOutcome, PresentationFeedback, Presenter,
    };
    use crate::scene::service::{Commit, FramePacing};
    use crate::Compositor;

    struct FakeClock(Cell<u64>);

    impl Clock for FakeClock {
        fn now_nanos(&self) -> u64 {
            self.0.get()
        }
    }

    struct FakePresenter {
        outcome: PresentOutcome,
        calls: usize,
    }

    impl Presenter for FakePresenter {
        fn present_frame(&mut self, _frame: &PresentFrame) -> PresentationFeedback {
            self.calls += 1;
            PresentationFeedback {
                outcome: self.outcome,
            }
        }
    }

    #[test]
    fn future_throttle_deadline_remains_exact() {
        assert_eq!(repaint_deadline(100, Some(140), 16), 140);
    }

    #[test]
    fn expired_and_first_retry_wait_one_refresh() {
        assert_eq!(repaint_deadline(100, Some(90), 16), 116);
        assert_eq!(repaint_deadline(100, None, 16), 116);
    }

    #[test]
    fn retry_deadline_saturates_without_wrapping() {
        assert_eq!(repaint_deadline(u64::MAX - 3, None, 16), u64::MAX);
    }

    #[test]
    fn due_deadline_is_consumed_before_retry() {
        let due = SurfaceId(4);
        let future = SurfaceId(5);
        let mut pending = HashMap::from([(due, 100), (future, 101)]);

        assert_eq!(take_due_repaints(&mut pending, 100), vec![due]);
        assert!(!pending.contains_key(&due));
        assert_eq!(pending.get(&future), Some(&101));

        pending.insert(due, repaint_deadline(100, Some(90), 16));
        assert!(take_due_repaints(&mut pending, 100).is_empty());
        assert_eq!(pending.get(&due), Some(&116));
    }

    #[test]
    fn offscreen_frame_retries_once_per_refresh_then_delivery_settles_callback() {
        let mut compositor = Compositor::new(
            FakePresenter {
                outcome: PresentOutcome::Offscreen,
                calls: 0,
            },
            FakeClock(Cell::new(0)),
        );
        compositor
            .scene
            .add_output(Output::new(OutputId(1), "test", 800, 600, 60_000));
        let refresh = compositor.scene.primary_output().unwrap().refresh_nanos();
        let root = compositor.scene.create_surface();
        compositor.scene.set_role(root, SurfaceRole::Toplevel);
        let mut commit = Commit::attach(BufferState {
            tex_w: 800,
            tex_h: 600,
            format: Format::Argb8888,
            buffer_scale: 1,
            gpu: false,
        });
        commit.frame_callback = true;

        let first = compositor.commit(root, commit).frame.unwrap();
        assert_eq!(first.pacing, FramePacing::RetryableFailure);
        assert_eq!(first.callbacks_fired, 0);
        assert_eq!(compositor.retained_callbacks(root), 1);
        assert_eq!(compositor.presenter().calls, 1);

        let mut pending = HashMap::from([(
            root,
            repaint_deadline(
                compositor.clock().now_nanos(),
                compositor.next_present_due_ns(root),
                refresh,
            ),
        )]);
        assert!(take_due_repaints(&mut pending, 0).is_empty());
        assert!(take_due_repaints(&mut pending, refresh - 1).is_empty());
        assert_eq!(
            compositor.presenter().calls,
            1,
            "offscreen content must not be recomposed on every host tick"
        );

        compositor.clock().0.set(refresh);
        assert_eq!(take_due_repaints(&mut pending, refresh), vec![root]);
        compositor.presenter_mut().outcome = PresentOutcome::Delivered {
            serial: 1,
            timing: None,
        };
        let delivered = compositor.present_root(root);
        assert_eq!(delivered.pacing, FramePacing::Presented);
        assert_eq!(delivered.callbacks_fired, 1);
        assert_eq!(compositor.retained_callbacks(root), 0);
        assert_eq!(compositor.presenter().calls, 2);
    }
}
