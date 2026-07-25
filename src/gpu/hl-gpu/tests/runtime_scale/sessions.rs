use super::*;

#[test]
fn many_concurrent_sessions() {
    with_timeout(120, || {
        const THREADS: u32 = 32;
        const IDS: u32 = 16; // every thread uses ids 1..=IDS (fully overlapping id space)
        const ROUNDS: usize = 8;
        const SIZE: usize = 64;

        // A generously-bounded shared account (peak concurrent live = THREADS*IDS objects, well under).
        let global = GlobalLedger::new(64 << 30, 1 << 24);
        assert_eq!(global.residency_bytes(), 0);
        assert_eq!(global.object_count(), 0);

        thread::scope(|scope| {
            for t in 0..THREADS {
                let global = &global;
                scope.spawn(move || {
                    // A byte pattern unique to this thread; another session's bytes would never match it.
                    let pattern = (t as u8).wrapping_mul(7).wrapping_add(1);
                    let mut s = sink_on(global);

                    for round in 0..ROUNDS {
                        // Create IDS buffers under ids 1..=IDS (the SAME ids every other thread uses).
                        let mut create = Vec::new();
                        for id in 1..=IDS {
                            create.push(buffer(id, SIZE as u64));
                        }
                        s.submit(&create).expect("concurrent create");

                        // Write this thread's unique pattern into each, then read it straight back.
                        for id in 1..=IDS {
                            s.submit(&[write(id, pattern, SIZE)]).expect("concurrent write");
                        }
                        for id in 1..=IDS {
                            let got = s.read_buffer(BufferId(id), 0, SIZE).expect("concurrent readback");
                            assert_eq!(
                                got,
                                vec![pattern; SIZE],
                                "thread {t} round {round} id {id}: cross-session bleed — saw another session's bytes",
                            );
                        }

                        assert_eq!(s.session().resources.live_count() as u32, IDS);

                        // Destroy them all so the next round re-creates the same ids from clean.
                        let destroy: Vec<Cmd> = (1..=IDS).map(Cmd::DestroyBuffer).collect();
                        s.submit(&destroy).expect("concurrent destroy");
                        assert_eq!(s.session().resources.live_count(), 0);
                        assert_eq!(s.session().residency_bytes(), 0, "thread {t}: own ledger back to zero each round");
                    }
                    // `s` drops here → Drop refunds this connection's (zero) contribution.
                });
            }
        });

        // Every session has dropped. The shared account is EXACTLY at baseline — no residency or object
        // count stranded by any concurrent teardown.
        assert_eq!(
            global.residency_bytes(),
            0,
            "shared residency did not return to baseline after concurrent sessions"
        );
        assert_eq!(
            global.object_count(),
            0,
            "shared object count did not return to baseline after concurrent sessions"
        );

        // ...and the shared account is immediately reusable by a fresh session that computes correctly.
        let mut fresh = sink_on(&global);
        fresh.submit(&[buffer(1, 32), write(1, 0x5A, 32)]).unwrap();
        assert_eq!(
            fresh.read_buffer(BufferId(1), 0, 32).unwrap(),
            vec![0x5A; 32]
        );
    });
}

// =================================================================================================
// 2. large_resource_tables — thousands of live resources, correct + non-degenerate lookup
// =================================================================================================
