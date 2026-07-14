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

pub use model::resources::{
    GlobalLedger, Ledger, Native, SessionResources, Totals, KIND_BUFFER, KIND_BIND_GROUP,
    KIND_EXTERNAL, KIND_FENCE, KIND_PIPELINE, KIND_SAMPLER, KIND_SHADER, KIND_SURFACE, KIND_TEXTURE,
};
pub use model::session::{Limits, Session};
pub use model::timeline::{FenceState, FenceTimeline};
pub use port::clock::{Clock, FakeClock, SystemClock};
pub use port::executor::{GpuExecutor, Presented};

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
    service::account::charge_frame(session, batch)?;
    service::dispatch::dispatch(session, exec, batch)
}
