use super::*;

/// A single session grown to THOUSANDS of live buffers. Correctness: create/lookup/destroy stay exact at
/// scale (live count is exact; an early-created buffer still reads back its pattern after thousands of
/// later creates; every id destroys cleanly). Scaling: a per-id lookup on the FULL table is within a
/// bounded factor of the same lookup on a tiny table (the table is O(1), not O(n)); and creating the
/// second half of the table takes no more than a bounded factor of the first half (no per-op blow-up).
#[test]
fn large_resource_tables() {
    with_timeout(180, || {
        const N: u32 = 32_768; // "thousands" of live resources on one session
        const BATCH: u32 = 512; // realistic batched submits (a driver never submits N creates as one frame)
        const SIZE: u64 = 64;
        const EARLY_ID: u32 = 1;
        const PATTERN: u8 = 0xC3;
        const LOOKUPS: u32 = 4_000;

        // Object ceiling above N; unbounded shared account (we care about the table, not the budget).
        let global = GlobalLedger::unbounded();
        let mut s = sink_on_with_objects(&global, (N as u64) * 2);

        // The early buffer: created first, written a known pattern. It must survive every later create.
        s.submit(&[buffer(EARLY_ID, SIZE)]).unwrap();
        s.submit(&[write(EARLY_ID, PATTERN, SIZE as usize)])
            .unwrap();

        // Baseline: LOOKUPS reads of the early buffer while the table holds a single entry.
        let t0 = Instant::now();
        for _ in 0..LOOKUPS {
            let got = s.read_buffer(BufferId(EARLY_ID), 0, SIZE as usize).unwrap();
            assert_eq!(got[0], PATTERN);
        }
        let small_table_lookup = t0.elapsed().max(Duration::from_nanos(1));

        // Create the FIRST half of the table (ids 2..=N/2+1), batched; time it.
        let half = N / 2;
        let t_first = Instant::now();
        create_range(&mut s, 2, half, SIZE, BATCH);
        let first_half_create = t_first.elapsed().max(Duration::from_nanos(1));

        // Create the SECOND half (ids half+2..=N+1), batched; time it. Superlinear per-op behaviour would
        // make this dramatically slower than the first half.
        let t_second = Instant::now();
        create_range(&mut s, half + 2, half, SIZE, BATCH);
        let second_half_create = t_second.elapsed().max(Duration::from_nanos(1));

        // Exact live count: the early buffer + N created.
        assert_eq!(
            s.session().resources.buffers.len() as u32,
            N + 1,
            "every create is live in the table"
        );
        assert_eq!(s.session().object_count(), (N + 1) as u64);

        // The EARLY buffer still reads back its original pattern after N later creates (no clobber/alias).
        let got = s.read_buffer(BufferId(EARLY_ID), 0, SIZE as usize).unwrap();
        assert_eq!(
            got,
            vec![PATTERN; SIZE as usize],
            "early buffer survived thousands of later creates intact"
        );

        // A lookup on the FULL table is still O(1): LOOKUPS reads of the early buffer with N+1 entries.
        let t1 = Instant::now();
        for _ in 0..LOOKUPS {
            let got = s.read_buffer(BufferId(EARLY_ID), 0, SIZE as usize).unwrap();
            assert_eq!(got[0], PATTERN);
        }
        let full_table_lookup = t1.elapsed().max(Duration::from_nanos(1));

        // Spot-check lookups scattered across the whole id range are all live and correct.
        for id in [2u32, half / 2, half, half + 2, N, N + 1] {
            assert!(
                s.session().resources.buffers.contains(id),
                "id {id} must be live"
            );
        }
        // A never-created id is cleanly UnknownId even with the table full.
        assert_eq!(
            s.read_buffer(BufferId(N + 100), 0, 4).unwrap_err(),
            GpuError::UnknownId {
                kind: "buffer",
                id: N + 100
            },
        );

        // SCALING GUARDS (loose, like perf.rs — a real regression fails, ordinary variance passes):
        // O(1) table lookup: a full-table lookup batch is within 8x of a single-entry lookup batch.
        let lookup_ratio = full_table_lookup.as_secs_f64() / small_table_lookup.as_secs_f64();
        println!(
            "large_resource_tables: lookup small={:?} full={:?} ratio={:.2} (N={N})",
            small_table_lookup, full_table_lookup, lookup_ratio,
        );
        assert!(
            lookup_ratio < 8.0,
            "table lookup degraded with size (ratio {lookup_ratio:.2}) — not O(1)",
        );
        // Bounded create: second half is within 12x of the first half (batched creates stay near-linear;
        // the per-frame copy-on-write ledger grows only with batch size, not table size).
        let create_ratio = second_half_create.as_secs_f64() / first_half_create.as_secs_f64();
        println!(
            "large_resource_tables: create first={:?} second={:?} ratio={:.2}",
            first_half_create, second_half_create, create_ratio,
        );
        assert!(
            create_ratio < 12.0,
            "creating the second half degraded superlinearly vs the first (ratio {create_ratio:.2})",
        );

        // Destroy the WHOLE table, batched, and confirm it drains to exactly empty (no stranded entry).
        destroy_range(&mut s, EARLY_ID, 1, BATCH); // the early buffer
        destroy_range(&mut s, 2, N, BATCH); // the N created buffers
        assert_eq!(
            s.session().resources.buffers.len(),
            0,
            "every id destroyed cleanly"
        );
        assert_eq!(
            s.session().residency_bytes(),
            0,
            "residency fully refunded at scale"
        );
        assert_eq!(s.session().object_count(), 0);
        drop(s);
        assert_eq!(
            global.residency_bytes(),
            0,
            "shared account back to baseline after the large session"
        );
    });
}

/// Create `count` buffers with ids `start..start+count`, each `size` bytes, in submits of `batch` creates.
fn create_range(
    s: &mut InProcessCommandSink<CpuExecutor>,
    start: u32,
    count: u32,
    size: u64,
    batch: u32,
) {
    let mut id = start;
    let end = start + count;
    while id < end {
        let hi = (id + batch).min(end);
        let cmds: Vec<Cmd> = (id..hi).map(|i| buffer(i, size)).collect();
        s.submit(&cmds).expect("batched create");
        id = hi;
    }
}

/// Destroy `count` buffers with ids `start..start+count`, in submits of `batch` destroys.
fn destroy_range(s: &mut InProcessCommandSink<CpuExecutor>, start: u32, count: u32, batch: u32) {
    let mut id = start;
    let end = start + count;
    while id < end {
        let hi = (id + batch).min(end);
        let cmds: Vec<Cmd> = (id..hi).map(Cmd::DestroyBuffer).collect();
        s.submit(&cmds).expect("batched destroy");
        id = hi;
    }
}

// =================================================================================================
// 3. account_ledger_under_concurrency — exact global total under contention, no lost/double charge
// =================================================================================================
