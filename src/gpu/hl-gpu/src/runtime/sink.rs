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
use crate::protocol::model::capability::{Capabilities, FeatureRequest};
use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::{GpuError, Result};
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
    /// Set once [`close`](InProcessCommandSink::close) has torn the session down. A closed sink rejects
    /// every further `negotiate`/`submit`/`wait`/`read_buffer` with a typed error instead of silently
    /// re-driving a released session, so a use-after-close is a clean `Err`, never a panic or a no-op.
    closed: bool,
}

impl<E: GpuExecutor> InProcessCommandSink<E> {
    /// Build a sink over `exec`, deriving the session's validation/accounting ceilings from the
    /// executor's advertised [`Capabilities`] and giving it an unbounded residency budget + a fixed
    /// clock (this is an in-process bridge, not a multi-tenant residency arbiter).
    pub fn new(exec: E) -> Self {
        let limits = Limits::from_capabilities(exec.capabilities());
        let session = Session::new(
            limits,
            GlobalLedger::unbounded(),
            Box::new(FakeClock::new(0)),
        );
        Self {
            session,
            exec,
            closed: false,
        }
    }

    /// Join this sink's session to the process-global export registry — the composition root's half of
    /// cross-connection sharing. One [`Exports`] is cloned into every session; a sink that never gets one
    /// refuses to export or import rather than sharing with nobody.
    pub fn with_exports(mut self, exports: crate::runtime::model::sharing::Exports) -> Self {
        self.session.exports = Some(exports);
        self
    }

    /// Build a sink over an explicit `session` + `exec`, for callers that need custom ceilings/clock.
    pub fn with_session(session: Session, exec: E) -> Self {
        Self {
            session,
            exec,
            closed: false,
        }
    }

    /// Explicitly tear this sink's session down: reclaim every live resource, refund this connection's
    /// residency to the shared global account, and mark the sink closed. Idempotent — a second `close`
    /// (double-teardown) is a no-op, never a panic or a double-refund. After `close`, every
    /// `negotiate`/`submit`/`wait`/`read_buffer` returns `Err(GpuError::Invalid("session closed"))`.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.session.release_all();
    }

    /// Whether this sink has been [`close`](InProcessCommandSink::close)d.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The typed rejection a closed sink returns from every command entry point.
    fn ensure_open(&self) -> Result<()> {
        if self.closed {
            return Err(GpuError::Invalid("session closed"));
        }
        Ok(())
    }

    /// The per-connection runtime session (its `resources` hold every created object's native state).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Mutable access to the session's resource tables. Needed to attach a sharing guard to a live
    /// resource; the sharing workflow owns when that happens.
    pub fn resources_mut(&mut self) -> &mut SessionResources {
        &mut self.session.resources
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
        self.ensure_open()?;
        negotiate::negotiate(&mut self.session, &self.exec, request)
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        self.ensure_open()?;
        let _span = hl_log::hl_span!(hl_log::tag::EXEC, "submit");
        // The runtime checks the encoded frame size against the negotiated ceiling; encode to get the
        // real byte count so the in-process path exercises the same frame-budget check the wire does.
        let frame_bytes = crate::protocol::codec::Encoder::stream(batch).len();
        crate::runtime::submit(&mut self.session, &mut self.exec, frame_bytes, batch)?;
        Ok(())
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()> {
        self.ensure_open()?;
        dispatch::wait(&mut self.session, &mut self.exec, fence, value)
    }

    fn poll_fence(&mut self, fence: FenceId, value: u64) -> Result<bool> {
        self.ensure_open()?;
        dispatch::poll_fence(&self.session, &mut self.exec, fence, value)
    }

    fn wait_timeout(
        &mut self,
        fence: FenceId,
        value: u64,
        timeout_ns: u64,
    ) -> Result<crate::FenceWait> {
        self.ensure_open()?;
        dispatch::wait_timeout(&mut self.session, &mut self.exec, fence, value, timeout_ns)
    }

    /// Read a buffer back straight off the runtime-owned resources via the injected executor — the
    /// socket-free half of the readback port. Works for ANY `GpuExecutor` that implements readback (the
    /// default returns `Unsupported`); the CPU reference executor serves it directly.
    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.ensure_open()?;
        let _span = hl_log::hl_span!(hl_log::tag::EXEC, "readback");
        hl_log::hl_add!(hl_log::tag::EXEC, "readback_bytes", len as u64);
        dispatch::read_buffer(&self.session, &self.exec, id, offset, len)
    }
}

impl InProcessCommandSink<CpuExecutor> {
    /// Read `len` bytes back from buffer `id` at `offset`, delegating to the CPU executor's readback over
    /// the runtime-owned resources. The ergonomic result-readback for the CPU reference executor.
    pub fn read_buffer(&self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.ensure_open()?;
        let mut out = vec![0u8; len];
        self.exec
            .read_buffer(&self.session.resources, id, offset, &mut out)?;
        Ok(out)
    }
}
