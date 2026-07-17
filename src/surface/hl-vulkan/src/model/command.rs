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

    // ---- extended dynamic state 1/2/3 (VK_EXT_extended_dynamic_state{,2,3} + core 1.3) -------------
    // Recorded verbatim as observable, honest command state exactly like the fields above. The software
    // color rasterizer models none of this fixed-function state, so each carries NO encoder op — but the
    // value is retained (never silently dropped), so a later increment can consume it and a test can
    // assert it was recorded. Every field's initial value is the Vulkan "unset" default (0 / false).
    /// `vkCmdSetCullMode` — raw `VkCullModeFlags`.
    pub cull_mode: u32,
    /// `vkCmdSetFrontFace` — raw `VkFrontFace`.
    pub front_face: i32,
    /// `vkCmdSetPrimitiveTopology` — raw `VkPrimitiveTopology`.
    pub primitive_topology: i32,
    /// `vkCmdSetPrimitiveRestartEnable`.
    pub primitive_restart_enable: bool,
    /// `vkCmdSetRasterizerDiscardEnable`.
    pub rasterizer_discard_enable: bool,
    /// `vkCmdSetDepthTestEnable`.
    pub depth_test_enable: bool,
    /// `vkCmdSetDepthWriteEnable`.
    pub depth_write_enable: bool,
    /// `vkCmdSetDepthCompareOp` — raw `VkCompareOp`.
    pub depth_compare_op: i32,
    /// `vkCmdSetDepthBoundsTestEnable`.
    pub depth_bounds_test_enable: bool,
    /// `vkCmdSetDepthBounds` `(min, max)`.
    pub depth_bounds: (f32, f32),
    /// `vkCmdSetDepthBiasEnable`.
    pub depth_bias_enable: bool,
    /// `vkCmdSetStencilTestEnable`.
    pub stencil_test_enable: bool,
    /// `vkCmdSetStencilOp` front face `(failOp, passOp, depthFailOp, compareOp)` (raw `VkStencilOp`/`VkCompareOp`).
    pub stencil_op_front: (i32, i32, i32, i32),
    /// `vkCmdSetStencilOp` back face `(failOp, passOp, depthFailOp, compareOp)`.
    pub stencil_op_back: (i32, i32, i32, i32),
    /// `vkCmdSetLineStipple` `(factor, pattern)`.
    pub line_stipple: (u32, u16),
    /// `vkCmdSetLineStippleEnableEXT`.
    pub line_stipple_enable: bool,
    /// `vkCmdSetVertexInputEXT` binding count (the vertex-input state is recorded as unmodeled).
    pub vertex_binding_count: u32,
    // ---- extended dynamic state 3 ----
    /// `vkCmdSetRasterizationSamplesEXT` — raw `VkSampleCountFlagBits`.
    pub rasterization_samples: u32,
    /// `vkCmdSetSampleMaskEXT` (first 32-bit word).
    pub sample_mask: u32,
    /// `vkCmdSetAlphaToCoverageEnableEXT`.
    pub alpha_to_coverage_enable: bool,
    /// `vkCmdSetAlphaToOneEnableEXT`.
    pub alpha_to_one_enable: bool,
    /// `vkCmdSetLogicOpEnableEXT`.
    pub logic_op_enable: bool,
    /// `vkCmdSetLogicOpEXT` — raw `VkLogicOp`.
    pub logic_op: i32,
    /// `vkCmdSetPolygonModeEXT` — raw `VkPolygonMode`.
    pub polygon_mode: i32,
    /// `vkCmdSetPatchControlPointsEXT`.
    pub patch_control_points: u32,
    /// `vkCmdSetTessellationDomainOriginEXT` — raw `VkTessellationDomainOrigin`.
    pub tessellation_domain_origin: i32,
    /// `vkCmdSetProvokingVertexModeEXT` — raw `VkProvokingVertexModeEXT`.
    pub provoking_vertex_mode: i32,
    /// `vkCmdSetLineRasterizationModeEXT` — raw `VkLineRasterizationModeEXT`.
    pub line_rasterization_mode: i32,
    /// `vkCmdSetDepthClampEnableEXT`.
    pub depth_clamp_enable: bool,
    /// `vkCmdSetDepthClipEnableEXT`.
    pub depth_clip_enable: bool,
    /// `vkCmdSetDepthClipNegativeOneToOneEXT`.
    pub depth_clip_negative_one_to_one: bool,
    /// `vkCmdSetConservativeRasterizationModeEXT` — raw `VkConservativeRasterizationModeEXT`.
    pub conservative_rasterization_mode: i32,
    /// `vkCmdSetExtraPrimitiveOverestimationSizeEXT`.
    pub extra_primitive_overestimation_size: f32,
    /// `vkCmdSetSampleLocationsEnableEXT`.
    pub sample_locations_enable: bool,
    /// `vkCmdSetRasterizationStreamEXT`.
    pub rasterization_stream: u32,
    /// `vkCmdSetColorBlendEnableEXT` — per-attachment enable (raw `VkBool32`), indexed by attachment.
    pub color_blend_enables: Vec<u32>,
    /// `vkCmdSetColorWriteMaskEXT` — per-attachment `VkColorComponentFlags`.
    pub color_write_masks: Vec<u32>,
    /// `vkCmdSetColorWriteEnableEXT` — per-attachment enable (raw `VkBool32`).
    pub color_write_enables: Vec<u32>,
}

