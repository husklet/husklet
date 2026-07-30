use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
#[cfg(feature = "macos-surface")]
use crate::scene::model::{OutputId, PresentableImage};
#[cfg(feature = "macos-surface")]
use crate::scene::port::PresentOutcome;
#[cfg(feature = "macos-surface")]
use crate::scene::port::PresentTiming;

#[cfg(feature = "macos-surface")]
fn present(
    adapter: &mut AdapterPresenter,
    image: &PresentableImage,
    timing: PresentTiming,
) -> PresentationFeedback {
    adapter.present_frame(&PresentFrame {
        output: OutputId(1),
        role: image.surface,
        origin: (0, 0),
        layers: vec![crate::scene::port::PresentLayer {
            image: image.clone(),
            x: 0,
            y: 0,
            damage: Vec::new(),
        }],
        timing,
    })
}

struct RecordingPresenter {
    polls: Arc<AtomicUsize>,
    windows: Arc<AtomicUsize>,
    deposits: Arc<AtomicUsize>,
    forgets: Arc<AtomicUsize>,
}

impl Presenter for RecordingPresenter {
    fn present_frame(&mut self, _frame: &PresentFrame) -> PresentationFeedback {
        PresentationFeedback::offscreen()
    }
}

impl HostEvents for RecordingPresenter {
    fn poll_events(&mut self) {
        self.polls.fetch_add(1, Ordering::Relaxed);
    }
}

impl Windows for RecordingPresenter {
    fn reconcile_window(&mut self, _window: &crate::scene::model::WindowState) {
        self.windows.fetch_add(1, Ordering::Relaxed);
    }
}

impl Clipboard for RecordingPresenter {}

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

#[cfg(feature = "macos-surface")]
struct NativeRecordingPresenter {
    outcome: PresentOutcome,
    completions: Vec<(SurfaceId, bool)>,
}

#[cfg(feature = "macos-surface")]
impl Presenter for NativeRecordingPresenter {
    fn present_frame(&mut self, _frame: &PresentFrame) -> PresentationFeedback {
        PresentationFeedback {
            outcome: self.outcome,
        }
    }
}

#[cfg(feature = "macos-surface")]
impl HostEvents for NativeRecordingPresenter {}
#[cfg(feature = "macos-surface")]
impl Clipboard for NativeRecordingPresenter {}
#[cfg(feature = "macos-surface")]
impl Windows for NativeRecordingPresenter {}

#[cfg(feature = "macos-surface")]
impl SurfacePresenter for NativeRecordingPresenter {
    fn deposit(&mut self, _surface: SurfaceId, _buffer: StoredBuffer) {}

    fn forget(&mut self, _surface: SurfaceId) {}

    fn attach_native(
        &mut self,
        _surface: SurfaceId,
        _content: hl_iosurface::Surface,
    ) -> Result<(), hl_iosurface::Surface> {
        Ok(())
    }

    fn take_native_completions(&mut self) -> Vec<(SurfaceId, bool)> {
        std::mem::take(&mut self.completions)
    }
}

