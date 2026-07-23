use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

struct RecordingPresenter {
    polls: Arc<AtomicUsize>,
    windows: Arc<AtomicUsize>,
    deposits: Arc<AtomicUsize>,
    forgets: Arc<AtomicUsize>,
}

impl Presenter for RecordingPresenter {
    fn poll_events(&mut self) {
        self.polls.fetch_add(1, Ordering::Relaxed);
    }

    fn reconcile_window(&mut self, _window: &crate::scene::model::WindowState) {
        self.windows.fetch_add(1, Ordering::Relaxed);
    }

    fn present(
        &mut self,
        _output: OutputId,
        _image: &PresentableImage,
        _damage: &[Rect],
        _timing: PresentTiming,
    ) -> PresentationFeedback {
        PresentationFeedback::offscreen()
    }
}

impl SurfacePresenter for RecordingPresenter {
    fn deposit(&mut self, _surface: SurfaceId, _buffer: StoredBuffer) {
        self.deposits.fetch_add(1, Ordering::Relaxed);
    }

    fn forget(&mut self, _surface: SurfaceId) {
        self.forgets.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn delegates_adapter_lifecycle_to_injected_presenter() {
    let polls = Arc::new(AtomicUsize::new(0));
    let windows = Arc::new(AtomicUsize::new(0));
    let deposits = Arc::new(AtomicUsize::new(0));
    let forgets = Arc::new(AtomicUsize::new(0));
    let mut adapter = AdapterPresenter::new(RecordingPresenter {
        polls: Arc::clone(&polls),
        windows: Arc::clone(&windows),
        deposits: Arc::clone(&deposits),
        forgets: Arc::clone(&forgets),
    });

    adapter.poll_events();
    adapter.reconcile_window(&crate::scene::model::WindowState {
        surface: SurfaceId(4),
        kind: crate::scene::model::WindowKind::Toplevel { parent: None },
        title: "terminal".into(),
        logical_size: Some((1, 1)),
        min_size: (None, None),
        max_size: (None, None),
        maximized: false,
        fullscreen: false,
        geometry: None,
        visibility: crate::scene::model::Visibility::Visible,
    });
    adapter.deposit(
        SurfaceId(4),
        StoredBuffer {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 4],
            bgra: false,
            damage: None,
        },
    );
    adapter.forget(SurfaceId(4));

    assert_eq!(polls.load(Ordering::Relaxed), 1);
    assert_eq!(windows.load(Ordering::Relaxed), 1);
    assert_eq!(deposits.load(Ordering::Relaxed), 1);
    assert_eq!(forgets.load(Ordering::Relaxed), 1);
}
