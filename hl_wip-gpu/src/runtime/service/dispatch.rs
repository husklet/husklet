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
use crate::runtime::port::executor::{GpuExecutor, Presented};

/// Dispatch a validated, accounted batch. The executor performs the native work (creating/destroying
/// resources behind `session.resources`, recording submits, presenting); afterwards the runtime records
/// each fence's lifecycle and any completion-signal timeline values. Returns one [`Presented`] per
/// `Present` command, in order.
pub fn dispatch(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    batch: &[Cmd],
) -> Result<Vec<Presented>> {
    // Execute inside an all-tables transaction so the batch's resource-lifecycle mutations are atomic:
    // an executor that fails PART-WAY through a batch (a Submit that fails device validation, an unknown
    // resource ref, a shader the backend can't compile — i.e. a NACK) would otherwise leave the id tables
    // half-mutated (some creates applied, some destroys already dropped) while `account` has already
    // committed the whole frame's residency charge. Rolling the tables back on failure restores them
    // EXACTLY to the pre-frame state (the ledger is rolled back by `runtime::submit`), so the connection
    // recovers: a subsequent destroy of a still-live id no longer `UnknownId`s and a retry no longer
    // `DuplicateId`s. This is the executor-side "swap-reset-on-NACK".
    session.resources.begin_txn();
    let presents = match exec.execute(&mut session.resources, batch) {
        Ok(presents) => {
            session.resources.commit_txn();
            presents
        }
        Err(e) => {
            session.resources.rollback_txn();
            return Err(e);
        }
    };

    // Reflect the batch's fence lifecycle + completion signals into the timeline only after the executor
    // has accepted the work, so a failed execute leaves the timeline untouched (failure atomicity).
    let now = session.clock.now_nanos();
    for cmd in batch {
        match cmd {
            Cmd::CreateFence(id) => session.timeline.register(*id),
            Cmd::DestroyFence(id) => session.timeline.retire(*id),
            Cmd::Submit(cb) => {
                if let Some((fence, value)) = cb.signal {
                    session.timeline.signal(fence, value, now)?;
                }
            }
            _ => {}
        }
    }
    Ok(presents)
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