impl Default for DynamicState {
    fn default() -> Self {
        // Vulkan's initial dynamic state: line width 1.0, everything else zero/false.
        DynamicState {
            line_width: 1.0,
            depth_bias: (0.0, 0.0, 0.0),
            blend_constants: [0.0; 4],
            stencil_compare_mask: (0, 0),
            stencil_write_mask: (0, 0),
            stencil_reference: (0, 0),
            cull_mode: 0,
            front_face: 0,
            primitive_topology: 0,
            primitive_restart_enable: false,
            rasterizer_discard_enable: false,
            depth_test_enable: false,
            depth_write_enable: false,
            depth_compare_op: 0,
            depth_bounds_test_enable: false,
            depth_bounds: (0.0, 0.0),
            depth_bias_enable: false,
            stencil_test_enable: false,
            stencil_op_front: (0, 0, 0, 0),
            stencil_op_back: (0, 0, 0, 0),
            line_stipple: (1, 0),
            line_stipple_enable: false,
            vertex_binding_count: 0,
            rasterization_samples: 1,
            sample_mask: u32::MAX,
            alpha_to_coverage_enable: false,
            alpha_to_one_enable: false,
            logic_op_enable: false,
            logic_op: 0,
            polygon_mode: 0,
            patch_control_points: 0,
            tessellation_domain_origin: 0,
            provoking_vertex_mode: 0,
            line_rasterization_mode: 0,
            depth_clamp_enable: false,
            depth_clip_enable: false,
            depth_clip_negative_one_to_one: false,
            conservative_rasterization_mode: 0,
            extra_primitive_overestimation_size: 0.0,
            sample_locations_enable: false,
            rasterization_stream: 0,
            color_blend_enables: Vec::new(),
            color_write_masks: Vec::new(),
            color_write_enables: Vec::new(),
        }
    }
}

/// A command buffer's recorded encoder + the transient recording state (bound pipeline, pending bind
/// groups, render-pass depth) needed to lower the next `vkCmd*`. Mirrors `MVKCommandBuffer`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct CmdBufRec {
    pub state: CommandBufferState,
    /// `VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT` was set at `vkBeginCommandBuffer`. A one-time buffer
    /// must not be re-submitted (it becomes non-resubmittable once its submit completes); a buffer without
    /// this flag returns to `Executable` after a (synchronous) submit completes, so the standard "record
    /// once, submit every frame" pattern (vkcube's per-image draw buffers) keeps working across frames.
    pub one_time_submit: bool,
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
    /// Running OCCLUSION sample count for the currently-open occlusion query — `Some(accumulated
    /// fragments)` between a `vkCmdBeginQuery`/`vkCmdEndQuery` on an OCCLUSION pool, else `None`. Each
    /// draw recorded inside the scope adds its scissor-clipped render-area footprint (see
    /// [`Self::occlusion_coverage`]); `vkCmdEndQuery` moves the total into the query slot. This gives an
    /// occlusion result that reflects reality (>0 when a draw is visible, 0 when nothing rasterizes /
    /// the draw is fully scissored) instead of the old conservative constant `0`.
    pub occlusion_accum: Option<u64>,
    /// The active render pass's `(width, height)` in pixels (the color attachment extent), captured at
    /// `vkCmdBeginRenderPass` / `vkCmdBeginRendering`. The full-frame sample footprint the occlusion
    /// model clips each draw's scissor against. `(0, 0)` outside a render pass.
    pub render_extent: (u32, u32),
    /// The current dynamic scissor `(x, y, w, h)` set by `vkCmdSetScissor`, or `None` (no scissor →
    /// the full [`Self::render_extent`]). Reset at each render-pass begin. Used by the occlusion model
    /// to bound a draw's covered samples exactly as the executor's `Enc::SetScissor` bounds the raster.
    pub scissor: Option<(u32, u32, u32, u32)>,
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

    /// The OCCLUSION-query sample footprint of one draw: the current scissor (or the full render
    /// extent when no dynamic scissor is set) clipped to the render area, in fragments, times
    /// `instance_count`. This is the count of samples a full-coverage draw (e.g. a full-screen quad —
    /// what an occlusion probe uses) writes, matching exactly what the executor's `Enc::SetScissor`
    /// lets rasterize; for a sub-scissor primitive it is an upper bound. A zero-area scissor (a fully
    /// scissored draw) yields 0 — the "occluded" case.
    pub fn occlusion_coverage(&self, instance_count: u32) -> u64 {
        let (rw, rh) = self.render_extent;
        let (sx, sy, sw, sh) = self.scissor.unwrap_or((0, 0, rw, rh));
        // Intersect the scissor rectangle with the render area [0,rw) x [0,rh).
        let x0 = sx.min(rw);
        let y0 = sy.min(rh);
        let x1 = sx.saturating_add(sw).min(rw);
        let y1 = sy.saturating_add(sh).min(rh);
        let w = x1.saturating_sub(x0) as u64;
        let h = y1.saturating_sub(y0) as u64;
        w * h * instance_count as u64
    }

    /// Clear the recorded contents back to a just-begun state (MoltenVK `MVKCommandBuffer::reset`).
    pub fn reset_recording(&mut self) {
        self.one_time_submit = false;
        self.enc.clear();
        self.bound_pipeline = None;
        self.bound_pipeline_kind = None;
        self.pending_bind_groups.clear();
        self.in_render_pass = false;
        self.active_render_texture = None;
        self.active_query = None;
        self.occlusion_accum = None;
        self.render_extent = (0, 0);
        self.scissor = None;
        self.buffer_writes.clear();
        self.deferred.clear();
        self.dynamic = DynamicState::default();
        self.push_constants.clear();
    }
}
