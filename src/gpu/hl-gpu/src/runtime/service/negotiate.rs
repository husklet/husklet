//! `negotiate` — the capability handshake against the injected executor's advertised capabilities.
//!
//! Called once per connection before any command flows: the executor's [`Capabilities`] are matched
//! against the guest's [`FeatureRequest`], and on success the negotiated caps are recorded in the
//! [`Session`] (and folded into the validation ceilings). An incompatible pair fails cleanly here rather
//! than surfacing as a runtime `BadTag`/`Unsupported` after the app already committed to a path. The
//! bit-matching itself lives in [`Capabilities::negotiate`](crate::protocol::model::capability::Capabilities::negotiate)
//! (protocol); this service only wires it to the executor + session.

use crate::protocol::model::capability::{Capabilities, FeatureRequest};
use crate::protocol::model::error::Result;
use crate::runtime::model::session::Session;
use crate::runtime::port::executor::GpuExecutor;

/// Negotiate `req` against the executor behind this session. Returns the executor's advertised
/// capabilities on success and records them on the session (refreshing the per-object validation
/// ceilings) so subsequent validation uses the negotiated limits.
pub fn negotiate(
    session: &mut Session,
    exec: &dyn GpuExecutor,
    req: &FeatureRequest,
) -> Result<Capabilities> {
    let caps = exec.capabilities();
    caps.negotiate(req)?;
    // Refresh the per-object validation ceilings from the negotiated caps while preserving the
    // connection-residency policy (max_connection_bytes/objects, alignment, compiled-cache ceiling).
    session.limits.caps = caps.clone();
    session.caps = Some(caps.clone());
    Ok(caps)
}
