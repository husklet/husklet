//! Runtime CONCURRENT + LARGE-SCALE battery — the scaling/isolation/leak layer the single-threaded
//! `session_lifecycle.rs`, `account_atomicity.rs`, and the single-connection `transport_socket.rs` do
//! not cover. Where those drive one session (or two, interleaved on one thread), this drives DOZENS of
//! independent [`Session`]s from real OS threads against ONE shared [`GlobalLedger`], plus a single
//! session grown to THOUSANDS of live resources.
//!
//! The one piece of state shared across sessions is the process-global residency account
//! ([`GlobalLedger`]: an `Arc<Mutex<Totals>>`). A [`Session`] / [`SessionResources`] is `!Send` (it owns
//! `Box<dyn Any>` executor natives), so a thread NEVER receives a session — it clones the `Send + Sync`
//! `GlobalLedger` and stands up its OWN session locally. That models the real host: N per-connection
//! sessions, each on its own worker, all metering the same shared budget.
//!
//! Every test:
//!   * asserts CORRECTNESS (each session computes/reads back its own bytes — zero cross-session bleed),
//!   * asserts a SCALING / ISOLATION / LEAK property (shared residency returns EXACTLY to baseline; the
//!     final account is exactly the sum of live contributions with no lost/double charge under
//!     contention; the resource table does not degrade superlinearly), and
//!   * runs under a HARD TIMEOUT (a deadlock/hang fails the test instead of blocking the suite forever).
//!
//! Deterministic: fixed byte patterns, a `FakeClock`, the pure-CPU reference executor, a real `vecadd`
//! kernel with known inputs. The only nondeterminism is thread interleaving — which is exactly the axis
//! under test — and the invariants asserted hold for EVERY interleaving.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ComputePipelineDesc, ShaderRef,
};
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::protocol::model::kernel::{
    gty, Inst, KernelProgram, Op, Param, CMP_GE, KERNEL_MAGIC, SR_CTAID_X, SR_NTID_X, SR_TID_X,
};
use hl_gpu::{
    BufferId, Cmd, CommandBuffer, CommandSink, CpuExecutor, Enc, FakeClock, GlobalLedger, GpuError,
    GpuExecutor, InProcessCommandSink, Limits, Session, ShaderPayloadKind,
};

// =================================================================================================
// harness
// =================================================================================================

/// Run `f` on a worker thread and FAIL the test if it does not finish within `secs`. A deadlock, a lost
/// wakeup, or a livelock in the runtime under concurrency then surfaces as a test failure (not a hung
/// suite). A panic (assertion failure) inside `f` — including one propagated out of a `thread::scope` —
/// is re-raised here so it fails the test with its original message.
fn with_timeout<F: FnOnce() + Send + 'static>(secs: u64, f: F) {
    let handle = thread::spawn(f);
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !handle.is_finished() {
        if Instant::now() > deadline {
            // Leave the worker parked (the process tears down after the test binary exits); the point is
            // to convert a hang into a deterministic failure rather than block the whole suite.
            panic!("runtime_scale: work did not complete within {secs}s — deadlock/hang under concurrency");
        }
        thread::sleep(Duration::from_millis(20));
    }
    // Propagate any assertion failure / panic raised inside the work (scoped-thread panics re-raise here).
    handle.join().expect("runtime_scale: worker thread panicked");
}

/// Build a fresh in-process sink whose session shares `global` (the surface a concurrency/leak check
/// meters against). Built INSIDE the worker thread — a `Session` is `!Send` and never crosses a thread.
fn sink_on(global: &GlobalLedger) -> InProcessCommandSink<CpuExecutor> {
    let limits = Limits::from_capabilities(CpuExecutor::new().capabilities());
    let session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));
    InProcessCommandSink::with_session(session, CpuExecutor::new())
}

/// A sink like [`sink_on`] but with an explicit per-connection object ceiling (the large-table test needs
/// headroom above the 65_536 default; the churn tests keep the default).
fn sink_on_with_objects(global: &GlobalLedger, max_objects: u64) -> InProcessCommandSink<CpuExecutor> {
    let mut limits = Limits::from_capabilities(CpuExecutor::new().capabilities());
    limits.max_connection_objects = max_objects;
    let session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));
    InProcessCommandSink::with_session(session, CpuExecutor::new())
}

