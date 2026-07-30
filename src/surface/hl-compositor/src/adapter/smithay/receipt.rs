#[cfg(test)]
mod readiness_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn pending_gpu_frame_retains_surface_until_completion() {
        let (sender, frames) = native_frames(2).unwrap();
        let ready = Arc::new(AtomicBool::new(false));
        let probe = Arc::clone(&ready);
        let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
        let id = surface.id();
        let receipt = sender
            .publish(
                NativeFrame::new(1, 1, surface)
                    .unwrap()
                    .with_readiness(move || probe.load(Ordering::Acquire)),
            )
            .unwrap();

        assert!(frames.try_next().unwrap().is_none());
        assert!(receipt.try_complete().unwrap().is_none());

        ready.store(true, Ordering::Release);
        let frame = frames.try_next().unwrap().expect("completed frame");
        assert_eq!(frame.surface.id(), id);
        assert!(receipt.try_complete().unwrap().is_none());
        frame.complete(NativeFrameOutcome::Displayed);
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Displayed
        );
    }

    #[test]
    fn readiness_probe_runs_without_holding_ingress_lock() {
        let (sender, frames) = native_frames(2).unwrap();
        let ingress = Arc::clone(&sender.0);
        let frame = NativeFrame::new(1, 1, hl_iosurface::Surface::new_bgra(2, 2).unwrap())
            .unwrap()
            .with_readiness(move || ingress.try_lock().is_ok());
        sender.publish(frame).unwrap();

        assert!(frames.try_next().unwrap().is_some());
    }

    #[test]
    fn blocked_device_does_not_head_block_another_device() {
        let (sender, frames) = native_frames(3).unwrap();
        sender
            .publish(
                NativeFrame::new(1, 1, hl_iosurface::Surface::new_bgra(2, 2).unwrap())
                    .unwrap()
                    .with_device_readiness(10, || false),
            )
            .unwrap();
        sender
            .publish(
                NativeFrame::new(2, 1, hl_iosurface::Surface::new_bgra(2, 2).unwrap())
                    .unwrap()
                    .with_device_readiness(20, || true),
            )
            .unwrap();

        assert_eq!(frames.try_next().unwrap().unwrap().token.get(), 2);
    }

    #[test]
    fn frames_remain_fifo_within_one_device() {
        let (sender, frames) = native_frames(3).unwrap();
        let later_polled = Arc::new(AtomicBool::new(false));
        sender
            .publish(
                NativeFrame::new(1, 1, hl_iosurface::Surface::new_bgra(2, 2).unwrap())
                    .unwrap()
                    .with_device_readiness(10, || false),
            )
            .unwrap();
        let observed = Arc::clone(&later_polled);
        sender
            .publish(
                NativeFrame::new(2, 1, hl_iosurface::Surface::new_bgra(2, 2).unwrap())
                    .unwrap()
                    .with_device_readiness(10, move || {
                        observed.store(true, Ordering::Release);
                        true
                    }),
            )
            .unwrap();

        assert!(frames.try_next().unwrap().is_none());
        assert!(!later_polled.load(Ordering::Acquire));
    }
}