#[cfg(feature = "macos-surface")]
#[test]
fn native_completion_waits_for_real_delivery() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::adapter::smithay::native::{native_frames, NativeFrame, NativeFrameOutcome};

    let (sender, frames) = native_frames(1).unwrap();
    let receipt = sender
        .publish(NativeFrame::new(4, 8, hl_iosurface::Surface::new_bgra(2, 2).unwrap()).unwrap())
        .unwrap();
    let frame = frames.try_next().unwrap().unwrap();
    let mut adapter = AdapterPresenter::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Delivered {
            serial: 1,
            timing: None,
        },
        completions: Vec::new(),
    });
    adapter.attach_native(SurfaceId(1), frame, None).unwrap();
    let releases = Arc::new(AtomicUsize::new(0));
    adapter
        .native
        .get_mut(&SurfaceId(1))
        .unwrap()
        .back_mut()
        .unwrap()
        .buffer = Some(BufferLease::Probe(releases.clone()));
    let image = PresentableImage {
        surface: SurfaceId(1),
        width: 2,
        height: 2,
        format: crate::scene::model::Format::Argb8888,
        gpu: true,
        popup: None,
        present_crop: None,
        transform: crate::scene::model::BufferTransform::Normal,
    };
    let timing = PresentTiming::fallback(1, 1);
    present(&mut adapter, &image, timing);
    assert!(receipt.try_complete().unwrap().is_none());
    assert_eq!(releases.load(Ordering::Relaxed), 0);

    adapter.poll_events();
    assert!(receipt.try_complete().unwrap().is_none());
    assert_eq!(releases.load(Ordering::Relaxed), 0);

    adapter.inner = Box::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Delivered {
            serial: 1,
            timing: None,
        },
        completions: vec![(SurfaceId(1), false)],
    });
    adapter.poll_events();
    assert_eq!(
        receipt.try_complete().unwrap().unwrap().outcome,
        NativeFrameOutcome::Displayed
    );
    assert_eq!(releases.load(Ordering::Relaxed), 1);
}

#[cfg(feature = "macos-surface")]
#[test]
fn retryable_native_present_keeps_frame_until_a_drawable_succeeds() {
    use crate::adapter::smithay::native::{native_frames, NativeFrame, NativeFrameOutcome};

    let (sender, frames) = native_frames(1).unwrap();
    let receipt = sender
        .publish(NativeFrame::new(4, 9, hl_iosurface::Surface::new_bgra(2, 2).unwrap()).unwrap())
        .unwrap();
    let mut adapter = AdapterPresenter::new(NativeRecordingPresenter {
        outcome: PresentOutcome::RetryableFailure,
        completions: Vec::new(),
    });
    adapter
        .attach_native(SurfaceId(2), frames.try_next().unwrap().unwrap(), None)
        .unwrap();
    let image = PresentableImage {
        surface: SurfaceId(2),
        width: 2,
        height: 2,
        format: crate::scene::model::Format::Argb8888,
        gpu: true,
        popup: None,
        present_crop: None,
        transform: crate::scene::model::BufferTransform::Normal,
    };
    let timing = PresentTiming::fallback(1, 1);

    present(&mut adapter, &image, timing);
    adapter.poll_events();
    assert!(receipt.try_complete().unwrap().is_none());
    assert!(!adapter.native[&SurfaceId(2)].front().unwrap().submitted);

    adapter.inner = Box::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Delivered {
            serial: 1,
            timing: None,
        },
        completions: vec![(SurfaceId(2), false)],
    });
    present(&mut adapter, &image, timing);
    adapter.poll_events();
    assert_eq!(
        receipt.try_complete().unwrap().unwrap().outcome,
        NativeFrameOutcome::Displayed
    );
}

#[cfg(feature = "macos-surface")]
#[test]
fn dropping_native_attachment_releases_its_wayland_lease() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::adapter::smithay::native::NativeFrame;

    let releases = Arc::new(AtomicUsize::new(0));
    let attachment = NativeAttachment {
        frame: Some(
            NativeFrame::new(1, 1, hl_iosurface::Surface::new_bgra(2, 2).unwrap()).unwrap(),
        ),
        buffer: Some(BufferLease::Probe(releases.clone())),
        submission: None,
        submitted: false,
    };
    drop(attachment);
    assert_eq!(releases.load(Ordering::Relaxed), 1);
}

