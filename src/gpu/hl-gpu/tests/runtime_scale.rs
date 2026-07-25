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
    handle
        .join()
        .expect("runtime_scale: worker thread panicked");
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
fn sink_on_with_objects(
    global: &GlobalLedger,
    max_objects: u64,
) -> InProcessCommandSink<CpuExecutor> {
    let mut limits = Limits::from_capabilities(CpuExecutor::new().capabilities());
    limits.max_connection_objects = max_objects;
    let session = Session::new(limits, global.clone(), Box::new(FakeClock::new(0)));
    InProcessCommandSink::with_session(session, CpuExecutor::new())
}

fn buffer(id: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc {
            size,
            usage: buffer_usage::COPY_DST | buffer_usage::STORAGE,
            label: String::new(),
        },
    )
}

fn write(id: u32, byte: u8, len: usize) -> Cmd {
    Cmd::WriteBuffer {
        id,
        offset: 0,
        data: vec![byte; len],
    }
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
/// `c[i] = a[i] + b[i]` with `i = blockIdx*blockDim + tid` and an `if (i >= n) return;` guard — the real
/// vecadd kernel IR used by `perf.rs` / `conformance.rs` (the PTX front-end is a driver concern).
fn vecadd_program() -> KernelProgram {
    KernelProgram {
        entry: "vecadd".into(),
        block: [4, 1, 1],
        params: vec![
            Param {
                width: 8,
                offset: 0,
                is_ptr: true,
                region: 0,
            },
            Param {
                width: 8,
                offset: 8,
                is_ptr: true,
                region: 1,
            },
            Param {
                width: 8,
                offset: 16,
                is_ptr: true,
                region: 2,
            },
            Param {
                width: 4,
                offset: 24,
                is_ptr: false,
                region: 0,
            },
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
            Inst::MovSReg {
                d: 4,
                sreg: SR_NTID_X,
            },
            Inst::MovSReg {
                d: 5,
                sreg: SR_CTAID_X,
            },
            Inst::MovSReg {
                d: 6,
                sreg: SR_TID_X,
            },
            Inst::IMad {
                d: 7,
                a: Op::Reg(5),
                b: Op::Reg(4),
                c: Op::Reg(6),
            },
            Inst::Setp {
                d: 8,
                a: Op::Reg(7),
                b: Op::Reg(3),
                cmp: CMP_GE,
                unsigned: false,
            },
            Inst::Bra {
                target: 21,
                pred: Some((8, false)),
            },
            Inst::Cvta { d: 9, s: 0 },
            Inst::IMul {
                d: 10,
                a: Op::Reg(7),
                b: Op::ImmI(4),
                wide: true,
                unsigned: false,
            },
            Inst::IAdd {
                d: 11,
                a: Op::Reg(9),
                b: Op::Reg(10),
                wide: true,
            },
            Inst::Cvta { d: 12, s: 1 },
            Inst::IAdd {
                d: 13,
                a: Op::Reg(12),
                b: Op::Reg(10),
                wide: true,
            },
            Inst::LdGlobal {
                d: 14,
                addr: 13,
                off: 0,
                ty: gty::F32,
            },
            Inst::LdGlobal {
                d: 15,
                addr: 11,
                off: 0,
                ty: gty::F32,
            },
            Inst::FAdd {
                d: 16,
                a: Op::Reg(15),
                b: Op::Reg(14),
            },
            Inst::Cvta { d: 17, s: 2 },
            Inst::IAdd {
                d: 18,
                a: Op::Reg(17),
                b: Op::Reg(10),
                wide: true,
            },
            Inst::StGlobal {
                addr: 18,
                off: 0,
                src: Op::Reg(16),
                ty: gty::F32,
            },
            Inst::Ret,
        ],
    }
}

#[path = "runtime_scale/churn.rs"]
mod churn;
#[path = "runtime_scale/ledger.rs"]
mod ledger;
#[path = "runtime_scale/mixed.rs"]
mod mixed;
#[path = "runtime_scale/resources.rs"]
mod resources;
#[path = "runtime_scale/sessions.rs"]
mod sessions;
