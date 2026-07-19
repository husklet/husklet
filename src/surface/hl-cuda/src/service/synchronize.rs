//! `cuCtxSynchronize` / `cuStreamSynchronize` — the host-blocking barrier.
//!
//! Lowered as a real timeline-fence barrier: create a fence, submit an empty command buffer that signals
//! it, then block on [`CommandSink::wait`] until the fence reaches that value, and release the fence.
//! With the current synchronous submit model every prior op has already completed, so this is a clean
//! ordering point; once async streams land (deferred) the same shape carries per-stream ordering.

use crate::model::context::CudaContext;
use crate::model::stream::Stream;
use hl_gpu::{Cmd, CommandBuffer, CommandSink, FenceId, GpuError, Result};

/// Emit `CreateFence` + an empty signalling `Submit`, block on the fence, then `DestroyFence`.
impl CudaContext {
    fn barrier(&mut self, sink: &mut dyn CommandSink) -> Result<()> {
        let _s = hl_log::hl_span!(hl_log::tag::CUDA, "sync");
        let fence = self.alloc_fence();
        let value = self.next_fence_value();
        hl_log::hl_debug!(hl_log::tag::CUDA, "sync fence={} value={}", fence, value);
        sink.submit(&[
            Cmd::CreateFence(fence),
            Cmd::Submit(CommandBuffer {
                encoder: Vec::new(),
                signal: Some((fence, value)),
            }),
        ])?;
        sink.wait(FenceId(fence), value)?;
        sink.submit(&[Cmd::DestroyFence(fence)])?;
        Ok(())
    }

    /// `cuCtxSynchronize()` — block until all previously-submitted work on the context completes.
    pub fn synchronize(&mut self, sink: &mut dyn CommandSink) -> Result<()> {
        self.barrier(sink)
    }

    /// `cuStreamSynchronize(stream)` — block until `stream`'s work completes. Errors
    /// (`CUDA_ERROR_INVALID_HANDLE` analogue) on an unknown stream handle.
    pub fn synchronize_stream(&mut self, sink: &mut dyn CommandSink, stream: Stream) -> Result<()> {
        if !self.streams.is_valid(stream) {
            hl_log::hl_warn!(hl_log::tag::CUDA, "stream_sync invalid handle");
            return Err(GpuError::Invalid(
                "cuStreamSynchronize: invalid stream handle",
            ));
        }
        self.barrier(sink)
    }
}
