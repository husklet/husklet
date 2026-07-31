//! `runtime` — the per-connection [`Session`] that owns mutable client state and drives an injected
//! [`GpuExecutor`]. It VALIDATES and ACCOUNTS a decoded command batch, then DISPATCHES it to the
//! executor, updating the fence timeline; it does **not** choose which executor exists and contains no
//! socket/GPU/platform code.
//!
//! Layering (v2 doctrine): [`model`] owns the state (session, resources, timeline), [`port`] owns the
//! boundary traits it drives through ([`GpuExecutor`], [`Clock`]), [`service`] owns the workflows
//! (negotiate → validate → account → dispatch), and this facade only re-exports + wires the fixed
//! pipeline. Built ON TOP of [`crate::protocol`]; it does not change it.
//!
//! Reading direction (§2): `… → protocol → transport → runtime → executor → PresentableImage → …`.

pub mod model;
pub mod port;
pub mod service;
pub mod sink;

pub use model::resources::{
    GlobalLedger, Ledger, Native, SessionResources, Totals, KIND_BIND_GROUP, KIND_BUFFER,
    KIND_EXTERNAL, KIND_FENCE, KIND_PIPELINE, KIND_SAMPLER, KIND_SHADER, KIND_SURFACE,
    KIND_TEXTURE,
};
pub use model::session::{Limits, Session};
pub use model::timeline::{FenceState, FenceTimeline};
pub use port::clock::{Clock, FakeClock, SystemClock};
pub use port::executor::{GpuExecutor, Presentation};
pub use sink::InProcessCommandSink;

use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::Result;

/// Run one decoded frame through the fixed runtime pipeline: **validate → account → dispatch**. A failure
/// in an earlier stage short-circuits before any later stage runs (and before the executor is touched),
/// giving failure atomicity: a malformed or over-budget frame rejected at `validate` or `account` never
/// reaches the executor at all, and one the executor NACKs is rolled back.
///
/// The guarantee's exact scope — a rejected batch leaves the ID LIFECYCLE and the RESIDENCY LEDGER as they
/// were, while resource CONTENTS are not transactional — is stated with its reasoning in
/// [`service::dispatch::dispatch`]. Read it before relying on atomicity for anything a batch WROTE.
///
/// `negotiate` is a per-connection prelude (call it once at connect); `frame_bytes` is the encoded
/// size of the frame the batch decoded from (checked against the negotiated frame ceiling).
pub fn submit(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    frame_bytes: usize,
    batch: &[Cmd],
) -> Result<Vec<Presentation>> {
    service::validate::validate(&session.limits, frame_bytes, batch)?;
    // Snapshot the residency ledger BEFORE the charge so a later executor NACK can undo it. `account`
    // commits the whole frame's charge up front (both the connection ledger and its slice of the shared
    // global account); `dispatch` then hands the batch to the executor, which can still NACK it. Without
    // this rollback the charge would stick even though nothing rendered, so the connection's residency
    // would climb until every frame trips the cap — exactly the "NACK never recovers" failure.
    let ledger_before = session.ledger.clone();
    session.charge_frame(batch)?;
    // Dispatch across an unwind boundary so the charge is rolled back on EVERY abnormal exit, not only on
    // a clean NACK. An executor that panics unwinds past the error arm below, and the charge it left
    // committed was the WHOLE frame's — including the commands that never ran, because `charge_frame`
    // commits up front (deliberately: that is what refuses an over-budget frame BEFORE anything is
    // allocated). Worse, the charge carries the ledger's own `live` id map, so a leaked charge refuses
    // every later create of those ids with `DuplicateId` even once the resource tables have been rolled
    // back correctly. That is the leak `tests/panic_atomicity.rs` arm A measures; it is a SEPARATE death
    // from the id-table leak that `dispatch` now guards, and it is the one that is actually observable,
    // because the account rejects the retry before the tables are ever consulted.
    //
    // A panic is then REFUSED rather than re-raised, which makes the outcome of a backend defect
    // deterministic instead of depending on who happens to be above it on the stack. The socket transport
    // has its own unwind boundary and would have NACKed the frame anyway; an in-process caller
    // (`InProcessCommandSink`, the CPU oracle harnesses, every test) had none, so the same defect either
    // NACKed one frame or killed the caller depending only on the deployment. Converting here — at the
    // stage that already owns "the frame either happened or it did not" — gives every caller the same
    // answer. The typed [`GpuError::Panicked`] keeps the CAUSE legible, which the ack byte alone cannot.
    //
    // Scope: this wraps `dispatch`, i.e. EXECUTOR work. Validation and accounting run before it and are
    // deliberately outside, so a panic in the runtime's own logic still surfaces as a panic.
    let dispatched = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        service::dispatch::dispatch(session, exec, batch)
    }));
    // Once an abnormal abort is an ordinary `Err`, it takes the SAME rollback path as every other
    // rejection — one place that restores the account, reached by both causes.
    let dispatched = dispatched.unwrap_or_else(|payload| {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "non-string panic payload".to_owned());
        Err(crate::protocol::model::error::GpuError::Panicked(message))
    });
    match dispatched {
        Ok(presents) => Ok(presents),
        Err(e) => {
            // Executor NACK: `dispatch` already rolled the id tables back to the pre-frame state; roll the
            // account charge back to match so the whole submit is atomic — the connection is left EXACTLY
            // as before the frame (ledger + global + tables), ready for a retry or a subsequent destroy.
            // Restoring a previously-valid, smaller-or-equal contribution can never exceed a ceiling, so
            // the global re-commit cannot fail; ignore its result and always restore the local ledger.
            let _ = session
                .global
                .commit(session.ledger.totals, ledger_before.totals);
            session.ledger = ledger_before;
            Err(e)
        }
    }
}