fn buffer(id: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc { size, usage: buffer_usage::COPY_DST | buffer_usage::STORAGE, label: String::new() },
    )
}

fn write(id: u32, byte: u8, len: usize) -> Cmd {
    Cmd::WriteBuffer { id, offset: 0, data: vec![byte; len] }
}

// =================================================================================================
// 1. many_concurrent_sessions — dozens of sessions, overlapping ids, zero bleed, no leak
// =================================================================================================

/// Dozens of independent sessions, each on its own thread, all sharing ONE `GlobalLedger`, each
/// repeatedly creating/writing/reading/destroying resources under the SAME (overlapping) id space.
///
/// Correctness: every session reads back its OWN unique byte pattern — a cross-session id collision or a
/// shared-table bleed would make one thread see another's bytes. Leak/isolation: after every session
/// drops, the shared account returns EXACTLY to baseline (0 bytes, 0 objects) — no residency stranded by
/// a concurrent teardown.
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
        assert_eq!(global.residency_bytes(), 0, "shared residency did not return to baseline after concurrent sessions");
        assert_eq!(global.object_count(), 0, "shared object count did not return to baseline after concurrent sessions");

        // ...and the shared account is immediately reusable by a fresh session that computes correctly.
        let mut fresh = sink_on(&global);
        fresh.submit(&[buffer(1, 32), write(1, 0x5A, 32)]).unwrap();
        assert_eq!(fresh.read_buffer(BufferId(1), 0, 32).unwrap(), vec![0x5A; 32]);
    });
}

// =================================================================================================
// 2. large_resource_tables — thousands of live resources, correct + non-degenerate lookup
// =================================================================================================

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
        s.submit(&[write(EARLY_ID, PATTERN, SIZE as usize)]).unwrap();

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
        assert_eq!(s.session().resources.buffers.len() as u32, N + 1, "every create is live in the table");
        assert_eq!(s.session().object_count(), (N + 1) as u64);

        // The EARLY buffer still reads back its original pattern after N later creates (no clobber/alias).
        let got = s.read_buffer(BufferId(EARLY_ID), 0, SIZE as usize).unwrap();
        assert_eq!(got, vec![PATTERN; SIZE as usize], "early buffer survived thousands of later creates intact");

        // A lookup on the FULL table is still O(1): LOOKUPS reads of the early buffer with N+1 entries.
        let t1 = Instant::now();
        for _ in 0..LOOKUPS {
            let got = s.read_buffer(BufferId(EARLY_ID), 0, SIZE as usize).unwrap();
            assert_eq!(got[0], PATTERN);
        }
        let full_table_lookup = t1.elapsed().max(Duration::from_nanos(1));

        // Spot-check lookups scattered across the whole id range are all live and correct.
        for id in [2u32, half / 2, half, half + 2, N, N + 1] {
            assert!(s.session().resources.buffers.contains(id), "id {id} must be live");
        }
        // A never-created id is cleanly UnknownId even with the table full.
        assert_eq!(
            s.read_buffer(BufferId(N + 100), 0, 4).unwrap_err(),
            GpuError::UnknownId { kind: "buffer", id: N + 100 },
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
        assert_eq!(s.session().resources.buffers.len(), 0, "every id destroyed cleanly");
        assert_eq!(s.session().residency_bytes(), 0, "residency fully refunded at scale");
        assert_eq!(s.session().object_count(), 0);
        drop(s);
        assert_eq!(global.residency_bytes(), 0, "shared account back to baseline after the large session");
    });
}