#[cfg(feature = "macos-surface")]
#[test]
fn same_surface_inflight_frames_release_only_at_matching_fences() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::adapter::smithay::native::{native_frames, NativeFrame, NativeFrameOutcome};

    let (sender, frames) = native_frames(2).unwrap();
    let mut adapter = AdapterPresenter::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Delivered {
            serial: 1,
            timing: None,
        },
        completions: Vec::new(),
    });
    let releases = Arc::new(AtomicUsize::new(0));
    let image = PresentableImage {
        surface: SurfaceId(7),
        width: 2,
        height: 2,
        format: crate::scene::model::Format::Argb8888,
        gpu: true,
        popup: None,
        present_crop: None,
        transform: crate::scene::model::BufferTransform::Normal,
    };
    let timing = PresentTiming::fallback(1, 1);
    let mut receipts = Vec::new();
    for serial in 1..=2 {
        let receipt = sender
            .publish(
                NativeFrame::new(5, serial, hl_iosurface::Surface::new_bgra(2, 2).unwrap())
                    .unwrap(),
            )
            .unwrap();
        let frame = frames.try_next().unwrap().unwrap();
        adapter.attach_native(SurfaceId(7), frame, None).unwrap();
        adapter
            .native
            .get_mut(&SurfaceId(7))
            .unwrap()
            .back_mut()
            .unwrap()
            .buffer = Some(BufferLease::Probe(releases.clone()));
        present(&mut adapter, &image, timing);
        receipts.push(receipt);
    }
    assert_eq!(releases.load(Ordering::Relaxed), 0);

    adapter.inner = Box::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Offscreen,
        completions: vec![(SurfaceId(7), false)],
    });
    adapter.poll_events();
    assert_eq!(releases.load(Ordering::Relaxed), 1);
    assert_eq!(
        receipts[0].try_complete().unwrap().unwrap().outcome,
        NativeFrameOutcome::Displayed
    );
    assert!(receipts[1].try_complete().unwrap().is_none());

    adapter.inner = Box::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Offscreen,
        completions: vec![(SurfaceId(7), false)],
    });
    adapter.poll_events();
    assert_eq!(releases.load(Ordering::Relaxed), 2);
    assert_eq!(
        receipts[1].try_complete().unwrap().unwrap().outcome,
        NativeFrameOutcome::Displayed
    );
}

#[cfg(feature = "macos-surface")]
#[test]
fn destroy_waits_for_fence_and_metal_error_is_terminal() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::adapter::smithay::native::{native_frames, NativeFrame, NativeFrameOutcome};

    let (sender, frames) = native_frames(1).unwrap();
    let receipt = sender
        .publish(NativeFrame::new(9, 1, hl_iosurface::Surface::new_bgra(2, 2).unwrap()).unwrap())
        .unwrap();
    let mut adapter = AdapterPresenter::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Delivered {
            serial: 1,
            timing: None,
        },
        completions: Vec::new(),
    });
    adapter
        .attach_native(SurfaceId(8), frames.try_next().unwrap().unwrap(), None)
        .unwrap();
    let releases = Arc::new(AtomicUsize::new(0));
    adapter
        .native
        .get_mut(&SurfaceId(8))
        .unwrap()
        .back_mut()
        .unwrap()
        .buffer = Some(BufferLease::Probe(releases.clone()));
    let image = PresentableImage {
        surface: SurfaceId(8),
        width: 2,
        height: 2,
        format: crate::scene::model::Format::Argb8888,
        gpu: true,
        popup: None,
        present_crop: None,
        transform: crate::scene::model::BufferTransform::Normal,
    };
    present(&mut adapter, &image, PresentTiming::fallback(1, 1));
    adapter.destroy_window(SurfaceId(8));
    assert_eq!(releases.load(Ordering::Relaxed), 0);
    assert!(receipt.try_complete().unwrap().is_none());

    adapter.inner = Box::new(NativeRecordingPresenter {
        outcome: PresentOutcome::Offscreen,
        completions: vec![(SurfaceId(8), true)],
    });
    adapter.poll_events();
    assert_eq!(releases.load(Ordering::Relaxed), 1);
    assert_eq!(
        receipt.try_complete().unwrap().unwrap().outcome,
        NativeFrameOutcome::TerminalFailure
    );
}
