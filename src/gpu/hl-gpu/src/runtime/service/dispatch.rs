//! `dispatch` — hand a validated + accounted batch to the executor and update the fence timeline.
//!
//! Final stage of the runtime pipeline (decode → validate → account → **dispatch**). By the time a batch
//! reaches here it has passed [`validate`](super::validate) and [`charge_frame`](super::account::charge_frame),
//! so this stage only drives the executor over the runtime-owned [`SessionResources`] and reflects the
//! batch's fence lifecycle (register/retire) and completion signals into the [`FenceTimeline`], stamped
//! with the session [`Clock`](crate::runtime::port::clock::Clock). Ported from `hl-gpu/src/replay.rs`
//! (the `apply`/`replay` loop that drove a `GpuBackend`), collapsed onto the single batch
//! [`GpuExecutor::execute`] call.

use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::Result;
use crate::protocol::model::id::{BufferId, FenceId};
use crate::runtime::model::session::Session;
use crate::runtime::port::executor::{GpuExecutor, Presentation};

/// Dispatch a validated, accounted batch. The executor performs the native work (creating/destroying
/// resources behind `session.resources`, recording submits, presenting); afterwards the runtime records
/// each fence's lifecycle and any completion-signal timeline values. Returns one [`Presentation`] per
/// `Present` command, in order.
pub fn dispatch(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    batch: &[Cmd],
) -> Result<Vec<Presentation>> {
    // Reflect the batch's fence lifecycle + completion signals onto a COPY of the timeline first. A signal
    // that moves a fence backwards is a typed rejection, and raising it after the executor already applied
    // (and committed) the batch would leave resources live on the executor that `runtime::submit` has just
    // un-charged from the ledger — an accounting divergence a guest could repeat to grow past its residency
    // bound. Pre-flighting makes the rejection happen before ANY mutation; the copy is then installed
    // wholesale once the executor has accepted the work, so the timeline moves exactly when the frame does.
    let now = session.clock.now_nanos();
    let mut next_timeline = session.timeline.clone();
    for cmd in batch {
        match cmd {
            Cmd::CreateFence(id) => next_timeline.register(*id),
            Cmd::DestroyFence(id) => next_timeline.retire(*id),
            Cmd::Submit(cb) => {
                if let Some((fence, value)) = cb.signal {
                    next_timeline.signal(fence, value, now)?;
                }
            }
            _ => {}
        }
    }

    // SCOPE OF THE TRANSACTION — the contract, stated exactly:
    //
    //   A rejected batch leaves the ID LIFECYCLE and the RESIDENCY LEDGER precisely as they were.
    //   Resource CONTENTS are NOT transactional.
    //
    // The table journal reverts inserts and removes, so a NACKed frame's creates disappear and its destroys
    // come back — that is what lets a connection retry. It cannot revert a write made THROUGH a handle that
    // survives the rollback: if a batch clears a texture and a LATER command in the same batch fails, the
    // clear stays. Verified, not assumed: a `[Submit{clear T}, CreateBuffer(live id)]` batch NACKs on the
    // duplicate id and leaves T cleared.
    //
    // This is the honest intersection rather than a gap to close. A copy-on-write snapshot of every mutated
    // resource per batch is the only way to make contents transactional, and it is unaffordable at
    // browser frame rates; more decisively, the wgpu executor cannot roll back GPU memory at all, so a
    // promise of content atomicity would be one the real executor could never keep. Executors narrow the
    // window instead by validating a whole command buffer before mutating anything (see the CPU executor's
    // `EncoderState::validate`), which is why a single `Submit` is content-atomic even though a
    // multi-command batch is not.
    //
    // Execute inside an all-tables transaction so the batch's resource-lifecycle mutations are atomic:
    // an executor that fails PART-WAY through a batch (a Submit that fails device validation, an unknown
    // resource ref, a shader the backend can't compile — i.e. a NACK) would otherwise leave the id tables
    // half-mutated (some creates applied, some destroys already dropped) while `account` has already
    // committed the whole frame's residency charge. Rolling the tables back on failure restores them
    // EXACTLY to the pre-frame state (the ledger is rolled back by `runtime::submit`), so the connection
    // recovers: a subsequent destroy of a still-live id no longer `UnknownId`s and a retry no longer
    // `DuplicateId`s. This is the executor-side "swap-reset-on-NACK".
    // The transaction is scoped by an RAII guard rather than by matching on the result, because an
    // executor can leave this stage in a THIRD way that the match could not see: by PANICKING. A panic
    // unwinds straight past a rollback written on the error arm, so every mutation the batch had already
    // applied stayed applied, with the transaction still open and no owner aware of it — the id the
    // aborted batch created remained allocated forever, and every later batch reusing it was refused,
    // which a guest reads as an unrecoverable device. That was measured, not theorised: see
    // `tests/panic_atomicity.rs`, whose arm A is exactly this leak and whose arm B shows the same panic
    // costs nothing when the aborted batch had allocated nothing — which is why the same defect presented
    // as "cost a thread" on one run and "killed the session" on another.
    //
    // `Drop` runs during unwinding, so the guard restores the pre-frame tables on the panic path too, and
    // an abnormal abort becomes indistinguishable from a clean refusal.
    let presents = {
        let txn = Transaction::begin(&mut session.resources);
        let presents = exec.execute(txn.resources, batch)?;
        txn.commit();
        presents
    };

    // Install the pre-flighted timeline only after the executor accepted the work, so a failed execute
    // leaves the timeline untouched (failure atomicity).
    session.timeline = next_timeline;
    Ok(presents)
}

