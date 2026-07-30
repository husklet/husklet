#[cfg(test)]
mod tests {
    use std::os::fd::OwnedFd;
    use std::os::unix::fs::FileExt;

    use smithay::backend::allocator::dmabuf::DmabufFlags;

    use super::*;
    use crate::adapter::smithay::native::{
        native_frames, NativeFramePublishFailure, NativeFrameReceipt,
    };
    use crate::adapter::smithay::present::SurfacePresenter;
    use crate::scene::port::{
        Clipboard, HostEvents, PresentFrame, PresentationFeedback, Presenter, Windows,
    };

    struct NativePresenter {
        completions: Vec<(SurfaceId, bool)>,
    }

    impl Presenter for NativePresenter {
        fn present_frame(&mut self, _frame: &PresentFrame) -> PresentationFeedback {
            PresentationFeedback::delivered(1, None)
        }
    }

    impl HostEvents for NativePresenter {}
    impl Clipboard for NativePresenter {}
    impl Windows for NativePresenter {}

    impl SurfacePresenter for NativePresenter {
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

    fn deferred(surface: u32) -> Deferred {
        Deferred {
            surface: SurfaceId(surface),
            commit: Commit::default(),
            was_mapped: false,
            min_size: (None, None),
            max_size: (None, None),
            frame_callbacks: Vec::new(),
            feedbacks: Vec::new(),
            buffer: None,
            external: None,
            metadata: None,
        }
    }

    fn external(surface: u32, token: u64, serial: u64) -> (Deferred, std::fs::File) {
        let stride = 8;
        let allocation = 16 + hl_surface_protocol::buffer::HEADER_LEN as u64;
        let header = hl_surface_protocol::buffer::Header::new(
            token,
            2,
            2,
            hl_surface_protocol::buffer::DRM_FMT_ARGB8888,
            stride,
            allocation,
        )
        .unwrap();
        let header = if serial == 0 {
            header
        } else {
            header.with_serial(serial).unwrap()
        };
        let path = std::env::temp_dir().join(format!(
            "hl-native-pending-{}-{}",
            std::process::id(),
            getrandom::u64().unwrap()
        ));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        std::fs::remove_file(path).unwrap();
        file.set_len(allocation).unwrap();
        file.write_all_at(&header.encode().unwrap(), 16).unwrap();
        let mut builder = Dmabuf::builder(
            (2, 2),
            Fourcc::Argb8888,
            Modifier::from(hl_surface_protocol::buffer::MODIFIER),
            DmabufFlags::empty(),
        );
        assert!(builder.add_plane(OwnedFd::from(file.try_clone().unwrap()), 0, 0, stride));
        let mut deferred = deferred(surface);
        deferred.external = Some(builder.build().unwrap());
        (deferred, file)
    }

    fn publish_header(file: &std::fs::File, token: u64, serial: u64) {
        let allocation = 16 + hl_surface_protocol::buffer::HEADER_LEN as u64;
        let header = hl_surface_protocol::buffer::Header::new(
            token,
            2,
            2,
            hl_surface_protocol::buffer::DRM_FMT_ARGB8888,
            8,
            allocation,
        )
        .unwrap()
        .with_serial(serial)
        .unwrap();
        file.write_all_at(&header.encode().unwrap(), 16).unwrap();
    }

    fn published(
        sender: &crate::adapter::smithay::native::NativeFrameSender,
        ingress: &NativeFrames,
        token: u64,
        serial: u64,
    ) -> (NativeFrame, NativeFrameReceipt) {
        let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
        let receipt = sender
            .publish(NativeFrame::new(token, serial, surface).unwrap())
            .unwrap();
        (ingress.try_next().unwrap().unwrap(), receipt)
    }

    fn native_state(
        ingress: NativeFrames,
    ) -> (
        smithay::reexports::wayland_server::Display<HlState>,
        HlState,
        SurfaceId,
    ) {
        use smithay::reexports::wayland_server::Display;

        let display: Display<HlState> = Display::new().unwrap();
        let presenter = AdapterPresenter::new(NativePresenter {
            completions: vec![(SurfaceId(1), false)],
        });
        let mut state = HlState::with_native_frames(&display.handle(), presenter, ingress);
        let surface = state.engine.scene.create_surface();
        assert_eq!(surface, SurfaceId(1));
        state.engine.scene.set_role(surface, SurfaceRole::Toplevel);
        (display, state, surface)
    }