/// Create `count` buffers with ids `start..start+count`, each `size` bytes, in submits of `batch` creates.
fn create_range(s: &mut InProcessCommandSink<CpuExecutor>, start: u32, count: u32, size: u64, batch: u32) {
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
                    assert_eq!(s.session().residency_bytes(), (M as u64) * SIZE, "thread {t}: own charge exact");
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
                        s.submit(&[Cmd::DestroyBuffer(1), buffer(1, newsize)]).expect("recycle id 1");
                        // Illegal duplicate create over a still-live id → DuplicateId, rolled back, no drift.
                        let err = s.submit(&[buffer(2, 999)]).unwrap_err();
                        assert!(matches!(err, GpuError::DuplicateId { kind: "buffer", id: 2 }), "thread {t}: expected DuplicateId");
                        // Restore id 1 to SIZE so the running total is exactly the baseline again.
                        s.submit(&[Cmd::DestroyBuffer(1), buffer(1, SIZE)]).expect("restore id 1");
                        assert_eq!(s.session().residency_bytes(), baseline_bytes, "thread {t} cycle {c}: bytes drifted");
                        assert_eq!(s.session().object_count(), baseline_objs, "thread {t} cycle {c}: objects drifted");
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
        assert_eq!(global.residency_bytes(), 0, "shared residency drifted from baseline after concurrent churn");
        assert_eq!(global.object_count(), 0, "shared object count drifted from baseline after concurrent churn");
    });
}

// =================================================================================================
// 4. session_churn_no_leak — rapid create+drop of many sessions returns to baseline every cycle
// =================================================================================================

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
        persistent.submit(&[buffer(1, 4096), write(1, 0x77, 4096)]).unwrap();
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
                assert_eq!(persistent.read_buffer(BufferId(1), 0, 4096).unwrap(), vec![0x77; 4096]);
                // drop `s` here.
            }
            // Back to exactly the persistent floor — the ephemeral session leaked nothing.
            assert_eq!(global.residency_bytes(), persistent_bytes, "cycle {c}: residency did not return to the floor");
            assert_eq!(global.object_count(), persistent_objs, "cycle {c}: object count did not return to the floor");
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
                        let create: Vec<Cmd> = (1..=PER_SESSION).map(|id| buffer(id, SIZE)).collect();
                        s.submit(&create).unwrap();
                        assert_eq!(s.session().object_count(), PER_SESSION as u64);
                        // drop at end of iteration.
                    }
                });
            }
        });

        // After all the concurrent churn, the ONLY thing left on the account is the persistent session.
        assert_eq!(global.residency_bytes(), persistent_bytes, "concurrent churn leaked into the shared account");
        assert_eq!(global.object_count(), persistent_objs);
        // The persistent session is still intact and correct.
        assert_eq!(persistent.read_buffer(BufferId(1), 0, 4096).unwrap(), vec![0x77; 4096]);
        drop(persistent);
        assert_eq!(global.residency_bytes(), 0, "final teardown returns the account to baseline");
        assert_eq!(global.object_count(), 0);
    });
}

// =================================================================================================
// 5. mixed_load — real compute concurrent with create/destroy churn, all results correct
// =================================================================================================

