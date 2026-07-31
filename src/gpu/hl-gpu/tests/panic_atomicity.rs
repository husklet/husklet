//! What a PANICKING executor costs the session — the measurement that separates the two lethalities the
//! same shader panic produced in the field.
//!
//! A guest shader the wgpu backend could not accept used to panic inside `create_shader_module` instead of
//! returning an error (fixed by routing every guest-derived module through a validation error scope). The
//! transport's `HandlerBoundary` catches such a panic and NACKs the frame, so the host PROCESS survives —
//! and yet the same panic was observed twice with different outcomes: on one bundle it cost only a thread
//! and a dEQP sweep continued past 15,000 cases, and on another it took the whole execution domain down at
//! case 13,977. A nondeterministic killer is far harder to diagnose than a reliable one, so the mechanism
//! is worth pinning rather than guessing.
//!
//! The candidate mechanism is `runtime::submit`'s atomicity contract. On a clean `Err` it rolls the id
//! tables and the residency ledger back to their pre-frame state, so a refused batch leaves no trace. A
//! PANIC unwinds straight past that rollback: whatever the executor already applied before it panicked
//! stays applied, with no record that a frame was in flight. So the cost of a panic should depend entirely
//! on WHAT THE ABORTED BATCH HAD ALREADY DONE when it died — which is not a property of the panic at all,
//! and would look like nondeterminism from the outside.
//!
//! The two arms below hold the panic fixed and vary only that. `panic_on` marks the command the executor
//! dies on; every command before it is really applied to the resource tables first, exactly as a mid-batch
//! backend failure does.

use std::panic::{catch_unwind, AssertUnwindSafe};

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::runtime::port::executor::Presentation;
use hl_gpu::{
    Capabilities, Cmd, CpuExecutor, FakeClock, FenceId, GlobalLedger, GpuExecutor, Limits, Result,
    Session,
};

/// The id whose creation the executor panics on — a stand-in for `create_shader_module`'s panic, reached
/// through an ordinary command so the batch shape is the only variable.
const PANIC_ID: u32 = 777;

/// The reference CPU executor, except that it panics partway through a batch. Commands BEFORE the marker
/// are applied for real, so the session is left in whatever state a mid-batch abort would leave it.
struct PanicMidBatch(CpuExecutor);

impl GpuExecutor for PanicMidBatch {
    fn capabilities(&self) -> Capabilities {
        self.0.capabilities()
    }

    fn execute(
        &mut self,
        resources: &mut SessionResources,
        batch: &[Cmd],
    ) -> Result<Vec<Presentation>> {
        let mut presentations = Vec::new();
        for command in batch {
            if matches!(command, Cmd::CreateBuffer(PANIC_ID, _)) {
                panic!("executor panicked mid-batch (stand-in for a backend panic)");
            }
            presentations.extend(self.0.execute(resources, std::slice::from_ref(command))?);
        }
        Ok(presentations)
    }

    fn wait(&mut self, resources: &mut SessionResources, fence: FenceId, value: u64) -> Result<()> {
        self.0.wait(resources, fence, value)
    }
}

fn session(exec: &PanicMidBatch) -> Session {
    Session::new(
        Limits::from_capabilities(exec.capabilities()),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

fn buffer(id: u32) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc {
            size: 256,
            usage: buffer_usage::STORAGE | buffer_usage::COPY_DST,
            label: String::new(),
        },
    )
}

/// ARM A — the aborted batch had ALREADY ALLOCATED when it panicked.
///
/// `submit` never returns, so neither the id-table rollback nor the ledger rollback runs. Id 3 stays
/// allocated with no owner aware of it, and the frame's residency charge stays committed. Every later
/// batch that reuses that id is refused, which the guest reads as an unrecoverable device — the session is
/// wedged even though the process is fine.
#[test]
fn a_panic_after_an_allocation_leaks_the_id_and_wedges_the_session() {
    let mut exec = PanicMidBatch(CpuExecutor::new());
    let mut s = session(&exec);

    let before = s.ledger.totals;

    let panicked = catch_unwind(AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3), buffer(PANIC_ID)])
    }))
    .is_err();
    assert!(panicked, "the executor must have panicked");

    // The id the aborted batch created is STILL TAKEN — the rollback the `Err` path performs was skipped.
    let retry = hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3)]);
    assert!(
        matches!(retry, Err(hl_gpu::GpuError::DuplicateId { kind: "buffer", .. })),
        "id 3 must still be allocated after the panic — this leak is what wedges the session, got {retry:?}"
    );

    // The residency charge leaked too, and it leaked the WHOLE frame's charge: `submit` commits the entire
    // batch's cost up front, so both buffers are still billed (2 objects, 512 bytes) even though only one
    // of them was ever created. A connection that survives this drifts upward every time until every frame
    // trips its residency cap — the second way the same panic ends a session.
    assert_eq!(before.objects, 0, "the session starts unbilled");
    assert_eq!(
        (s.ledger.totals.objects, s.ledger.totals.bytes),
        (2, 512),
        "the aborted frame's full charge is still committed, including the command that never ran"
    );
}

/// ARM B — the aborted batch had ALLOCATED NOTHING when it panicked.
///
/// There is nothing for the skipped rollback to have undone, so the session is left exactly as it was: the
/// same ids are still free, and the next batch is served normally. The panic costs the frame and nothing
/// else.
#[test]
fn a_panic_with_nothing_allocated_leaves_the_session_fully_usable() {
    let mut exec = PanicMidBatch(CpuExecutor::new());
    let mut s = session(&exec);

    let panicked = catch_unwind(AssertUnwindSafe(|| {
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(PANIC_ID)])
    }))
    .is_err();
    assert!(panicked, "the executor must have panicked");

    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3)])
        .expect("a panic that allocated nothing must leave the session fully usable");
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[Cmd::DestroyBuffer(3)])
        .expect("and the session stays coherent afterwards");
}

/// The CONTROL that makes the two arms mean something: the same allocating batch, refused CLEANLY instead
/// of panicking, leaves NO trace. So the difference between arm A and this is entirely the skipped
/// rollback, not the allocation itself.
#[test]
fn a_clean_refusal_after_an_allocation_leaves_no_trace() {
    let mut exec = PanicMidBatch(CpuExecutor::new());
    let mut s = session(&exec);

    // `DestroyBuffer` of an id that was never created is a clean typed error from the resource tables.
    let refused = hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3), Cmd::DestroyBuffer(9)]);
    assert!(refused.is_err(), "the batch must be refused");

    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3)])
        .expect("a cleanly refused batch rolls its allocation back, so id 3 is free again");
}
