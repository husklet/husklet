use super::*;
    #[test]
    fn stale_external_serial_waits_for_a_new_frame() {
        let token = NonZeroU64::new(31).unwrap();
        let (_sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        join.last_joined.insert(token, NonZeroU64::new(12).unwrap());
        assert!(join.external_is_stale(token, NonZeroU64::new(12).unwrap()));
        assert!(join.external_is_stale(token, NonZeroU64::new(11).unwrap()));
        assert!(!join.external_is_stale(token, NonZeroU64::new(13).unwrap()));
    }

    #[test]
    fn replacing_or_destroying_a_pending_surface_discards_its_late_frame() {
        let token = NonZeroU64::new(37).unwrap();
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        let (first, _first_file) = external(44, 37, 1);
        let (second, _second_file) = external(45, 37, 2);
        assert!(join.defer_pending(token, first).1.is_empty());
        assert!(join.defer_pending(token, second).1.is_empty());
        let discarded = join.cancel(SurfaceId(44));
        assert_eq!(discarded.len(), 1);
        assert_eq!(discarded[0].surface, SurfaceId(44));
        let (late, receipt) = published(&sender, &join.ingress, 37, 1);
        assert!(join.ingest(late).is_none());
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        let (current, current_receipt) = published(&sender, &join.ingress, 37, 2);
        let ready = join.ingest(current).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(45));
        assert!(current_receipt.try_complete().unwrap().is_none());

        assert!(join.defer_pending(token, deferred(46)).1.is_empty());
        let retired = join.retire(token);
        assert_eq!(retired.len(), 1);
        assert!(join.pending.is_empty());
        assert!(!join.active_tokens.contains(&token));
    }

    #[test]
    fn pending_external_state_is_bounded_by_registered_tokens() {
        let (_sender, ingress) = native_frames(1).unwrap();
        let mut join = NativeState::new(ingress);
        for raw in 1..=join.token_capacity as u64 {
            let token = NonZeroU64::new(raw).unwrap();
            assert!(join.register(token));
            assert!(join.defer_pending(token, deferred(raw as u32)).1.is_empty());
        }
        assert_eq!(join.pending.len(), join.token_capacity);
        let overflow = NonZeroU64::new(join.token_capacity as u64 + 1).unwrap();
        assert!(!join.register(overflow));
        assert_eq!(join.pending.len(), join.token_capacity);
        let all = join.disconnect();
        assert_eq!(all.len(), join.token_capacity);
        assert!(join.pending.is_empty());
    }

    #[test]
    fn pending_commit_capacity_is_independent_per_token() {
        let (_sender, ingress) = native_frames(1).unwrap();
        let mut join = NativeState::new(ingress);
        let first = Key {
            token: NonZeroU64::new(1).unwrap(),
            serial: NonZeroU64::new(1).unwrap(),
        };
        let second = Key {
            token: NonZeroU64::new(2).unwrap(),
            serial: NonZeroU64::new(2).unwrap(),
        };
        assert!(join.defer(first, deferred(4)).1.is_empty());
        assert!(join.defer(second, deferred(5)).1.is_empty());
        assert_eq!(join.commits.len(), 2);
    }

    #[test]
    fn multiple_serials_wait_independently_and_capacity_evicts_oldest() {
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(NonZeroU64::new(9).unwrap()));
        let (first, first_receipt) = published(&sender, &join.ingress, 9, 2);
        join.ingest(first);
        let (second, second_receipt) = published(&sender, &join.ingress, 9, 3);
        join.ingest(second);
        assert!(first_receipt.try_complete().unwrap().is_none());
        assert!(second_receipt.try_complete().unwrap().is_none());

        let first_ready = join
            .defer(
                Key {
                    token: NonZeroU64::new(9).unwrap(),
                    serial: NonZeroU64::new(2).unwrap(),
                },
                deferred(2),
            )
            .0;
        assert!(matches!(first_ready, Defer::Ready(_)));
        let second_ready = join
            .defer(
                Key {
                    token: NonZeroU64::new(9).unwrap(),
                    serial: NonZeroU64::new(3).unwrap(),
                },
                deferred(3),
            )
            .0;
        assert!(matches!(second_ready, Defer::Ready(_)));

        let (fourth, fourth_receipt) = published(&sender, &join.ingress, 9, 4);
        join.ingest(fourth);
        let (fifth, fifth_receipt) = published(&sender, &join.ingress, 9, 5);
        join.ingest(fifth);
        let (sixth, _) = published(&sender, &join.ingress, 9, 6);
        join.ingest(sixth);
        assert_eq!(
            fourth_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Capacity
        );
        assert!(fifth_receipt.try_complete().unwrap().is_none());
    }

    #[test]
    fn duplicate_decreasing_and_teardown_settle_leases() {
        let (sender, ingress) = native_frames(4).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(NonZeroU64::new(9).unwrap()));
        let (frame, _) = published(&sender, &join.ingress, 9, 3);
        join.ingest(frame);
        let (duplicate, duplicate_receipt) = published(&sender, &join.ingress, 9, 3);
        join.ingest(duplicate);
        assert_eq!(
            duplicate_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Duplicate
        );
        let (decreasing, decreasing_receipt) = published(&sender, &join.ingress, 9, 1);
        join.ingest(decreasing);
        assert_eq!(
            decreasing_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Decreasing
        );
        join.retire(NonZeroU64::new(9).unwrap());
    }

    #[test]
    fn triple_buffer_reuse_and_duplicate_reattach_are_deterministic() {
        let (sender, ingress) = native_frames(4).unwrap();
        let mut join = NativeState::new(ingress);
        for token in 1..=3 {
            assert!(join.register(NonZeroU64::new(token).unwrap()));
        }
        for (token, serial, surface) in [(1, 1, 1), (2, 1, 2), (3, 1, 3), (1, 2, 1)] {
            let (frame, _) = published(&sender, &join.ingress, token, serial);
            assert!(join.ingest(frame).is_none());
            let key = Key {
                token: NonZeroU64::new(token).unwrap(),
                serial: NonZeroU64::new(serial).unwrap(),
            };
            assert!(matches!(
                join.defer(key, deferred(surface)).0,
                Defer::Ready(_)
            ));
        }
        let repeated = Key {
            token: NonZeroU64::new(1).unwrap(),
            serial: NonZeroU64::new(2).unwrap(),
        };
        assert!(matches!(
            join.defer(repeated, deferred(1)).0,
            Defer::Reuse(_)
        ));
        let stale = Key {
            token: NonZeroU64::new(1).unwrap(),
            serial: NonZeroU64::new(1).unwrap(),
        };
        let (result, discarded) = join.defer(stale, deferred(1));
        assert!(matches!(result, Defer::Waiting));
        assert_eq!(discarded.len(), 1);
    }

    #[test]
    fn discarded_commit_settles_every_callback_with_compositor_time() {
        let mut settled = Vec::new();
        settle_callbacks([1, 2, 3], 8_765_432_100, |callback, time_ms| {
            settled.push((callback, time_ms));
        });
        assert_eq!(settled, [(1, 8_765), (2, 8_765), (3, 8_765)]);
    }

    #[test]
    fn publishing_preserves_fifo_until_capacity_and_reports_closed_channels() {
        let (sender, ingress) = native_frames(1).unwrap();
        let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
        let first = sender
            .publish(NativeFrame::new(1, 1, surface).unwrap())
            .unwrap();
        let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
        let second = sender
            .publish(NativeFrame::new(1, 2, surface).unwrap())
            .unwrap();
        assert_eq!(
            first.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Capacity
        );
        assert!(second.try_complete().unwrap().is_none());
        assert_eq!(ingress.try_next().unwrap().unwrap().serial.get(), 2);

        drop(ingress);
        let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
        let error = sender
            .publish(NativeFrame::new(1, 3, surface).unwrap())
            .unwrap_err();
        assert_eq!(error.reason, NativeFramePublishFailure::Closed);
    }

    #[test]
    fn bounded_fifo_keeps_later_exact_serials_matchable() {
        let (sender, ingress) = native_frames(3).unwrap();
        let mut receipts = Vec::new();
        for serial in 1..=6 {
            let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
            receipts.push(
                sender
                    .publish(NativeFrame::new(9, serial, surface).unwrap())
                    .unwrap(),
            );
        }
        for receipt in &receipts[..3] {
            assert_eq!(
                receipt.try_complete().unwrap().unwrap().outcome,
                NativeFrameOutcome::Capacity
            );
        }
        assert!(receipts[3..]
            .iter()
            .all(|receipt| receipt.try_complete().unwrap().is_none()));

        let mut join = NativeState::new(ingress);
        assert!(join.register(NonZeroU64::new(9).unwrap()));
        while let Some(frame) = join.take_ingress().unwrap() {
            join.ingest(frame);
        }
        let ready = join
            .defer(
                Key {
                    token: NonZeroU64::new(9).unwrap(),
                    serial: NonZeroU64::new(6).unwrap(),
                },
                deferred(6),
            )
            .0;
        assert!(matches!(&ready, Defer::Ready(_)));
        assert!(receipts[5].try_complete().unwrap().is_none());
    }

    #[test]
    fn distinct_chrome_buffers_are_not_evicted_by_serial_capacity() {
        let (sender, ingress) = native_frames(3).unwrap();
        let mut receipts = Vec::new();
        for token in 1..=6 {
            let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
            receipts.push(
                sender
                    .publish(NativeFrame::new(token, token, surface).unwrap())
                    .unwrap(),
            );
        }
        assert!(receipts
            .iter()
            .all(|receipt| receipt.try_complete().unwrap().is_none()));

        let mut join = NativeState::new(ingress);
        for token in 1..=6 {
            assert!(join.register(NonZeroU64::new(token).unwrap()));
        }
        while let Some(frame) = join.take_ingress().unwrap() {
            join.ingest(frame);
        }
        for token in 1..=6 {
            assert!(matches!(
                join.defer(
                    Key {
                        token: NonZeroU64::new(token).unwrap(),
                        serial: NonZeroU64::new(token).unwrap(),
                    },
                    deferred(token as u32),
                )
                .0,
                Defer::Ready(_)
            ));
        }
    }

    #[test]
    fn producer_disconnect_discards_frames_and_returns_waiting_commits() {
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(NonZeroU64::new(1).unwrap()));
        let (frame, receipt) = published(&sender, &join.ingress, 1, 1);
        join.ingest(frame);
        let waiting = Key {
            token: NonZeroU64::new(2).unwrap(),
            serial: NonZeroU64::new(1).unwrap(),
        };
        assert!(matches!(join.defer(waiting, deferred(9)).0, Defer::Waiting));
        drop(sender);

        assert!(matches!(
            join.take_ingress(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        let deferred = join.disconnect();
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].surface, SurfaceId(9));
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
    }

    #[test]
    fn superseded_commit_rejects_its_late_frame() {
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        let key = Key {
            token: NonZeroU64::new(7).unwrap(),
            serial: NonZeroU64::new(3).unwrap(),
        };
        assert!(matches!(join.defer(key, deferred(44)).0, Defer::Waiting));
        assert_eq!(join.cancel(SurfaceId(44)).len(), 1);

        let (late, receipt) = published(&sender, &join.ingress, 7, 3);
        assert!(join.ingest(late).is_none());
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        assert!(join.frames.is_empty());
        assert!(join.commits.is_empty());
    }

    #[test]
    fn failed_submission_cancels_the_exact_deferred_commit() {
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        let failed = Key {
            token: NonZeroU64::new(7).unwrap(),
            serial: NonZeroU64::new(3).unwrap(),
        };
        let unrelated = Key {
            token: NonZeroU64::new(7).unwrap(),
            serial: NonZeroU64::new(4).unwrap(),
        };
        assert!(matches!(join.defer(failed, deferred(44)).0, Defer::Waiting));
        assert!(matches!(
            join.defer(unrelated, deferred(45)).0,
            Defer::Waiting
        ));

        sender.cancel(7, 3).unwrap();
        let cancellation = join.take_cancellation().unwrap().unwrap();
        let settled = join.cancel_key(Key {
            token: cancellation.token,
            serial: cancellation.serial,
        });

        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].surface, SurfaceId(44));
        assert!(!join.commits.contains_key(&failed));
        assert!(join.commits.contains_key(&unrelated));
    }

    #[test]
    fn cancellation_before_commit_terminally_discards_that_later_commit() {
        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        sender.cancel(9, 8).unwrap();
        let cancellation = join.take_cancellation().unwrap().unwrap();
        assert!(join
            .cancel_key(Key {
                token: cancellation.token,
                serial: cancellation.serial,
            })
            .is_empty());
        let key = Key {
            token: NonZeroU64::new(9).unwrap(),
            serial: NonZeroU64::new(8).unwrap(),
        };
        let (result, settled) = join.defer(key, deferred(52));
        assert!(matches!(result, Defer::Waiting));
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].surface, SurfaceId(52));
    }

    #[test]
    fn cancellation_ingress_backpressures_without_losing_an_exact_key() {
        let (sender, ingress) = native_frames(1).unwrap();
        for serial in 1..=256 {
            sender.cancel(1, serial).unwrap();
        }
        let blocked = sender.clone();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(0);
        let worker = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            blocked.cancel(1, 257).unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(
            done_rx.try_recv().is_err(),
            "the bounded producer must wait instead of dropping cancellation 257"
        );

        let first = ingress.try_cancel().unwrap().unwrap();
        assert_eq!((first.token.get(), first.serial.get()), (1, 1));
        done_rx.recv().unwrap();
        worker.join().unwrap();
        let mut serials = vec![first.serial.get()];
        while let Some(cancellation) = ingress.try_cancel().unwrap() {
            serials.push(cancellation.serial.get());
        }
        assert_eq!(serials, (1..=257).collect::<Vec<_>>());
    }

    #[test]
    fn unique_private_tokens_keep_history_bounded() {
        let (_sender, ingress) = native_frames(3).unwrap();
        let mut join = NativeState::new(ingress);
        let capacity = join.token_capacity;
        for token in 1..=capacity {
            assert!(join.register(NonZeroU64::new(token as u64).unwrap()));
        }
        assert!(!join.register(NonZeroU64::new(capacity as u64 + 1).unwrap()));
        assert!(join.last_frame.len() <= join.token_capacity);
        assert!(join.last_joined.len() <= join.token_capacity);
        assert!(join.active_tokens.len() <= join.token_capacity);
        join.retire(NonZeroU64::new(1).unwrap());
        assert!(join.register(NonZeroU64::new(capacity as u64 + 1).unwrap()));
        assert_eq!(join.active_tokens.len(), capacity);
    }

    #[test]
    fn native_metadata_requires_exact_geometry_and_valid_host_stride() {
        let surface = hl_iosurface::Surface::new_bgra(2, 3).unwrap();
        let (_, _, stride) = surface.dimensions();
        let exact = Metadata {
            width: 2,
            height: 3,
            stride: u32::try_from(stride).unwrap(),
            format: Format::Xrgb8888,
        };
        assert_eq!(exact.failure(surface.dimensions()), None);
        assert_eq!(exact.format, Format::Xrgb8888);
        assert_eq!(
            Metadata { width: 1, ..exact }.failure(surface.dimensions()),
            Some(ImportFailure::Width)
        );
        assert_eq!(
            Metadata { height: 2, ..exact }.failure(surface.dimensions()),
            Some(ImportFailure::Height)
        );
        // Guest storage may have a different aligned pitch because the GPU output is repacked.
        assert_eq!(
            Metadata {
                stride: exact.stride + 256,
                ..exact
            }
            .failure(surface.dimensions()),
            None
        );
        assert_eq!(
            exact.failure((1, exact.height as usize, exact.stride as usize)),
            Some(ImportFailure::Width)
        );
        assert_eq!(
            exact.failure((exact.width as usize, 1, exact.stride as usize)),
            Some(ImportFailure::Height)
        );
        assert_eq!(
            exact.failure((
                exact.width as usize,
                exact.height as usize,
                exact.width as usize * 4 - 1,
            )),
            Some(ImportFailure::Stride)
        );
        assert_eq!(
            exact.failure((
                exact.width as usize,
                exact.height as usize,
                exact.width as usize * 4 + 64,
            )),
            None
        );
    }