    fn assert_protocol_frame_presents(frame_first: bool) {
        let (sender, ingress) = native_frames(2).unwrap();
        let (_display, mut state, surface) = native_state(ingress);
        let serial = if frame_first { 21 } else { 22 };
        let iosurface = hl_iosurface::Surface::new_bgra(3, 2).unwrap();
        let receipt = sender
            .publish(NativeFrame::new(7, serial, iosurface).unwrap())
            .unwrap();

        if frame_first {
            assert!(state.register_native_token(7));
            state.drain_native_frames();
        }
        match state.defer_native_commit(7, serial, deferred(surface.0)) {
            Defer::Ready(ready) => state.finish_native(ready),
            Defer::Waiting if !frame_first => {}
            result => panic!(
                "unexpected native join result: {}",
                match result {
                    Defer::Ready(_) => "ready",
                    Defer::Reuse(_) => "reuse",
                    Defer::Waiting => "waiting",
                }
            ),
        }
        if !frame_first {
            state.drain_native_frames();
        }

        assert!(receipt.try_complete().unwrap().is_none());
        state.engine.presenter_mut().poll_events();
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Displayed
        );
        let committed = state
            .engine
            .scene
            .get(surface)
            .unwrap()
            .buffer
            .as_ref()
            .unwrap();
        assert_eq!((committed.tex_w, committed.tex_h), (3, 2));
        assert_eq!(committed.format, Format::Argb8888);
        assert!(committed.gpu);
    }

    #[test]
    fn protocol_only_native_frame_presents_when_frame_arrives_first() {
        assert_protocol_frame_presents(true);
    }

    #[test]
    fn protocol_only_native_frame_presents_when_commit_arrives_first() {
        assert_protocol_frame_presents(false);
    }

