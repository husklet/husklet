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
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, FenceId, TextureId};
use crate::runtime::model::sharing::ExportId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceWait {
    Complete,
    Timeout,
}

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

    fn wait_timeout(&mut self, fence: FenceId, value: u64, timeout_ns: u64) -> Result<FenceWait> {
        if timeout_ns == 0 {
            return self.poll_fence(fence, value).map(|complete| {
                if complete {
                    FenceWait::Complete
                } else {
                    FenceWait::Timeout
                }
            });
        }
        self.wait(fence, value)?;
        Ok(FenceWait::Complete)
    }

    /// Nonblocking completion query for a timeline fence.
    fn poll_fence(&mut self, fence: FenceId, value: u64) -> Result<bool> {
        let _ = (fence, value);
        Err(GpuError::Unsupported("command sink: poll_fence"))
    }

    /// Read `len` bytes back from buffer `id` starting at `offset` — the device→host readback path that
    /// makes `cuMemcpyDtoH` / `glReadPixels`-style results available to a driver regardless of whether the
    /// sink is in-process or socketed.
    ///
    /// Additive: it has a default implementation returning [`GpuError::Unsupported`] so existing sinks and
    /// drivers that never read back keep compiling unchanged; the three real sinks override it.
    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let _ = (id, offset, len);
        Err(GpuError::Unsupported("command sink: read_buffer"))
    }

    /// Export a live local buffer and return the host-minted, process-global capability.
    fn export_buffer(&mut self, id: BufferId) -> Result<ExportId> {
        let _ = id;
        Err(GpuError::Unsupported("command sink: export_buffer"))
    }

    /// Import `export` at caller-minted local `id`, returning the registry's authoritative byte length.
    fn import_buffer(&mut self, id: BufferId, export: ExportId) -> Result<u64> {
        let _ = (id, export);
        Err(GpuError::Unsupported("command sink: import_buffer"))
    }

    fn export_texture(&mut self, id: TextureId) -> Result<ExportId> {
        let _ = id;
        Err(GpuError::Unsupported("command sink: export_texture"))
    }

    fn import_texture(&mut self, id: TextureId, export: ExportId) -> Result<u64> {
        let _ = (id, export);
        Err(GpuError::Unsupported("command sink: import_texture"))
    }

    fn map_buffer(&mut self, id: BufferId) -> Result<()> {
        let _ = id;
        Err(GpuError::Unsupported("command sink: map_buffer"))
    }

    fn unmap_buffer(&mut self, id: BufferId) -> Result<()> {
        let _ = id;
        Err(GpuError::Unsupported("command sink: unmap_buffer"))
    }
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
    /// Every `(buffer, offset, len)` passed to [`CommandSink::read_buffer`], in order — so a driver test can
    /// assert exactly what readback a driver requested.
    pub reads: Vec<(BufferId, u64, usize)>,
}

impl RecordingSink {
    /// A recording sink advertising the given capability descriptor.
    pub fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            negotiated: Vec::new(),
            batches: Vec::new(),
            waits: Vec::new(),
            reads: Vec::new(),
        }
    }

    /// A recording sink advertising the permissive test fixture
    /// (see [`Capabilities::permissive_fixture`] for what it does and does not include).
    pub fn with_full_caps() -> Self {
        Self::new(Capabilities::permissive_fixture("recording-sink"))
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

    fn poll_fence(&mut self, _fence: FenceId, _value: u64) -> Result<bool> {
        Ok(false)
    }

    /// Record the readback request and return a zero-filled buffer of the requested length (the test double
    /// executes nothing, so it has no real bytes to return).
    fn read_buffer(&mut self, id: BufferId, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.reads.push((id, offset, len));
        Ok(vec![0u8; len])
    }
}
