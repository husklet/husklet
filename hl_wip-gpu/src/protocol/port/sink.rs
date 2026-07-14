//! [`CommandSink`] — the boundary port a driver submits its recorded command batches through.
//!
//! This is one of the three small ports that replace the old god-trait `GpuBackend` (§2 of the v2
//! overview): `CommandSink` (drivers submit), `GpuExecutor` (host executes), `Presenter` (compositor).
//! `CommandSink` references **only** protocol types — a `RemoteCommandSink` (encodes to transport) and an
//! `InProcessCommandSink` (calls the runtime directly) will both implement it, so a driver is written once
//! against this contract and runs either socketed or socket-free. It is deliberately object-safe
//! (`&mut dyn CommandSink`).

use crate::protocol::model::capability::{Capabilities, FeatureRequest};
use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::Result;
use crate::protocol::model::id::FenceId;

/// The contract a driver submits through. A batch is a slice of protocol [`Cmd`]s; the sink validates,
/// transports, and (eventually) executes them — the driver neither knows nor cares which.
pub trait CommandSink {
    /// Negotiate the guest's required features against the backend behind this sink, returning the
    /// backend's advertised [`Capabilities`] on success (or a typed error the driver can act on). Must be
    /// called before any resource creation, so an incompatible pair fails cleanly here rather than as a
    /// runtime `BadTag` after the app already committed to a path.
    fn negotiate(&mut self, request: &FeatureRequest) -> Result<Capabilities>;

    /// Submit a batch of protocol commands for validation + execution.
    fn submit(&mut self, batch: &[Cmd]) -> Result<()>;

    /// Block until fence `fence` reaches timeline `value`.
    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()>;
}

/// A recording [`CommandSink`] test double: it negotiates against a fixed [`Capabilities`] and records
/// every submitted batch and fence wait, so driver tests can assert exactly what a driver emitted without
/// a socket or a GPU. This is the sink the driver-layer tests (added later) drive.
pub struct RecordingSink {
    /// The capability descriptor this sink advertises during [`CommandSink::negotiate`].
    pub caps: Capabilities,
    /// Every feature request passed to [`CommandSink::negotiate`], in order.
    pub negotiated: Vec<FeatureRequest>,
    /// Every batch passed to [`CommandSink::submit`], in order.
    pub batches: Vec<Vec<Cmd>>,
    /// Every `(fence, value)` passed to [`CommandSink::wait`], in order.
    pub waits: Vec<(FenceId, u64)>,
}

impl RecordingSink {
    /// A recording sink advertising the given capability descriptor.
    pub fn new(caps: Capabilities) -> Self {
        Self { caps, negotiated: Vec::new(), batches: Vec::new(), waits: Vec::new() }
    }

    /// A recording sink advertising the full current IR surface (see [`Capabilities::full`]).
    pub fn with_full_caps() -> Self {
        Self::new(Capabilities::full("recording-sink"))
    }

    /// Total commands recorded across all submitted batches.
    pub fn command_count(&self) -> usize {
        self.batches.iter().map(Vec::len).sum()
    }

    /// Iterate every recorded command across all batches, in submission order.
    pub fn commands(&self) -> impl Iterator<Item = &Cmd> {
        self.batches.iter().flat_map(|b| b.iter())
    }
}

impl CommandSink for RecordingSink {
    fn negotiate(&mut self, request: &FeatureRequest) -> Result<Capabilities> {
        self.negotiated.push(request.clone());
        // Honour the real negotiation contract so a test that requests something the advertised caps
        // don't cover gets the same typed error a real sink would return.
        self.caps.negotiate(request)?;
        Ok(self.caps.clone())
    }

    fn submit(&mut self, batch: &[Cmd]) -> Result<()> {
        self.batches.push(batch.to_vec());
        Ok(())
    }

    fn wait(&mut self, fence: FenceId, value: u64) -> Result<()> {
        self.waits.push((fence, value));
        Ok(())
    }
}
