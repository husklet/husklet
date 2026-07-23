use super::*;

/// Many threads charge and refund the ONE shared `GlobalLedger` concurrently. At a barrier where every
/// thread has a KNOWN live contribution, the shared account's residency + object_count are EXACTLY the
/// sum — a single lost or double charge under mutex contention would make them differ. Then every thread
/// runs many create/destroy + duplicate-create-rollback (#232) cycles that each return its session to the
/// same known contribution, proving no drift accumulates across churn under contention. Finally every
/// session tears down and the shared account is EXACTLY zero.
#[test]
fn account_ledger_under_concurrency() {
    with_timeout(120, || {
        const THREADS: u32 = 16;
        const M: u32 = 64; // buffers each thread holds live at the barrier
        const SIZE: u64 = 1024;
        const CYCLES: usize = 200;

        let global = GlobalLedger::new(64 << 30, 1 << 24);
        // Barriers gate the exact-sum assertion: workers charge M buffers, hit `charged`, then WAIT on
        // `release` (holding their contribution live) while the main thread reads the total.
        let charged = Arc::new(Barrier::new((THREADS + 1) as usize));
        let released = Arc::new(Barrier::new((THREADS + 1) as usize));

        thread::scope(|scope| {
            for t in 0..THREADS {
                let global = &global;
                let charged = Arc::clone(&charged);
                let released = Arc::clone(&released);
                scope.spawn(move || {
                    let mut s = sink_on(global);
                    // Each thread's ids are disjoint from other threads' only conceptually — the ledger is
                    // per-session, so overlapping ids are fine; use 1..=M.
                    let create: Vec<Cmd> = (1..=M).map(|id| buffer(id, SIZE)).collect();
                    s.submit(&create).expect("charge M buffers");
                    assert_eq!(
                        s.session().residency_bytes(),
                        (M as u64) * SIZE,
                        "thread {t}: own charge exact"
                    );
                    assert_eq!(s.session().object_count(), M as u64);

                    // All threads now hold exactly M*SIZE bytes / M objects live.
                    charged.wait();
                    // Hold the contribution live while the main thread reads the global total.
                    released.wait();

                    // CHURN: repeatedly destroy-then-recreate one id AND attempt an illegal duplicate
                    // create that must NACK and roll back — under contention on the shared mutex. After
                    // each cycle the session's contribution must be byte-identical to before.
                    let baseline_bytes = s.session().residency_bytes();
                    let baseline_objs = s.session().object_count();
                    for c in 0..CYCLES {
                        // Legal recycle of id 1: destroy then recreate a different size in one frame.
                        let newsize = SIZE + ((c as u64 % 8) * 8);
                        s.submit(&[Cmd::DestroyBuffer(1), buffer(1, newsize)])
                            .expect("recycle id 1");
                        // Illegal duplicate create over a still-live id → DuplicateId, rolled back, no drift.
                        let err = s.submit(&[buffer(2, 999)]).unwrap_err();
                        assert!(
                            matches!(
                                err,
                                GpuError::DuplicateId {
                                    kind: "buffer",
                                    id: 2
                                }
                            ),
                            "thread {t}: expected DuplicateId"
                        );
                        // Restore id 1 to SIZE so the running total is exactly the baseline again.
                        s.submit(&[Cmd::DestroyBuffer(1), buffer(1, SIZE)])
                            .expect("restore id 1");
                        assert_eq!(
                            s.session().residency_bytes(),
                            baseline_bytes,
                            "thread {t} cycle {c}: bytes drifted"
                        );
                        assert_eq!(
                            s.session().object_count(),
                            baseline_objs,
                            "thread {t} cycle {c}: objects drifted"
                        );
                    }

                    // Tear the whole contribution down before the thread exits.
                    let destroy: Vec<Cmd> = (1..=M).map(Cmd::DestroyBuffer).collect();
                    s.submit(&destroy).expect("destroy all");
                    assert_eq!(s.session().residency_bytes(), 0);
                    // `s` drops → refunds zero (already drained).
                });
            }

            // Main thread: at the barrier every worker holds EXACTLY M*SIZE bytes / M objects live.
            charged.wait();
            let expect_bytes = (THREADS as u64) * (M as u64) * SIZE;
            let expect_objs = (THREADS as u64) * (M as u64);
            assert_eq!(
                global.residency_bytes(),
                expect_bytes,
                "shared residency is not the exact sum of all sessions — a charge was lost or doubled under contention",
            );
            assert_eq!(
                global.object_count(),
                expect_objs,
                "shared object count is not the exact sum — a charge was lost or doubled under contention",
            );
            released.wait();
        });

        // Every session drained + dropped: the shared account is EXACTLY zero after all the churn.
        assert_eq!(
            global.residency_bytes(),
            0,
            "shared residency drifted from baseline after concurrent churn"
        );
        assert_eq!(
            global.object_count(),
            0,
            "shared object count drifted from baseline after concurrent churn"
        );
    });
}

// =================================================================================================
// 4. session_churn_no_leak — rapid create+drop of many sessions returns to baseline every cycle
// =================================================================================================
