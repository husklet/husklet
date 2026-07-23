use super::*;

/// Rapidly stand up and drop sessions in a loop; the shared account must return to baseline on EVERY
/// cycle (no residency accumulating across session lifetimes). A long-lived session coexists throughout —
/// its fixed contribution must be all that remains after each ephemeral session drops, proving the churn
/// neither leaks into nor steals from a concurrent connection. Also run several threads churning sessions
/// at once, then confirm the account is exactly the long-lived contribution.
#[test]
fn session_churn_no_leak() {
    with_timeout(120, || {
        const CYCLES: usize = 500;
        const PER_SESSION: u32 = 12;
        const SIZE: u64 = 2048;

        let global = GlobalLedger::new(64 << 30, 1 << 24);

        // A long-lived session that stays up across the whole churn with a fixed, known contribution.
        let mut persistent = sink_on(&global);
        persistent
            .submit(&[buffer(1, 4096), write(1, 0x77, 4096)])
            .unwrap();
        let persistent_bytes = persistent.session().residency_bytes();
        let persistent_objs = persistent.session().object_count();
        assert_eq!(persistent_bytes, 4096);

        // Single-threaded rapid churn: each cycle must return the account to EXACTLY the persistent floor.
        for c in 0..CYCLES {
            {
                let mut s = sink_on(&global);
                let create: Vec<Cmd> = (1..=PER_SESSION).map(|id| buffer(id, SIZE)).collect();
                s.submit(&create).unwrap();
                assert_eq!(s.session().object_count(), PER_SESSION as u64);
                // While this session is live the account is the floor PLUS this session's contribution.
                assert_eq!(
                    global.residency_bytes(),
                    persistent_bytes + (PER_SESSION as u64) * SIZE,
                    "cycle {c}: account is floor + live session",
                );
                // ...and the persistent session is untouched by the ephemeral one (overlapping id 1).
                assert_eq!(
                    persistent.read_buffer(BufferId(1), 0, 4096).unwrap(),
                    vec![0x77; 4096]
                );
                // drop `s` here.
            }
            // Back to exactly the persistent floor — the ephemeral session leaked nothing.
            assert_eq!(
                global.residency_bytes(),
                persistent_bytes,
                "cycle {c}: residency did not return to the floor"
            );
            assert_eq!(
                global.object_count(),
                persistent_objs,
                "cycle {c}: object count did not return to the floor"
            );
        }

        // Concurrent churn: several threads rapidly create+drop sessions at once against the same account.
        const THREADS: u32 = 8;
        const CONCURRENT_CYCLES: usize = 100;
        thread::scope(|scope| {
            for _ in 0..THREADS {
                let global = &global;
                scope.spawn(move || {
                    for _ in 0..CONCURRENT_CYCLES {
                        let mut s = sink_on(global);
                        let create: Vec<Cmd> =
                            (1..=PER_SESSION).map(|id| buffer(id, SIZE)).collect();
                        s.submit(&create).unwrap();
                        assert_eq!(s.session().object_count(), PER_SESSION as u64);
                        // drop at end of iteration.
                    }
                });
            }
        });

        // After all the concurrent churn, the ONLY thing left on the account is the persistent session.
        assert_eq!(
            global.residency_bytes(),
            persistent_bytes,
            "concurrent churn leaked into the shared account"
        );
        assert_eq!(global.object_count(), persistent_objs);
        // The persistent session is still intact and correct.
        assert_eq!(
            persistent.read_buffer(BufferId(1), 0, 4096).unwrap(),
            vec![0x77; 4096]
        );
        drop(persistent);
        assert_eq!(
            global.residency_bytes(),
            0,
            "final teardown returns the account to baseline"
        );
        assert_eq!(global.object_count(), 0);
    });
}

// =================================================================================================
// 5. mixed_load — real compute concurrent with create/destroy churn, all results correct
// =================================================================================================