/// `c[i] = a[i] + b[i]` with `i = blockIdx*blockDim + tid` and an `if (i >= n) return;` guard — the real
/// vecadd kernel IR used by `perf.rs` / `conformance.rs` (the PTX front-end is a driver concern).
fn vecadd_program() -> KernelProgram {
    KernelProgram {
        entry: "vecadd".into(),
        block: [4, 1, 1],
        params: vec![
            Param { width: 8, offset: 0, is_ptr: true, region: 0 },
            Param { width: 8, offset: 8, is_ptr: true, region: 1 },
            Param { width: 8, offset: 16, is_ptr: true, region: 2 },
            Param { width: 4, offset: 24, is_ptr: false, region: 0 },
        ],
        param_bytes: 28,
        num_regions: 3,
        shared_bytes: 0,
        reg_count: 19,
        insts: vec![
            Inst::LdParam { d: 0, param: 0 },
            Inst::LdParam { d: 1, param: 1 },
            Inst::LdParam { d: 2, param: 2 },
            Inst::LdParam { d: 3, param: 3 },
            Inst::MovSReg { d: 4, sreg: SR_NTID_X },
            Inst::MovSReg { d: 5, sreg: SR_CTAID_X },
            Inst::MovSReg { d: 6, sreg: SR_TID_X },
            Inst::IMad { d: 7, a: Op::Reg(5), b: Op::Reg(4), c: Op::Reg(6) },
            Inst::Setp { d: 8, a: Op::Reg(7), b: Op::Reg(3), cmp: CMP_GE, unsigned: false },
            Inst::Bra { target: 21, pred: Some((8, false)) },
            Inst::Cvta { d: 9, s: 0 },
            Inst::IMul { d: 10, a: Op::Reg(7), b: Op::ImmI(4), wide: true, unsigned: false },
            Inst::IAdd { d: 11, a: Op::Reg(9), b: Op::Reg(10), wide: true },
            Inst::Cvta { d: 12, s: 1 },
            Inst::IAdd { d: 13, a: Op::Reg(12), b: Op::Reg(10), wide: true },
            Inst::LdGlobal { d: 14, addr: 13, off: 0, ty: gty::F32 },
            Inst::LdGlobal { d: 15, addr: 11, off: 0, ty: gty::F32 },
            Inst::FAdd { d: 16, a: Op::Reg(15), b: Op::Reg(14) },
            Inst::Cvta { d: 17, s: 2 },
            Inst::IAdd { d: 18, a: Op::Reg(17), b: Op::Reg(10), wide: true },
            Inst::StGlobal { addr: 18, off: 0, src: Op::Reg(16), ty: gty::F32 },
            Inst::Ret,
        ],
    }
}

