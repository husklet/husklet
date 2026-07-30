use super::*;
    #[test]
    fn shared_token_across_surfaces_prevents_unsafe_coalescing() {
        let token = NonZeroU64::new(28).unwrap();
        let (sender, ingress) = native_frames(4).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(token));
        assert!(join.register(token));
        let (first, _first_file) = external(4, 28, 1);
        let (collision, _collision_file) = external(5, 28, 1);
        assert!(join.defer_pending(token, first).1.is_empty());
        assert!(join.defer_pending(token, collision).1.is_empty());

        let (frame, receipt) = published(&sender, &join.ingress, 28, 1);
        assert!(join.ingest(frame).is_none());
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        assert!(!join.pending.contains_key(&token));
        assert_eq!(join.take_discarded().len(), 2);
        assert_eq!(join.registrations.get(&token), Some(&2));
        assert!(join.poisoned.contains(&token));
        assert!(!join.register(token));
        join.unregister(token);
        assert!(join.poisoned.contains(&token));
        join.unregister(token);
        assert!(!join.poisoned.contains(&token));
    }

    #[test]
    fn ambiguity_drains_every_token_queue_before_owner_retirement() {
        let token = NonZeroU64::new(44).unwrap();
        let (sender, ingress) = native_frames(6).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(token));
        assert!(join.register(token));

        let exact = Key {
            token,
            serial: NonZeroU64::new(9).unwrap(),
        };
        assert!(join.defer(exact, deferred(9)).1.is_empty());
        let (first, _first_file) = external(4, 44, 3);
        let (collision, _collision_file) = external(5, 44, 3);
        assert!(join.defer_pending(token, first).1.is_empty());
        assert!(join.defer_pending(token, collision).1.is_empty());
        // Model a previously queued frame explicitly. Normal ingress cannot interleave while
        // `resolve_pending` runs, but disconnect/cancellation queue state must remain safe regardless.
        let (other, other_receipt) = published(&sender, &join.ingress, 44, 2);
        let other_key = Key {
            token,
            serial: NonZeroU64::new(2).unwrap(),
        };
        join.frames.insert(other_key, other);
        join.frame_order.push_back(other_key);
        let (ambiguous, ambiguous_receipt) = published(&sender, &join.ingress, 44, 3);
        assert!(join.ingest(ambiguous).is_none());

        assert_eq!(
            other_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        assert_eq!(
            ambiguous_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        assert_eq!(join.take_discarded().len(), 3);
        assert!(join.take_discarded().is_empty());
        assert!(!join.frames.keys().any(|key| key.token == token));
        assert!(!join.commits.keys().any(|key| key.token == token));
        assert!(!join.pending.contains_key(&token));
        assert!(join.poisoned.contains(&token));

        join.unregister(token);
        join.unregister(token);
        assert!(!join.frames.keys().any(|key| key.token == token));
        assert!(!join.commits.keys().any(|key| key.token == token));
        assert!(!join.pending.contains_key(&token));
        assert!(!join.active_tokens.contains(&token));
        assert!(!join.poisoned.contains(&token));
    }

    #[test]
    fn destroying_terminal_buffer_without_a_frame_settles_its_tombstone() {
        let token = NonZeroU64::new(42).unwrap();
        let (_sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(token));
        let (commit, _file) = external(44, 42, 0);
        let external = commit.external.as_ref().unwrap().weak();
        assert!(join.defer_pending(token, commit).1.is_empty());
        assert_eq!(join.cancel(SurfaceId(44)).len(), 1);
        assert_eq!(join.pending[&token].len(), 1);

        join.settle_destroyed(token, &external);
        join.unregister(token);

        assert!(!join.pending.contains_key(&token));
        assert!(!join.active_tokens.contains(&token));
        assert!(!join.registrations.contains_key(&token));
    }

    #[test]
    fn destroying_active_buffer_does_not_drop_a_submitted_generation() {
        let token = NonZeroU64::new(43).unwrap();
        let (_sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(token));
        let (commit, _file) = external(44, 43, 1);
        let external = commit.external.as_ref().unwrap().weak();
        assert!(join.defer_pending(token, commit).1.is_empty());

        join.settle_destroyed(token, &external);
        join.unregister(token);

        assert_eq!(join.pending[&token].len(), 1);
        assert!(join.closing.contains(&token));
    }

    #[test]
    fn exact_cancellation_is_consumed_only_by_its_exact_frame() {
        let token = NonZeroU64::new(32).unwrap();
        let canceled = Key {
            token,
            serial: NonZeroU64::new(5).unwrap(),
        };
        let (sender, ingress) = native_frames(3).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.cancel_key(canceled).is_empty());
        let (commit, _file) = external(7, 32, 6);
        assert!(join.defer_pending(token, commit).1.is_empty());

        let (canceled_frame, canceled_receipt) = published(&sender, &join.ingress, 32, 5);
        assert!(join.ingest(canceled_frame).is_none());
        assert_eq!(
            canceled_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        let (current, current_receipt) = published(&sender, &join.ingress, 32, 6);
        let ready = join.ingest(current).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(7));
        assert!(current_receipt.try_complete().unwrap().is_none());
    }

    #[test]
    fn three_token_rotation_preserves_each_superseded_association() {
        let tokens = [33_u64, 34, 35].map(|raw| NonZeroU64::new(raw).unwrap());
        let (sender, ingress) = native_frames(4).unwrap();
        let mut join = NativeState::new(ingress);
        let (first, _first_file) = external(8, 33, 1);
        let (second, _second_file) = external(8, 34, 1);
        let (third, _third_file) = external(8, 35, 1);
        assert!(join.defer_pending(tokens[0], first).1.is_empty());
        assert_eq!(join.replace(SurfaceId(8)).len(), 1);
        assert!(join.defer_pending(tokens[1], second).1.is_empty());
        assert_eq!(join.replace(SurfaceId(8)).len(), 1);
        assert!(join.defer_pending(tokens[2], third).1.is_empty());

        for token in [33_u64, 34] {
            let (late, receipt) = published(&sender, &join.ingress, token, 1);
            assert!(join.ingest(late).is_none());
            assert_eq!(
                receipt.try_complete().unwrap().unwrap().outcome,
                NativeFrameOutcome::Discarded
            );
        }
        let (current, receipt) = published(&sender, &join.ingress, 35, 1);
        let ready = join.ingest(current).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(8));
        assert!(receipt.try_complete().unwrap().is_none());
    }

    #[test]
    fn rotating_tokens_rejoin_each_latest_same_surface_generation() {
        let tokens = [36_u64, 37, 38].map(|raw| NonZeroU64::new(raw).unwrap());
        let (sender, ingress) = native_frames(6).unwrap();
        let mut join = NativeState::new(ingress);

        let (first, _first_file) = external(9, 36, 1);
        assert!(join.defer_pending(tokens[0], first).1.is_empty());
        for token in [tokens[1], tokens[2]] {
            assert_eq!(join.replace(SurfaceId(9)).len(), 1);
            let (commit, _file) = external(9, token.get(), 1);
            assert!(join.defer_pending(token, commit).1.is_empty());
        }

        for (serial, token) in tokens.into_iter().enumerate() {
            let discarded = join.replace(SurfaceId(9));
            assert!(discarded.len() <= 1);
            let (mut latest, _file) = external(9, token.get(), 1);
            latest.min_size.0 = Some(serial as i32);
            assert!(join.defer_pending(token, latest).1.is_empty());

            let (frame, receipt) = published(&sender, &join.ingress, token.get(), 1);
            let ready = join
                .ingest(frame)
                .expect("a returning token must join its latest generation");
            assert_eq!(ready.deferred.surface, SurfaceId(9));
            assert_eq!(ready.deferred.min_size.0, Some(serial as i32));
            assert!(receipt.try_complete().unwrap().is_none());
        }
    }

    #[test]
    fn pending_commit_cancellation_is_exact_before_and_after_commit() {
        let token = NonZeroU64::new(29).unwrap();
        let canceled = Key {
            token,
            serial: NonZeroU64::new(4).unwrap(),
        };

        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(token));
        assert!(join.cancel_key(canceled).is_empty());
        let (result, discarded) = join.defer(canceled, deferred(1));
        assert!(matches!(result, Defer::Waiting));
        assert_eq!(discarded.len(), 1);
        let (frame, receipt) = published(&sender, &join.ingress, 29, 5);
        assert!(join.ingest(frame).is_none());
        assert!(receipt.try_complete().unwrap().is_none());

        let (sender, ingress) = native_frames(2).unwrap();
        let mut join = NativeState::new(ingress);
        assert!(join.register(token));
        assert!(join.defer(canceled, deferred(2)).1.is_empty());
        let discarded = join.cancel_key(canceled);
        assert_eq!(discarded.len(), 1);
        assert_eq!(discarded[0].surface, SurfaceId(2));
        let (frame, receipt) = published(&sender, &join.ingress, 29, 4);
        assert!(join.ingest(frame).is_none());
        assert_eq!(
            receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
    }

    #[test]
    fn later_cancellation_cannot_consume_an_earlier_pending_generation() {
        let token = NonZeroU64::new(30).unwrap();
        let (sender, ingress) = native_frames(3).unwrap();
        let mut join = NativeState::new(ingress);
        let (first, _first_file) = external(1, 30, 1);
        let (second, _second_file) = external(2, 30, 3);
        assert!(join.defer_pending(token, first).1.is_empty());
        assert!(join.defer_pending(token, second).1.is_empty());

        let surface = hl_iosurface::Surface::new_bgra(2, 2).unwrap();
        let receipt = sender
            .publish(NativeFrame::new(30, 1, surface).unwrap())
            .unwrap();
        sender.cancel(30, 2).unwrap();
        let cancellation = join.take_cancellation().unwrap().unwrap();
        let prior = join
            .take_before(cancellation.token, cancellation.serial)
            .unwrap();
        let ready = join.ingest(prior).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(1));
        assert!(receipt.try_complete().unwrap().is_none());

        let discarded = join.cancel_key(Key {
            token: cancellation.token,
            serial: cancellation.serial,
        });
        assert!(discarded.is_empty());
        assert_eq!(join.pending[&token].len(), 1);

        let (canceled, canceled_receipt) = published(&sender, &join.ingress, 30, 2);
        assert!(join.ingest(canceled).is_none());
        assert_eq!(
            canceled_receipt.try_complete().unwrap().unwrap().outcome,
            NativeFrameOutcome::Discarded
        );
        let (current, current_receipt) = published(&sender, &join.ingress, 30, 3);
        let ready = join.ingest(current).unwrap();
        assert_eq!(ready.deferred.surface, SurfaceId(2));
        assert!(current_receipt.try_complete().unwrap().is_none());
        assert!(join.pending.is_empty());
    }

