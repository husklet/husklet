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
    GlobalLedger, Ledger, Native, SessionResources, Totals, KIND_BUFFER, KIND_BIND_GROUP,
    KIND_EXTERNAL, KIND_FENCE, KIND_PIPELINE, KIND_SAMPLER, KIND_SHADER, KIND_SURFACE, KIND_TEXTURE,
};
pub use model::session::{Limits, Session};
pub use model::timeline::{FenceState, FenceTimeline};
pub use port::clock::{Clock, FakeClock, SystemClock};
pub use port::executor::{GpuExecutor, Presented};
pub use sink::InProcessCommandSink;

use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::Result;

/// Run one decoded frame through the fixed runtime pipeline: **validate → account → dispatch**. A failure
/// in an earlier stage short-circuits before any later stage runs (and before the executor is touched),
/// giving failure atomicity: a malformed or over-budget frame never partially mutates residency or native
/// state. `negotiate` is a per-connection prelude (call it once at connect); `frame_bytes` is the encoded
/// size of the frame the batch decoded from (checked against the negotiated frame ceiling).
pub fn submit(
    session: &mut Session,
    exec: &mut dyn GpuExecutor,
    frame_bytes: usize,
    batch: &[Cmd],
) -> Result<Vec<Presented>> {
    service::validate::validate(&session.limits, frame_bytes, batch)?;
    // Snapshot the residency ledger BEFORE the charge so a later executor NACK can undo it. `account`
    // commits the whole frame's charge up front (both the connection ledger and its slice of the shared
    // global account); `dispatch` then hands the batch to the executor, which can still NACK it. Without
    // this rollback the charge would stick even though nothing rendered, so the connection's residency
    // would climb until every frame trips the cap — exactly the "NACK never recovers" failure.
    let ledger_before = session.ledger.clone();
    service::account::charge_frame(session, batch)?;
    match service::dispatch::dispatch(session, exec, batch) {
        Ok(presents) => Ok(presents),
        Err(e) => {
            // Executor NACK: `dispatch` already rolled the id tables back to the pre-frame state; roll the
            // account charge back to match so the whole submit is atomic — the connection is left EXACTLY
            // as before the frame (ledger + global + tables), ready for a retry or a subsequent destroy.
            // Restoring a previously-valid, smaller-or-equal contribution can never exceed a ceiling, so
            // the global re-commit cannot fail; ignore its result and always restore the local ledger.
            let _ = session.global.commit(session.ledger.totals, ledger_before.totals);
            session.ledger = ledger_before;
            Err(e)
        }
    }
}
