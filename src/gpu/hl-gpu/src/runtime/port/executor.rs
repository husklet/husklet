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
use crate::protocol::model::descriptor::{FrameSerial, SurfaceToken};
use crate::protocol::model::error::{GpuError, Result};
use crate::protocol::model::id::{BufferId, FenceId, SurfaceId, TextureId};
use crate::protocol::port::sink::FenceWait;
use crate::runtime::model::resources::SessionResources;
use crate::runtime::model::resources::Native;
use crate::runtime::model::sharing::Shared;

/// The outcome of a `Present` command executed within a batch: which surface presented which texture.
/// (The out-of-band presentable-image handle is delivered to the compositor on a separate channel; the
/// runtime surfaces only the protocol-id pairing here.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Presentation {
    pub surface: SurfaceId,
    pub token: SurfaceToken,
    pub texture: TextureId,
    pub serial: FrameSerial,
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
    /// encoder work on `Submit`, and returns one [`Presentation`] per `Present` command in order.
    fn execute(
        &mut self,
        resources: &mut SessionResources,
        batch: &[Cmd],
    ) -> Result<Vec<Presentation>>;

    /// Block until timeline fence `fence` reaches `value`. Serves the `CommandSink::wait` path (an
    /// out-of-band wait not carried inside a command batch); `resources` is passed so the executor can
    /// resolve the fence's native primitive.
    fn wait(&mut self, resources: &mut SessionResources, fence: FenceId, value: u64) -> Result<()>;

    /// Poll fence completion without blocking.
    fn poll_fence(
        &mut self,
        resources: &SessionResources,
        fence: FenceId,
        value: u64,
    ) -> Result<bool> {
        let _ = (resources, fence, value);
        Err(GpuError::Unsupported("executor: poll_fence"))
    }

    fn wait_timeout(
        &mut self,
        resources: &mut SessionResources,
        fence: FenceId,
        value: u64,
        timeout_ns: u64,
    ) -> Result<FenceWait> {
        if timeout_ns == 0 {
            return self.poll_fence(resources, fence, value).map(|done| {
                if done {
                    FenceWait::Complete
                } else {
                    FenceWait::Timeout
                }
            });
        }
        self.wait(resources, fence, value)?;
        Ok(FenceWait::Complete)
    }

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

    /// Return a shareable native alias and its authoritative logical byte length for `id`.
    ///
    /// Additive and honestly unsupported by default: an executor must not satisfy this with a copy,
    /// because callers rely on writes through either session becoming visible through the other alias.
    fn export_buffer(
        &self,
        resources: &SessionResources,
        id: BufferId,
    ) -> Result<(Shared, u64)> {
        let _ = (resources, id);
        Err(GpuError::Unsupported("executor: export_buffer"))
    }

    /// Turn a previously exported native into the owned value an importing session can insert into its
    /// resource table. `bytes` is the registry's authoritative length and must agree with the native.
    fn import_buffer(&self, resource: Shared, bytes: u64) -> Result<Native> {
        let _ = (resource, bytes);
        Err(GpuError::Unsupported("executor: import_buffer"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CpuExecutor;

    #[test]
    fn cpu_executor_honestly_refuses_buffer_aliasing() {
        let executor = CpuExecutor::new();
        let resources = SessionResources::new();
        let error = match executor.export_buffer(&resources, BufferId(1)) {
            Ok(_) => panic!("CPU storage cannot be aliased safely"),
            Err(error) => error,
        };
        assert!(matches!(error, GpuError::Unsupported("executor: export_buffer")));

        let shared: Shared = std::sync::Arc::new(());
        let error = match executor.import_buffer(shared, 4) {
            Ok(_) => panic!("CPU storage cannot import an alias safely"),
            Err(error) => error,
        };
        assert!(matches!(error, GpuError::Unsupported("executor: import_buffer")));
    }
}
