//! [`InProcessCommandSink`] — a [`CommandSink`] that runs the real runtime pipeline directly, no socket.
//!
//! The third realization of the [`CommandSink`] port (alongside the recording test double and the
//! socketed [`RemoteCommandSink`](crate::transport::RemoteCommandSink)): it owns a per-connection
//! runtime [`Session`] and an injected [`GpuExecutor`], so a driver submitted through it exercises the
//! whole host pipeline in-process — [`negotiate`](crate::runtime::service::negotiate) against the
//! executor's capabilities, then [`validate → account → dispatch`](crate::runtime::submit) → the
//! executor's `execute`, with results read straight back off the runtime-owned [`SessionResources`].
//!
//! Because a driver is written once against `&mut dyn CommandSink`, the SAME driver code runs either
//! socket-free here or over the wire through `RemoteCommandSink`. This sink is what makes a full CUDA →
//! IR → runtime → CPU-executor → readback flow testable in a single process with no transport.

use crate::cpu::CpuExecutor;
use crate::protocol::codec::encode_stream;
use crate::protocol::model::capability::{Capabilities, FeatureRequest};
use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::Result;
use crate::protocol::model::id::{BufferId, FenceId};
use crate::protocol::port::sink::CommandSink;
use crate::runtime::model::resources::{GlobalLedger, SessionResources};
use crate::runtime::model::session::{Limits, Session};
use crate::runtime::port::clock::FakeClock;
use crate::runtime::port::executor::GpuExecutor;
use crate::runtime::service::{dispatch, negotiate};

/// A socket-free [`CommandSink`] that drives an owned runtime [`Session`] + injected [`GpuExecutor`]. A
/// submitted batch runs the exact runtime pipeline a socketed host would (validate → account → dispatch →
/// `executor.execute`); results are read back off the runtime-owned resources via [`resources`] (or the
/// [`read_buffer`] convenience when the executor is a [`CpuExecutor`]).
///
/// [`resources`]: InProcessCommandSink::resources
/// [`read_buffer`]: InProcessCommandSink::read_buffer
pub struct InProcessCommandSink<E: GpuExecutor> {
    session: Session,
    exec: E,
}

impl<E: GpuExecutor> InProcessCommandSink<E> {
    /// Build a sink over `exec`, deriving the session's validation/accounting ceilings from the
    /// executor's advertised [`Capabilities`] and giving it an unbounded residency budget + a fixed
    /// clock (this is an in-process bridge, not a multi-tenant residency arbiter).
    pub fn new(exec: E) -> Self {
        let limits = Limits::from_capabilities(exec.capabilities());
        let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
        Self { session, exec }
    }

    /// Build a sink over an explicit `session` + `exec`, for callers that need custom ceilings/clock.
    pub fn with_session(session: Session, exec: E) -> Self {
        Self { session, exec }
    }

    /// The per-connection runtime session (its `resources` hold every created object's native state).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// The runtime-owned id → native resource table — the readback surface a result is read from.
    pub fn resources(&self) -> &SessionResources {
        &self.session.resources
    }

    /// The injected executor (e.g. to call an executor-specific readback method).
    pub fn executor(&self) -> &E {
        &self.exec
    }

    /// The injected executor, mutably (e.g. to read work counters or configure the executor).
    pub fn executor_mut(&mut self) -> &mut E {
        &mut self.exec
    }
}

impl<E: GpuExecutor> CommandSink for InProcessCommandSink<E> {
    fn negotiate(&mut self, request: &FeatureRequest) -> Result<Capabilities> {
        negotiate::negotiate(&mut self.session, &self.exec, request)
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        // The runtime checks the encoded frame size against the negotiated ceiling; encode to get the
        // real byte count so the in-process path exercises the same frame-budget check the wire does.
        let frame_bytes = encode_stream(batch).len();
        crate::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch)?;
        Ok(())
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()> {
        dispatch::wait(&mut self.session, &mut self.exec, fence, value)
    }
}

impl InProcessCommandSink<CpuExecutor> {
    /// Read `len` bytes back from buffer `id` at `offset`, delegating to the CPU executor's readback over
    /// the runtime-owned resources. The ergonomic result-readback for the CPU reference executor.
    pub fn read_buffer(&self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        self.exec.read_buffer(&self.session.resources, id, offset, &mut out)?;
        Ok(out)
    }
}