/// Concurrent COMPUTE sessions (each running a real vecadd over known inputs, many times) racing against
/// concurrent CHURN sessions (creating/destroying resources) on the same shared account. Every compute
/// session must read back the exact `a[i]+b[i]` result on every round — a session-crossing corruption or
/// a shared-account race would perturb a result — and after everything drops the account is baseline.
#[test]
fn mixed_load() {
    with_timeout(180, || {
        const COMPUTE_THREADS: u32 = 8;
        const CHURN_THREADS: u32 = 8;
        const ROUNDS: usize = 12;
        const N: u32 = 256; // vecadd elements per dispatch
        const CHURN_BUFS: u32 = 32;
        const CHURN_SIZE: u64 = 4096;

        let global = GlobalLedger::unbounded();
        // Count completed compute rounds across all threads, to assert the load actually ran.
        let completed = Arc::new(AtomicU64::new(0));

        thread::scope(|scope| {
            // --- compute workers: real vecadd, unique per-thread inputs, verified readback every round ---
            for t in 0..COMPUTE_THREADS {
                let global = &global;
                let completed = Arc::clone(&completed);
                scope.spawn(move || {
                    let buf_bytes = (N as u64) * 4;
                    let mut exec = CpuExecutor::new();
                    exec.define_kernel(1, vecadd_program());
                    let limits = Limits::from_capabilities(exec.capabilities());
                    let session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));
                    let mut s = InProcessCommandSink::with_session(session, exec);

                    // Per-thread inputs: a[i] = i + t*1000, b[i] = 2*i + t, so every thread's result differs
                    // and a cross-session bleed would produce the wrong sum.
                    let a: Vec<u8> = (0..N)
                        .flat_map(|i| ((i as f32) + (t as f32) * 1000.0).to_le_bytes())
                        .collect();
                    let b: Vec<u8> = (0..N)
                        .flat_map(|i| ((2 * i) as f32 + t as f32).to_le_bytes())
                        .collect();
                    let mut param = vec![0u8; 28];
                    param[24..28].copy_from_slice(&N.to_le_bytes());

                    // Setup: shader + pipeline + 4 buffers + input uploads + bind group.
                    s.submit(&[
                        Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::PtxKernel, spirv: vec![KERNEL_MAGIC, 0] },
                        Cmd::CreateComputePipeline(
                            1,
                            ComputePipelineDesc { compute: ShaderRef { module: 1, entry: "vecadd".into() }, label: String::new() },
                        ),
                        Cmd::CreateBuffer(1, BufferDesc { size: 28, usage: buffer_usage::STORAGE, label: String::new() }),
                        Cmd::CreateBuffer(2, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE, label: String::new() }),
                        Cmd::CreateBuffer(3, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE, label: String::new() }),
                        Cmd::CreateBuffer(4, BufferDesc { size: buf_bytes, usage: buffer_usage::STORAGE | buffer_usage::COPY_SRC, label: String::new() }),
                        Cmd::WriteBuffer { id: 1, offset: 0, data: param },
                        Cmd::WriteBuffer { id: 2, offset: 0, data: a.clone() },
                        Cmd::WriteBuffer { id: 3, offset: 0, data: b.clone() },
                        Cmd::CreateBindGroup(
                            1,
                            BindGroupDesc {
                                set: 0,
                                entries: vec![
                                    BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 28 } },
                                    BindEntry { binding: 1, resource: BindResource::Buffer { id: 2, offset: 0, size: buf_bytes } },
                                    BindEntry { binding: 2, resource: BindResource::Buffer { id: 3, offset: 0, size: buf_bytes } },
                                    BindEntry { binding: 3, resource: BindResource::Buffer { id: 4, offset: 0, size: buf_bytes } },
                                ],
                            },
                        ),
                    ])
                    .expect("compute setup");

                    let groups = N / 4;
                    let dispatch = Cmd::Submit(CommandBuffer {
                        encoder: vec![
                            Enc::BeginComputePass,
                            Enc::SetPipeline(1),
                            Enc::SetBindGroup { index: 0, group: 1 },
                            Enc::Dispatch { x: groups, y: 1, z: 1 },
                            Enc::EndComputePass,
                        ],
                        signal: None,
                    });

                    for round in 0..ROUNDS {
                        s.submit(std::slice::from_ref(&dispatch)).expect("compute dispatch");
                        // Read the whole result buffer and verify c[i] == a[i] + b[i] for every element.
                        let out = s.read_buffer(BufferId(4), 0, buf_bytes as usize).expect("compute readback");
                        for i in 0..N as usize {
                            let bytes: [u8; 4] = out[i * 4..i * 4 + 4].try_into().unwrap();
                            let got = f32::from_le_bytes(bytes);
                            let expect = ((i as f32) + (t as f32) * 1000.0) + ((2 * i) as f32 + t as f32);
                            assert_eq!(
                                got, expect,
                                "compute thread {t} round {round} elem {i}: wrong result under mixed load (corruption/bleed)",
                            );
                        }
                        completed.fetch_add(1, Ordering::Relaxed);
                    }
                    // `s` drops → refunds this compute session's contribution.
                });
            }

            // --- churn workers: create/destroy pressure on the same shared account, concurrently ---
            for _ in 0..CHURN_THREADS {
                let global = &global;
                scope.spawn(move || {
                    let mut s = sink_on(global);
                    for _ in 0..ROUNDS * 4 {
                        let create: Vec<Cmd> = (1..=CHURN_BUFS).map(|id| buffer(id, CHURN_SIZE)).collect();
                        s.submit(&create).expect("churn create");
                        // Write + read one to keep the executor genuinely busy.
                        s.submit(&[write(1, 0xE7, CHURN_SIZE as usize)]).expect("churn write");
                        assert_eq!(s.read_buffer(BufferId(1), 0, 8).unwrap(), vec![0xE7; 8]);
                        let destroy: Vec<Cmd> = (1..=CHURN_BUFS).map(Cmd::DestroyBuffer).collect();
                        s.submit(&destroy).expect("churn destroy");
                        assert_eq!(s.session().residency_bytes(), 0);
                    }
                });
            }
        });

        // The compute load actually ran to completion on every thread and every round.
        assert_eq!(
            completed.load(Ordering::Relaxed),
            (COMPUTE_THREADS as u64) * (ROUNDS as u64),
            "not every compute round completed",
        );
        // All sessions dropped → shared account exactly at baseline (no corruption stranded residency).
        assert_eq!(global.residency_bytes(), 0, "mixed load leaked shared residency");
        assert_eq!(global.object_count(), 0, "mixed load leaked shared object count");
    });
}
