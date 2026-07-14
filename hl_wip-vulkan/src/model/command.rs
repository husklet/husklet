//! The `VkCommandBuffer` recording model + its lifecycle state machine.
//!
//! Ported from `hl-shim-vk/src/reg.rs` (`CmdBufRec`, `CommandBufferState`), mirroring MoltenVK's
//! `MVKCommandBuffer` flag model. A recording buffer accumulates the [`Enc`] encoder ops each `vkCmd*`
//! lowers to (via [`crate::service::record`]); `vkQueueSubmit` wraps the encoder in a
//! [`hl_gpu::Cmd::Submit`] ([`crate::service::submit`]).

use super::pipeline::PipelineKind;
use super::sync::DeferredOp;
use crate::VkQueryPool;
use hl_gpu::protocol::model::command::Enc;

/// The Vulkan command-buffer lifecycle state (spec §6). Ported from MoltenVK's flag model:
/// * `Initial`    — freshly allocated or reset; can only be begun.
/// * `Recording`  — inside `vkBeginCommandBuffer`; accepts `vkCmd*`.
/// * `Executable` — `vkEndCommandBuffer` succeeded; can be submitted.
/// * `Pending`    — submitted and not yet completed; must not be touched.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CommandBufferState {
    #[default]
    Initial,
    Recording,
    Executable,
    Pending,
}

/// The pipeline dynamic state a command buffer records via `vkCmdSet*` that the hl-GPU IR / CPU
/// rasterizer does NOT model (line width, depth bias, blend constants, stencil masks/reference). These
/// are recorded verbatim here — observable, honest command state — but carry no encoder op, because the
/// software rasterizer draws hairline-width, unbiased, blend-const-free, stencil-less triangles. Ported
/// from MoltenVK's `MVKCommandEncoderState`; viewport/scissor are the exception (they DO lower to
/// [`Enc::SetViewport`]/[`Enc::SetScissor`], so they are not held here).
#[derive(Clone, PartialEq, Debug)]
pub struct DynamicState {
    /// `vkCmdSetLineWidth` (default 1.0). The rasterizer fills triangles; wide lines are not modeled.
    pub line_width: f32,
    /// `vkCmdSetDepthBias` `(constantFactor, clamp, slopeFactor)`. No depth buffer in the color oracle.
    pub depth_bias: (f32, f32, f32),
    /// `vkCmdSetBlendConstants` RGBA. Constant-color blend factors are not modeled.
    pub blend_constants: [f32; 4],
    /// `vkCmdSetStencilCompareMask` `(front, back)`. No stencil buffer in the color oracle.
    pub stencil_compare_mask: (u32, u32),
    /// `vkCmdSetStencilWriteMask` `(front, back)`.
    pub stencil_write_mask: (u32, u32),
    /// `vkCmdSetStencilReference` `(front, back)`.
    pub stencil_reference: (u32, u32),
}

impl Default for DynamicState {
    fn default() -> Self {
        // Vulkan's initial dynamic state: line width 1.0, everything else zero.
        DynamicState {
            line_width: 1.0,
            depth_bias: (0.0, 0.0, 0.0),
            blend_constants: [0.0; 4],
            stencil_compare_mask: (0, 0),
            stencil_write_mask: (0, 0),
            stencil_reference: (0, 0),
        }
    }
}

/// A command buffer's recorded encoder + the transient recording state (bound pipeline, pending bind
/// groups, render-pass depth) needed to lower the next `vkCmd*`. Mirrors `MVKCommandBuffer`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CmdBufRec {
    pub state: CommandBufferState,
    /// The recorded encoder ops (the `Enc` stream `vkQueueSubmit` ships as one `Cmd::Submit`).
    pub enc: Vec<Enc>,
    /// hl-GPU pipeline id of the last `vkCmdBindPipeline` (replayed into the next pass).
    pub bound_pipeline: Option<u32>,
    pub bound_pipeline_kind: Option<PipelineKind>,
    /// `(set index, bind-group IR id)` bound by `vkCmdBindDescriptorSets`, replayed into the next pass.
    pub pending_bind_groups: Vec<(u32, u32)>,
    pub in_render_pass: bool,
    /// The IR texture id of the active render pass's color target (set at `vkCmdBeginRenderPass`) — the
    /// target `vkCmdClearAttachments` clears while inside the pass.
    pub active_render_texture: Option<u32>,
    /// The `(pool, query)` opened by `vkCmdBeginQuery` (spec §17.4: at most one active query of a type).
    /// `vkCmdEndQuery` resolves the matching slot.
    pub active_query: Option<(VkQueryPool, u32)>,
    /// Buffer writes (`vkCmdFillBuffer` / `vkCmdUpdateBuffer`) — `(ir buffer id, offset, bytes)`, flushed
    /// as `Cmd::WriteBuffer`s at the start of the owning `vkQueueSubmit`. Kept out of the `Enc` encoder
    /// (there is no encoder-level write op) exactly as `hl-shim-vk` does.
    pub buffer_writes: Vec<(u32, u64, Vec<u8>)>,
    /// Device event/query ops (`vkCmdSetEvent`/`vkCmdResetEvent`/`vkCmdResetQueryPool`/`vkCmdEndQuery`/
    /// `vkCmdWriteTimestamp`/`vkCmdCopyQueryPoolResults`) applied at (synchronous) submit completion.
    pub deferred: Vec<DeferredOp>,
    /// Pipeline dynamic state recorded by `vkCmdSet*` that the IR does not model (see [`DynamicState`]).
    pub dynamic: DynamicState,
    /// The push-constant block bytes recorded by `vkCmdPushConstants` (offset-indexed, grown on demand).
    /// The hl-GPU IR has no push-constant channel yet, so this is honest recorded command state a later
    /// increment can stage into a per-draw uniform bind — the bytes are retained, never silently dropped.
    pub push_constants: Vec<u8>,
}

impl CmdBufRec {
    /// A freshly-allocated command buffer (pool-owned, `Initial`).
    pub fn initial() -> Self {
        CmdBufRec::default()
    }

    /// Clear the recorded contents back to a just-begun state (MoltenVK `MVKCommandBuffer::reset`).
    pub fn reset_recording(&mut self) {
        self.enc.clear();
        self.bound_pipeline = None;
        self.bound_pipeline_kind = None;
        self.pending_bind_groups.clear();
        self.in_render_pass = false;
        self.active_render_texture = None;
        self.active_query = None;
        self.buffer_writes.clear();
        self.deferred.clear();
        self.dynamic = DynamicState::default();
        self.push_constants.clear();
    }
}
