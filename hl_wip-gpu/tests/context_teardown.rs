//! Connection residency across a NACK + teardown — the host-ledger half of the Chrome lost-context fix.
//!
//! A residency-over-budget frame must roll back ATOMICALLY (leaving the ledger exactly where it was), and a
//! subsequent teardown (destroying the connection's working set) must refund the whole ledger back to zero.
//! Together these guarantee the host per-connection residency ledger cannot CLIMB across a NACK/retry loop —
//! the death spiral the guest-side context-teardown retirement (`hl_gl::GlContext::retire_all`) feeds.

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{
    Cmd, CommandSink, CpuExecutor, FakeClock, GlobalLedger, GpuError, GpuExecutor,
    InProcessCommandSink, Limits, Session,
};

fn buffer(id: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(id, BufferDesc { size, usage: buffer_usage::COPY_DST, label: String::new() })
}

/// An in-process accounting sink whose per-connection residency ceiling is `max_bytes` (tight, so a working
/// set that overruns it NACKs) over an unbounded global account.
fn sink_with_cap(max_bytes: u64) -> InProcessCommandSink<CpuExecutor> {
    let exec = CpuExecutor::new();
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.max_connection_bytes = max_bytes;
    let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
    InProcessCommandSink::with_session(session, exec)
}

#[test]
fn residency_nack_then_teardown_does_not_climb() {
    let mut sink = sink_with_cap(4096);
    assert_eq!(sink.session().residency_bytes(), 0);

    // A working set that fits: two 1 KiB buffers → 2048 B resident.
    sink.submit(&[buffer(1, 1024), buffer(2, 1024)]).expect("working set fits under the cap");
    let resident = sink.session().residency_bytes();
    assert_eq!(resident, 2048);
    assert_eq!(sink.session().object_count(), 2);

    // Now hammer the cap the way a lost-context retry loop does: an over-budget frame that NACKs. It MUST
    // roll back atomically — the ledger stays exactly where it was, never creeping up per attempt.
    for attempt in 0..5 {
        let err = sink
            .submit(&[buffer(10, 1024), buffer(11, 1024), buffer(12, 1024)])
            .expect_err("over-budget frame must NACK");
        assert_eq!(err, GpuError::ResourceLimit("connection residency"), "attempt {attempt}");
        assert_eq!(
            sink.session().residency_bytes(),
            resident,
            "attempt {attempt}: a NACKed frame leaves the ledger UNCHANGED (no climb)"
        );
        assert_eq!(sink.session().object_count(), 2, "attempt {attempt}: object count unchanged");
    }

    // Teardown: destroy the working set. The ledger refunds back to zero — the abandoned set holds no
    // residency, so a fresh working set has the whole cap available again (spiral broken).
    sink.submit(&[Cmd::DestroyBuffer(1), Cmd::DestroyBuffer(2)]).expect("teardown destroys succeed");
    assert_eq!(sink.session().residency_bytes(), 0, "teardown refunds every resident byte");
    assert_eq!(sink.session().object_count(), 0, "teardown refunds every resident object");

    // And a full fresh working set now fits again — proof the ledger truly reset, not merely paused.
    sink.submit(&[buffer(20, 1024), buffer(21, 1024)]).expect("a fresh working set fits post-teardown");
    assert_eq!(sink.session().residency_bytes(), 2048);
}

#[test]
fn recreate_teardown_cycles_stay_bounded() {
    // Each cycle creates a working set with FRESH ids (a recreated context) and tears the prior one down.
    // Without teardown refunds the ledger would climb one working set per cycle; with them it is bounded.
    let mut sink = sink_with_cap(1 << 20);
    let mut high_water = 0u64;
    for cycle in 0u32..16 {
        let base = cycle * 10 + 1;
        sink.submit(&[buffer(base, 4096), buffer(base + 1, 4096)]).expect("cycle working set fits");
        high_water = high_water.max(sink.session().residency_bytes());
        sink.submit(&[Cmd::DestroyBuffer(base), Cmd::DestroyBuffer(base + 1)]).expect("teardown");
        assert_eq!(sink.session().residency_bytes(), 0, "cycle {cycle}: ledger back to zero");
    }
    assert_eq!(high_water, 8192, "the ledger never held more than a SINGLE cycle's working set");
}
