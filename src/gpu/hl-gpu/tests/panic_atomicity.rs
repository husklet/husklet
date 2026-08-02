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
//! That was measured here first, with these tests written as characterizations of the defect, and the
//! measurement corrected the diagnosis in a way worth recording: the leak that is actually OBSERVABLE is
//! not in the resource tables but in the residency ledger. Both leaked — `dispatch`'s table transaction was
//! left open and `submit`'s charge stayed committed — but the ledger carries its own `live` id map and is
//! consulted FIRST, so a retry was refused by the account before the tables were ever reached. Two
//! independent deaths sharing one trigger; fixing either alone leaves the session wedged.
//!
//! Both rollbacks are now unwind-safe (an RAII transaction guard in `dispatch`, an unwind boundary around
//! the charge in `submit`), so the arms below are the REGRESSION GUARD for that: a panic must cost the
//! frame and nothing else, whatever the aborted batch had already done. The arms hold the panic fixed and
//! vary only what it had allocated; if either rollback regresses, arm A diverges from the control again.

use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::runtime::port::executor::Presentation;
use hl_gpu::{
    Capabilities, Cmd, CpuExecutor, FakeClock, FenceId, GlobalLedger, GpuError, GpuExecutor,
    Limits, Result, Session,
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
/// The panic unwinds past both rollbacks, so both must survive unwinding: the id-table transaction is
/// rolled back by its guard's `Drop`, and the residency charge — committed for the WHOLE frame up front,
/// including the command that never ran — is restored across an unwind boundary. The session is left
/// exactly as it was before the frame: the id is free, the account is back to its pre-frame totals, and
/// nothing live was left behind.
#[test]
fn a_panic_after_an_allocation_leaves_no_trace() {
    let mut exec = PanicMidBatch(CpuExecutor::new());
    let mut s = session(&exec);

    let before = s.account.ledger().totals;
    assert_eq!(before.objects, 0, "the session starts unbilled");

    let refused = hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3), buffer(PANIC_ID)]);
    assert!(
        matches!(refused, Err(GpuError::Panicked(_))),
        "a panicking executor must REFUSE the frame, not unwind into the caller, got {refused:?}"
    );

    // Nothing the aborted batch created survived it.
    assert_eq!(
        s.resources.live_count(),
        0,
        "the id-table transaction must be rolled back on the unwind path"
    );
    assert_eq!(
        (
            s.account.ledger().totals.objects,
            s.account.ledger().totals.bytes
        ),
        (before.objects, before.bytes),
        "the aborted frame's residency charge must be released on the unwind path"
    );

    // And the id is genuinely reusable — the account no longer refuses it as a duplicate, which is the
    // symptom a guest actually saw (an unrecoverable device after one bad shader).
    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3)])
        .expect("id 3 must be free again after the aborted batch was rolled back");
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

    let refused = hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(PANIC_ID)]);
    assert!(
        matches!(refused, Err(GpuError::Panicked(_))),
        "a panicking executor must REFUSE the frame, got {refused:?}"
    );

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
    let refused =
        hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3), Cmd::DestroyBuffer(9)]);
    assert!(refused.is_err(), "the batch must be refused");

    hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(3)])
        .expect("a cleanly refused batch rolls its allocation back, so id 3 is free again");
}

/// A panic and a clean refusal produce the SAME observable outcome — the frame is refused and the session
/// is untouched — which is the whole point, but it means nothing downstream could tell a backend DEFECT
/// from a backend doing its job. The transport had a log line for this and nothing else did; the ack byte
/// certainly cannot carry it. The typed cause travels with the error instead, so an in-process caller and
/// the socket path agree, and a panicking backend is still findable.
#[test]
fn a_panic_is_distinguishable_from_a_clean_refusal() {
    let mut exec = PanicMidBatch(CpuExecutor::new());
    let mut s = session(&exec);

    let panicked = hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[buffer(PANIC_ID)]);
    let refused = hl_gpu::runtime::submit(&mut s, &mut exec, 0, &[Cmd::DestroyBuffer(9)]);

    assert!(
        matches!(panicked, Err(GpuError::Panicked(ref m)) if m.contains("panicked mid-batch")),
        "the panic's own message survives into the typed error, got {panicked:?}"
    );
    assert!(
        matches!(refused, Err(GpuError::UnknownId { .. })),
        "a clean refusal keeps its own typed cause, got {refused:?}"
    );
    assert!(
        !matches!(refused, Err(GpuError::Panicked(_))),
        "a clean refusal must never be reported as a backend panic"
    );
}
