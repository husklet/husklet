//! [`GpuExecutor`] — the host-side execution port, one of the three small ports that replace the old
//! god-trait `GpuBackend` (§2 of the v2 overview).
//!
//! The runtime validates + accounts a decoded command batch and then hands it to a `GpuExecutor` it does
//! **not** choose (the host binary injects a CPU reference executor or a wgpu executor). This is the
//! contract those executors implement: it references only protocol types + [`SessionResources`], so it is
//! object-safe (`&mut dyn GpuExecutor`) and free of any wgpu/Metal/CUDA/fd type.
//!
//! Ported from `hl-gpu/src/backend.rs` (the `GpuBackend` trait), collapsing its per-resource lifecycle
//! methods into a single batch `execute` over the runtime-owned [`SessionResources`] (the executor stores
//! natives behind those id entries) while keeping the `capabilities` / fence-`wait` semantics unchanged.

use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::command::Cmd;
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, FenceId, SurfaceId, TextureId};
use crate::runtime::model::resources::SessionResources;

/// The outcome of a `Present` command executed within a batch: which surface presented which texture.
/// (The out-of-band presentable-image handle is delivered to the compositor on a separate channel; the
/// runtime surfaces only the protocol-id pairing here.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Presented {
    pub surface: SurfaceId,
    pub texture: TextureId,
}

/// The host executor a runtime `Session` drives. Object-safe so the runtime holds `&mut dyn GpuExecutor`
/// and both a pure CPU reference executor and a wgpu executor implement the same contract.
///
/// The runtime guarantees ordering: a batch reaches [`execute`](GpuExecutor::execute) only *after* it has
/// been fully validated (shape/limits) and accounted (residency charged), so an executor never has to
/// re-validate limits and a rejected frame never partially mutates native state.
pub trait GpuExecutor {
    /// The capability descriptor this executor advertises; the runtime negotiates a guest's
    /// [`FeatureRequest`](crate::protocol::model::capability::FeatureRequest) against it before any
    /// command flows.
    fn capabilities(&self) -> Capabilities;

    /// Execute a validated, accounted batch against the runtime-owned `resources`. The executor inserts
    /// its native object behind each created id and removes it on destroy (lifecycle errors surface as
    /// typed [`GpuError`](crate::protocol::model::error::GpuError)s from the resource tables), records
    /// encoder work on `Submit`, and returns one [`Presented`] per `Present` command in order.
    fn execute(
        &mut self,
        resources: &mut SessionResources,
        batch: &[Cmd],
    ) -> Result<Vec<Presented>>;

    /// Block until timeline fence `fence` reaches `value`. Serves the `CommandSink::wait` path (an
    /// out-of-band wait not carried inside a command batch); `resources` is passed so the executor can
    /// resolve the fence's native primitive.
    fn wait(&mut self, resources: &mut SessionResources, fence: FenceId, value: u64) -> Result<()>;

    /// Read `len` bytes back from buffer `id` at `offset` out of the runtime-owned `resources` — the
    /// host-side half of the device→host readback path (`CommandSink::read_buffer` /`cuMemcpyDtoH`).
    ///
    /// Additive: the default returns [`GpuError::Unsupported`] so an executor that cannot expose device
    /// memory keeps compiling; the CPU reference executor overrides it over its `SessionResources` natives.
    fn read_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
        offset: u64,
        len: usize,
    ) -> Result<Vec<u8>> {
        let _ = (resources, id, offset, len);
        Err(GpuError::Unsupported("executor: read_buffer"))
    }
}