    #[test]
    fn joins_both_arrival_orders_exactly() {
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        let key = Key {
            token: NonZeroU64::new(7).unwrap(),
            serial: NonZeroU64::new(11).unwrap(),
        };
        let (ready, evicted) = join.defer(key, deferred(1));
        assert!(matches!(ready, Defer::Waiting) && evicted.is_empty());
        let (frame, _) = published(&sender, &join.ingress, 7, 11);
        assert!(join.ingest(frame).is_some());

        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(NonZeroU64::new(8).unwrap()));
        let (frame, _) = published(&sender, &join.ingress, 8, 12);
        assert!(join.ingest(frame).is_none());
        let key = Key {
            token: NonZeroU64::new(8).unwrap(),
            serial: NonZeroU64::new(12).unwrap(),
        };
        assert!(matches!(join.defer(key, deferred(2)).0, Defer::Ready(_)));
    }

    #[test]
    fn pending_external_commit_joins_the_next_frame_in_both_arrival_orders() {
        let token = NonZeroU64::new(17).unwrap();
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        let (commit, _file) = external(1, 17, 31);
        assert!(matches!(
            join.defer_pending(token, commit).0,
            Defer::Waiting
        ));
        let (frame, _) = published(&sender, &join.ingress, 17, 31);
        let ready = join.ingest(frame).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(1));
        assert_eq!(ready.frame.serial.get(), 31);

        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(token));
        let (frame, _) = published(&sender, &join.ingress, 17, 32);
        assert!(join.ingest(frame).is_none());
        let (commit, _file) = external(2, 17, 32);
        let ready = join.defer_pending(token, commit).0;
        let Defer::Ready(ready) = ready else {
            panic!("stored frame must satisfy pending external commit");
        };
        assert_eq!(ready.deferred.surface, SurfaceId(2));
        assert_eq!(ready.frame.serial.get(), 32);
    }

    #[test]
    fn repeated_pending_commits_and_frames_keep_fifo_generation_semantics() {
        let token = NonZeroU64::new(23).unwrap();
        let (sender, ingress) = native_frames(3).unwrap();
        let mut join = NativeState::new(ingress);
        let (first_commit, _first_file) = external(1, 23, 7);
        let (second_commit, _second_file) = external(2, 23, 8);
        assert!(join.defer_pending(token, first_commit).1.is_empty());
        let (result, discarded) = join.defer_pending(token, second_commit);
        assert!(matches!(result, Defer::Waiting));
        assert!(discarded.is_empty());
        assert_eq!(join.pending.len(), 1);
        assert_eq!(join.pending[&token].len(), 2);

        let (first, first_receipt) = published(&sender, &join.ingress, 23, 7);
        let ready = join.ingest(first).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(1));
        assert!(first_receipt.try_complete().unwrap().is_none());
        let (second, second_receipt) = published(&sender, &join.ingress, 23, 8);
        let ready = join.ingest(second).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(2));
        assert!(second_receipt.try_complete().unwrap().is_none());

        let (older, older_receipt) = published(&sender, &join.ingress, 24, 9);
        let (newer, newer_receipt) = published(&sender, &join.ingress, 24, 10);
        assert!(join.register(NonZeroU64::new(24).unwrap()));
        assert!(join.ingest(older).is_none());
        assert!(join.ingest(newer).is_none());
        let (third, _third_file) = external(3, 24, 9);
        let ready = join.defer_pending(NonZeroU64::new(24).unwrap(), third).0;
        let Defer::Ready(ready) = ready else {
            panic!("oldest stored frame must satisfy first pending external commit");
        };
        assert_eq!(ready.frame.serial.get(), 9);
        assert!(older_receipt.try_complete().unwrap().is_none());
        let (fourth, _fourth_file) = external(4, 24, 10);
        let ready = join.defer_pending(NonZeroU64::new(24).unwrap(), fourth).0;
        let Defer::Ready(ready) = ready else {
            panic!("second stored frame must satisfy second pending external commit");
        };
        assert_eq!(ready.frame.serial.get(), 10);
        assert!(newer_receipt.try_complete().unwrap().is_none());
    }

    #[test]
    fn repeated_same_surface_replacements_join_one_frame_to_latest_commit() {
        let token = NonZeroU64::new(25).unwrap();
        let (sender, ingress) = native_frames(4).unwrap();
        let mut join = NativeState::new(ingress);
        let (base, _file) = external(4, 25, 1);
        let shared = base.external.unwrap();

        for generation in 1..=3 {
            if generation > 1 {
                let discarded = join.replace(SurfaceId(4));
                assert_eq!(discarded.len(), 1);
            }
            let mut commit = deferred(4);
            commit.external = Some(shared.clone());
            commit.min_size.0 = Some(generation);
            assert!(matches!(
                join.defer_pending(token, commit).0,
                Defer::Waiting
            ));
        }

        let queue = &join.pending[&token];
        assert_eq!(queue.len(), 3);
        let latest = queue
            .back()
            .unwrap()
            .deferred
            .as_ref()
            .expect("latest replacement must remain pending");
        assert_eq!(latest.min_size.0, Some(3));

        let (frame, receipt) = published(&sender, &join.ingress, 25, 1);
        let ready = join
            .ingest(frame)
            .expect("the produced frame must join the latest replacement");
        assert_eq!(ready.deferred.surface, SurfaceId(4));
        assert_eq!(ready.deferred.min_size.0, Some(3));
        assert!(join.pending.is_empty());
        assert!(receipt.try_complete().unwrap().is_none());
    }

    #[test]
    fn overwritten_serial_discards_old_frame_and_keeps_latest_commit() {
        let token = NonZeroU64::new(39).unwrap();
        let (sender, ingress) = native_frames(3).unwrap();
        let mut join = NativeState::new(ingress);
        let (commit, file) = external(10, 39, 0);
        assert!(join.defer_pending(token, commit).1.is_empty());
        publish_header(&file, 39, 2);

        let (stale, stale_receipt) = published(&sender, &join.ingress, 39, 1);
        assert!(join.ingest(stale).is_none());
        assert_eq!(
            stale_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Decreasing
        );
        assert_eq!(join.pending[&token].len(), 1);

        let (latest, latest_receipt) = published(&sender, &join.ingress, 39, 2);
        let ready = join.ingest(latest).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(10));
        assert!(latest_receipt.try_complete().unwrap().is_none());
    }

    #[test]
    fn token_change_preserves_the_old_pending_frame_tombstone() {
        let first = NonZeroU64::new(25).unwrap();
        let second = NonZeroU64::new(26).unwrap();
        let (sender, ingress) = native_frames(4).unwrap();
        let mut join = NativeState::new(ingress);
        let (first_commit, _first_file) = external(4, 25, 1);
        let (second_commit, _second_file) = external(4, 26, 1);
        assert!(join.defer_pending(first, first_commit).1.is_empty());
        assert_eq!(join.replace(SurfaceId(4)).len(), 1);
        assert!(join.defer_pending(second, second_commit).1.is_empty());

        let (late, late_receipt) = published(&sender, &join.ingress, 25, 1);
        assert!(join.ingest(late).is_none());
        assert_eq!(
            late_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        let (current, current_receipt) = published(&sender, &join.ingress, 26, 1);
        let ready = join.ingest(current).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(4));
        assert!(current_receipt.try_complete().unwrap().is_none());
    }

    #[test]
    fn leaving_native_frames_preserves_the_pending_frame_tombstone() {
        let token = NonZeroU64::new(27).unwrap();
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        let (commit, _file) = external(4, 27, 1);
        assert!(join.defer_pending(token, commit).1.is_empty());
        assert_eq!(join.replace(SurfaceId(4)).len(), 1);

        let (late, receipt) = published(&sender, &join.ingress, 27, 1);
        assert!(join.ingest(late).is_none());
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
    }


    #[path = "middle.rs"]
    mod middle;
    #[path = "late.rs"]
    mod late;
}