/// The in-flight all-tables transaction, scoped so it is resolved on EVERY exit path — including an
/// unwind out of the executor.
///
/// Holding the `&mut SessionResources` is what makes this airtight: the tables cannot be reached except
/// through the guard while a transaction is open, so no path can forget to end one. `commit` consumes the
/// guard; anything else — an early `?`, a panic — drops it and rolls back.
struct Transaction<'a> {
    resources: &'a mut super::super::model::resources::SessionResources,
    committed: bool,
}

impl<'a> Transaction<'a> {
    fn begin(resources: &'a mut super::super::model::resources::SessionResources) -> Self {
        resources.begin_txn();
        Self {
            resources,
            committed: false,
        }
    }

    /// Accept the batch: parked natives are freed for real and the tables keep the batch's mutations.
    fn commit(mut self) {
        self.resources.commit_txn();
        self.committed = true;
    }
}

impl Drop for Transaction<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.resources.rollback_txn();
        }
    }
}

/// Service the `CommandSink::wait` path: block on the executor until fence `fence` reaches `value`. Not
/// part of a command batch — an out-of-band wait the transport layer forwards.
pub fn wait(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    fence: FenceId,
    value: u64,
) -> Result<()> {
    exec.wait(&mut session.resources, fence, value)
}

pub fn poll_fence(
    session: &Session,
    exec: &mut dyn GpuExecutor,
    fence: FenceId,
    value: u64,
) -> Result<bool> {
    exec.poll_fence(&session.resources, fence, value)
}

pub fn wait_timeout(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    fence: FenceId,
    value: u64,
    timeout_ns: u64,
) -> Result<crate::FenceWait> {
    exec.wait_timeout(&mut session.resources, fence, value, timeout_ns)
}

/// Service the device→host readback path: return `len` bytes of buffer `id` at `offset` from the executor
/// over the runtime-owned resources. Not part of a command batch — an out-of-band query the transport layer
/// forwards to answer a `CommandSink::read_buffer` / `cuMemcpyDtoH`.
pub fn read_buffer(
    session: &Session,
    exec: &dyn GpuExecutor,
    id: BufferId,
    offset: u64,
    len: usize,
) -> Result<Vec<u8>> {
    exec.read_buffer(&session.resources, id, offset, len)
}
