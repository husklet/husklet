//! The dd-shim-vk recording registry: the process-global state that turns a stream of `vk*` calls
//! into a `hl_gpu::ir` command stream (the shared contract `dd-shim-common` re-exports).
//!
//! ## Execution model (mirrors dd-shim-cuda's embedded-backend seam)
//! Every resource-creating `vk*` call (`vkCreateBuffer`, `vkCreateShaderModule`, `vkCreate*Pipelines`,
//! …) allocates an IR object id and appends the matching [`Cmd`] to [`VkState::ir_log`]; every
//! `vkCmd*` records an [`Enc`] into the target command buffer; `vkQueueSubmit` wraps the recorded
//! encoder in a [`Cmd::Submit`]. The accumulated IR is the exact stream the host SPIR-V→Metal executor
//! (`dd-gpu-wgpu`, proven in `spirv_compute.rs`/`spirv_triangle.rs`) replays — in production shipped
//! over `hl_shim::transport` to `$DD_GPU_EXEC`; in the validation tests drained via
//! [`take_ir`] and replayed on a real-Metal `WgpuBackend` (the test playing the host exec service,
//! exactly as dd-shim-cuda's tests replay on an embedded backend).
//!
//! The Vulkan→IR mapping is ported from MoltenVK (the canonical Vulkan-over-Metal driver); each
//! entry-point module cites the specific `MVK*` source it mirrors.

use hl_shim::ir::*;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

/// The reserved IR texture id the host executor uses for the presented surface (dd-display's Metal
/// executor `set_render_target(1, <current IOSurface>)` per frame). Every swapchain image's render
/// pass targets this id so the render lands in the IOSurface. IR texture ids and buffer ids are
/// separate namespaces host-side, so this never collides with a buffer id 1.
pub const PRESENT_IR_ID: u32 = 1;

// ---- per-resource records ------------------------------------------------------------------------

pub struct BufferRec {
    pub ir_id: u32,
    pub size: u64,
    pub usage: u32, // dd-gpu buffer_usage bits
    pub bound_mem: Option<u64>,
    pub bound_offset: u64,
}

pub struct MemRec {
    pub data: Vec<u8>,
    /// The `VkMemoryAllocateInfo::allocationSize` this was created with (the valid bind/map range).
    pub size: u64,
    /// The `VkMemoryAllocateInfo::memoryTypeIndex` (our single unified type is 0). Retained so bind can
    /// validate a buffer/image's `memoryTypeBits` against it (MoltenVK `MVKDeviceMemory` records this).
    pub memory_type_index: u32,
    pub bound_buffer: Option<u64>,
    /// Currently mapped (vkMapMemory without vkUnmapMemory). HOST_COHERENT memory is often mapped
    /// persistently and written every frame WITHOUT an unmap (vkcube's rotating MVP uniform buffer),
    /// so such buffers must be re-uploaded to the host each submit — see `flush_mapped` at vkQueueSubmit.
    pub mapped: bool,
    /// The currently-mapped byte range `[offset, offset+len)` (validated in `vkMapMemory`); `None` when
    /// unmapped. Ported from `MVKDeviceMemory::_mappedRange`.
    pub mapped_range: Option<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShaderType {
    Bool,
    Int { width: u32, signed: bool },
    Float { width: u32 },
    Vector { component: Box<ShaderType>, count: u32 },
    Matrix { column: Box<ShaderType>, count: u32 },
    Other,
}

#[derive(Clone, Debug)]
pub struct ShaderEntry {
    pub stage: u32,
    pub inputs: HashMap<u32, ShaderType>,
    pub outputs: HashMap<u32, ShaderType>,
}

#[derive(Clone, Debug)]
pub struct SpecConstantRec {
    pub ty: ShaderType,
}

pub struct ShaderRec {
    pub ir_id: u32,
    pub spirv: Vec<u32>,
    pub entries: HashMap<String, ShaderEntry>,
    pub descriptors: Vec<(u32, u32)>,
    /// The `VkDescriptorType` (raw) inferred from each `(set, binding)` resource's SPIR-V type +
    /// storage class (sampler / sampled|storage image / (texel) buffer / input attachment). Absent for
    /// a binding whose type could not be classified (then no type check is enforced). Consumed by
    /// `pipeline::layout_supports_shader` to reject a descriptor-set-layout type mismatch.
    pub descriptor_types: HashMap<(u32, u32), i32>,
    pub push_constant: bool,
    pub spec_constants: HashMap<u32, SpecConstantRec>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum PipeKind {
    Compute,
    Graphics,
}

pub struct PipelineRec {
    pub ir_id: u32,
    pub kind: PipeKind,
}

pub struct PipelineLayoutRec {
    pub set_layouts: Vec<u64>,
    pub push_ranges: Vec<(u32, u32, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageSubresourceState {
    pub layout: i32,
    pub last_access: u32,
    pub last_stage: u32,
    pub owner_queue_family: u32,
}

#[derive(Clone, Debug)]
pub struct ImageRec {
    pub ir_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub is_render_target: bool,
    pub mip_levels: u32,
    pub array_layers: u32,
    pub aspect_mask: u32,
    pub usage: u32,
    pub sample_count: u32,
    pub subresources: HashMap<(u32, u32, u32), ImageSubresourceState>,
    /// The `VkDeviceMemory` this image is bound to (`vkBindImageMemory`), or `None` if unbound. Image
    /// binding is NOT a no-op: it validates and records ownership (MoltenVK `MVKImage::bindDeviceMemory`).
    pub bound_mem: Option<u64>,
}

#[derive(Clone, Copy)]
pub struct ImageViewRec {
    pub image: u64,
    pub range: ImageSubresourceRange,
}

#[derive(Clone, Copy, Debug)]
pub struct ImageSubresourceRange {
    pub aspect_mask: u32,
    pub base_mip_level: u32,
    pub level_count: u32,
    pub base_array_layer: u32,
    pub layer_count: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct ImageTransition {
    pub image: u64,
    pub range: ImageSubresourceRange,
    pub old_layout: i32,
    pub new_layout: i32,
    pub src_access: u32,
    pub dst_access: u32,
    pub src_stage: u32,
    pub dst_stage: u32,
    pub src_queue_family: u32,
    pub dst_queue_family: u32,
}

#[derive(Clone, Debug)]
pub enum ImageEvent {
    Barriers(Vec<ImageTransition>),
    RenderBegin {
        image: u64,
        range: ImageSubresourceRange,
        initial_layout: i32,
        subpass_layout: i32,
    },
    RenderEnd {
        image: u64,
        range: ImageSubresourceRange,
        subpass_layout: i32,
        final_layout: i32,
    },
    TransferUse {
        image: u64,
        range: ImageSubresourceRange,
        required_layout: i32,
        access: u32,
    },
}

/// One immutable binding of a `VkDescriptorSetLayout` (MoltenVK `MVKDescriptorSetLayout` binding
/// record): the typed per-binding descriptor description a set is allocated + validated against.
pub struct DescriptorLayoutBinding {
    pub binding: u32,
    pub descriptor_type: i32, // VkDescriptorType
    pub descriptor_count: u32, // array size (0 disables the binding)
    pub stage_flags: u32, // VkShaderStageFlags
    pub immutable_samplers: Vec<u64>, // VkSampler handles baked into the layout, if any
    /// `VkDescriptorBindingFlags` (VK_EXT_descriptor_indexing): UPDATE_AFTER_BIND(1) / UPDATE_UNUSED_WHILE_
    /// PENDING(2) / PARTIALLY_BOUND(4) / VARIABLE_DESCRIPTOR_COUNT(8). Parsed from the layout's pNext.
    pub binding_flags: u32,
}

/// A `VkDescriptorSetLayout` (MoltenVK `MVKDescriptorSetLayout`): its immutable binding table.
pub struct DescriptorSetLayoutRec {
    pub bindings: Vec<DescriptorLayoutBinding>,
}

impl DescriptorSetLayoutRec {
    /// The bindings that are dynamic buffers (UNIFORM/STORAGE_BUFFER_DYNAMIC), in ascending binding
    /// order — the order `vkCmdBindDescriptorSets`'s `pDynamicOffsets` are consumed in.
    pub fn dynamic_bindings(&self) -> Vec<u32> {
        // VkDescriptorType: 8 = UNIFORM_BUFFER_DYNAMIC, 9 = STORAGE_BUFFER_DYNAMIC.
        let mut v: Vec<u32> = self
            .bindings
            .iter()
            .filter(|b| b.descriptor_type == 8 || b.descriptor_type == 9)
            .map(|b| b.binding)
            .collect();
        v.sort_unstable();
        v
    }
}

/// A `VkDescriptorPool` (MoltenVK `MVKDescriptorPool`): its capacity, free policy and live set count.
pub struct DescriptorPoolRec {
    /// `VkDescriptorPoolCreateInfo::maxSets`. `0` means the app declared no positive limit (the ash
    /// `default()` used by the bring-up tests) — we don't fabricate one, so allocation is not quota-capped.
    pub max_sets: u32,
    /// Sets currently allocated from this pool (checked against `max_sets`).
    pub allocated: u32,
    /// `VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT`: whether `vkFreeDescriptorSets` may return
    /// individual sets (else free is a no-op per the spec).
    pub free_descriptor_set: bool,
    /// `VK_DESCRIPTOR_POOL_CREATE_UPDATE_AFTER_BIND_BIT` (VK_EXT_descriptor_indexing): the pool may allocate
    /// UPDATE_AFTER_BIND sets. Accepted + recorded (bindless creation succeeds).
    pub update_after_bind: bool,
}

/// A descriptor set: the layout + owning pool it was allocated with, and the `binding -> (buffer,
/// offset, range)` table `vkUpdateDescriptorSets` writes. Resolved to an IR bind group at
/// `vkCmdBindDescriptorSets` (dynamic offsets applied there). Image/sampler/texel writes are retained
/// (`image_writes`/`texel_writes`) so nothing is silently dropped; their IR lowering is a later increment.
pub struct DsetRec {
    pub set: u32,
    pub layout: u64,
    pub pool: u64,
    pub buffers: HashMap<u32, (u64, u64, u64)>,
    /// binding -> [(imageView, sampler, imageLayout)] image/sampler descriptor writes (retained).
    pub image_writes: HashMap<u32, Vec<(u64, u64, i32)>>,
    /// binding -> [bufferView] texel-buffer descriptor writes (retained).
    pub texel_writes: HashMap<u32, Vec<u64>>,
    /// The runtime array size of the layout's VARIABLE_DESCRIPTOR_COUNT binding (VK_EXT_descriptor_indexing),
    /// from `VkDescriptorSetVariableDescriptorCountAllocateInfo`; `None` when the set has no variable binding.
    pub variable_count: Option<u32>,
}

/// A render pass: the single color attachment's format + load/clear/store (the subset bring-up needs).
pub struct RenderPassRec {
    pub color_format: TextureFormat,
    pub color_load_clear: bool,
    pub clear: [f32; 4],
    pub color_store: bool,
    pub initial_layout: i32,
    pub subpass_layout: i32,
    pub final_layout: i32,
}

pub struct FramebufferRec {
    pub width: u32,
    pub height: u32,
    pub color_view: Option<u64>,
}

/// A `VkSurfaceKHR` (wayland): the app-owned `wl_display`/`wl_surface` pointers it wraps (kept as
/// `usize` so the state stays `Send`; the present path speaks wayland on the app's connection).
pub struct SurfaceRec {
    pub wl_display: usize,
    pub wl_surface: usize,
}

/// One presentable swapchain image: its `VkImage` handle + IR texture id, and the host-forward
/// present surface (the `renderd` IOSurface/dma-buf the host Metal executor renders into; `fd == -1`
/// in the offscreen fallback used off-guest / in tests).
pub struct SwapImage {
    pub image: u64,
    pub ir_id: u32,
    pub surface: hl_shim::transport::Surface,
    pub state: SwapImageState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapImageState {
    Available,
    Acquired,
    Presenting,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapchainState {
    Active,
    Retired,
    Lost,
}

/// A `VkSwapchainKHR`: its presentable images, geometry, ownership cursor and lifecycle.
pub struct SwapchainRec {
    pub surface: u64,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub images: Vec<SwapImage>,
    pub next: usize,
    pub state: SwapchainState,
}

/// The Vulkan command-buffer lifecycle state (spec §6 "Command Buffer Lifecycle"). Ported from
/// MoltenVK's `MVKCommandBuffer` flag model (`_canAcceptCommands` / `_isReusable` / `_wasExecuted` /
/// `_isExecutingNonConcurrently`) into the explicit enum the audit (§9.2) recommends:
///
/// * `Initial`    — freshly allocated or reset; can only be begun.
/// * `Recording`  — inside `vkBeginCommandBuffer`; accepts `vkCmd*`.
/// * `Executable` — `vkEndCommandBuffer` succeeded; can be submitted (and re-submitted if reusable).
/// * `Pending`    — submitted to a queue and not yet completed; must NOT be touched (external sync).
/// * `Invalid`    — a one-time-submit buffer that has executed, or a recording error; must be reset.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CommandBufferState {
    #[default]
    Initial,
    Recording,
    Executable,
    Pending,
    Invalid,
}

/// Command-buffer recording state + its lifecycle state machine.
#[derive(Default)]
pub struct CmdBufRec {
    /// The lifecycle state (Initial/Recording/Executable/Pending/Invalid).
    pub state: CommandBufferState,
    /// `VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT` was set at begin: the buffer becomes `Invalid`
    /// after one execution (MoltenVK `_isReusable = !ONE_TIME_SUBMIT`).
    pub one_time_submit: bool,
    /// `VK_COMMAND_BUFFER_USAGE_SIMULTANEOUS_USE_BIT` was set at begin (MoltenVK
    /// `_supportsConcurrentExecution`): the buffer may be resubmitted while still `Pending`.
    pub simultaneous_use: bool,
    /// Whether the buffer has been executed at least once (MoltenVK `_wasExecuted`).
    pub was_executed: bool,
    pub enc: Vec<Enc>,
    pub bound_pipeline: Option<u32>,
    pub bound_pipeline_kind: Option<u8>, // 0 = graphics, 1 = compute
    pub pending_bind_groups: Vec<(u32, u32)>, // (set index, bind-group IR id)
    pub in_render_pass: bool,
    pub pipeline_set_in_pass: bool,
    pub image_events: Vec<ImageEvent>,
    pub active_render_image: Option<(u64, ImageSubresourceRange, i32, i32)>,
    /// Device query/event ops (`vkCmdBeginQuery`/`EndQuery`/`ResetQueryPool`/`WriteTimestamp`,
    /// `vkCmdSetEvent`/`ResetEvent`) applied to the global state at submit completion (synchronous replay).
    pub deferred: Vec<DeferredOp>,
    /// `vkCmdUpdateBuffer` / `vkCmdFillBuffer` payloads, emitted as `Cmd::WriteBuffer` immediately before
    /// this command buffer's `Cmd::Submit` (the same start-of-submit upload model as persistently-mapped
    /// coherent memory): `(ir buffer id, dst offset, bytes)`.
    pub buffer_writes: Vec<(u32, u64, Vec<u8>)>,
    /// The active occlusion/pipeline-statistics query `(pool, query)` opened by `vkCmdBeginQuery`, if any
    /// (a command buffer may have at most one active query of a given type at a time — spec §17.4).
    pub active_query: Option<(u64, u32)>,
    /// Recorded dynamic pipeline state (`vkCmdSetLineWidth`/`SetDepthBias`/… — MoltenVK's
    /// `MVKCommandEncoderState`). Observable guest-side; the IR draw state does not yet consume it.
    pub dynamic: DynamicState,
    /// Recorded push-constant bytes (`vkCmdPushConstants`), written at each range's offset. Retained
    /// (validated against the pipeline layout ranges); the IR does not yet carry a push-constant block.
    pub push_constants: Vec<u8>,
}

/// The subset of dynamic pipeline state the `vkCmdSet*` commands record (MoltenVK `MVKCommandEncoderState`).
/// Captured verbatim so recording is observable; lowering into the IR draw state is a later increment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynamicState {
    pub line_width: f32,
    /// `(constantFactor, clamp, slopeFactor)`.
    pub depth_bias: (f32, f32, f32),
    /// `(minDepthBounds, maxDepthBounds)`.
    pub depth_bounds: (f32, f32),
    pub blend_constants: [f32; 4],
    /// `(front, back)` stencil compare masks.
    pub stencil_compare_mask: (u32, u32),
    /// `(front, back)` stencil write masks.
    pub stencil_write_mask: (u32, u32),
    /// `(front, back)` stencil reference values.
    pub stencil_reference: (u32, u32),
    // ---- Vulkan 1.3 extended dynamic state (EDS1 + EDS2), recorded verbatim (MVKCommandEncoderState) ----
    pub cull_mode: u32,
    pub front_face: i32,
    pub primitive_topology: i32,
    pub primitive_restart_enable: bool,
    pub rasterizer_discard_enable: bool,
    pub depth_test_enable: bool,
    pub depth_write_enable: bool,
    pub depth_compare_op: i32,
    pub depth_bounds_test_enable: bool,
    pub depth_bias_enable: bool,
    pub stencil_test_enable: bool,
    /// `(faceMask, failOp, passOp, depthFailOp, compareOp)` of the last `vkCmdSetStencilOp`.
    pub stencil_op: (u32, i32, i32, i32, i32),
    /// The last `vkCmdSetViewportWithCount` / `vkCmdSetScissorWithCount` count.
    pub viewport_count: u32,
    pub scissor_count: u32,
    /// `vkCmdSetLineStipple` (Vulkan 1.4): `(lineStippleFactor, lineStipplePattern)`.
    pub line_stipple: (u32, u16),
}

impl Default for DynamicState {
    fn default() -> Self {
        DynamicState {
            line_width: 1.0,
            depth_bias: (0.0, 0.0, 0.0),
            depth_bounds: (0.0, 1.0),
            blend_constants: [0.0; 4],
            stencil_compare_mask: (u32::MAX, u32::MAX),
            stencil_write_mask: (u32::MAX, u32::MAX),
            stencil_reference: (0, 0),
            cull_mode: 0,
            front_face: 0,
            primitive_topology: 3, // TRIANGLE_LIST
            primitive_restart_enable: false,
            rasterizer_discard_enable: false,
            depth_test_enable: false,
            depth_write_enable: false,
            depth_compare_op: 1, // LESS
            depth_bounds_test_enable: false,
            depth_bias_enable: false,
            stencil_test_enable: false,
            stencil_op: (0, 0, 0, 0, 0),
            viewport_count: 0,
            scissor_count: 0,
            line_stipple: (1, 0xFFFF),
        }
    }
}

impl CmdBufRec {
    /// A freshly-allocated command buffer (MoltenVK: pool-owned, `Initial`).
    pub fn initial() -> Self {
        CmdBufRec::default()
    }

    /// Clear the recorded contents back to a just-begun state, preserving the usage flags parsed at
    /// begin. Ported from `MVKCommandBuffer::reset` (called at the start of `begin`).
    pub fn reset_recording(&mut self) {
        self.enc.clear();
        self.bound_pipeline = None;
        self.bound_pipeline_kind = None;
        self.pending_bind_groups.clear();
        self.in_render_pass = false;
        self.pipeline_set_in_pass = false;
        self.image_events.clear();
        self.active_render_image = None;
        self.deferred.clear();
        self.buffer_writes.clear();
        self.active_query = None;
        self.dynamic = DynamicState::default();
        self.push_constants.clear();
    }
}

/// A `VkFence`: its guest-side signaled state. The host Metal executor renders synchronously and does
/// not model fences, so the fence is a guest-side state machine (MoltenVK `MVKFence`): unsignaled at
/// creation (unless `VK_FENCE_CREATE_SIGNALED_BIT`), signaled when the submission it guards completes,
/// reset by `vkResetFences`.
pub struct FenceRec {
    pub ir_id: u32,
    pub signaled: bool,
}

/// A binary `VkSemaphore`: its guest-side signaled state (MoltenVK `MVKSemaphore` binary model). A
/// submit's wait-semaphores must be signaled and are consumed (reset); its signal-semaphores are
/// signaled on completion.
#[derive(Default)]
pub struct SemaphoreRec {
    pub signaled: bool,
    /// `VK_SEMAPHORE_TYPE_TIMELINE` (VK_KHR_timeline_semaphore): a monotonically increasing counter the
    /// host reads (`vkGetSemaphoreCounterValue`), waits on (`vkWaitSemaphores`), and signals
    /// (`vkSignalSemaphore` / a queue submit's `VkTimelineSemaphoreSubmitInfo` values).
    pub timeline: bool,
    /// The timeline counter value (0 for a binary semaphore).
    pub counter: u64,
}

/// A `VkEvent` (MoltenVK `MVKEvent`): a guest-side boolean the host and device set/reset/poll. Created
/// unsignaled. Host ops (`vkSetEvent`/`vkResetEvent`/`vkGetEventStatus`) mutate/read it directly;
/// device ops (`vkCmdSetEvent`/`vkCmdResetEvent`) are recorded and applied at submit completion (our
/// host replay is synchronous, so a device-set event is observably signaled once the submit returns).
#[derive(Default)]
pub struct EventRec {
    pub signaled: bool,
}

/// A `VkSampler` (MoltenVK `MVKSampler`): lowered to a dd-gpu IR sampler (`Cmd::CreateSampler`). The
/// filter/address state is translated at creation; the handle carries the IR id for descriptor lowering.
pub struct SamplerRec {
    pub ir_id: u32,
}

/// A `VkBufferView` (MoltenVK `MVKBufferView`): a typed window `[offset, offset+range)` onto a buffer,
/// used by texel-buffer descriptors. Retained (validated) for the texel-descriptor IR increment.
#[derive(Clone, Copy)]
pub struct BufferViewRec {
    pub buffer: u64,
    pub format: i32,
    pub offset: u64,
    pub range: u64,
}

/// One `[first, first+count)` result slot of a query. `available` gates `vkGetQueryPoolResults`
/// (unavailable → `VK_NOT_READY` unless `WAIT`); `value` is the (bounded) synchronous result.
#[derive(Clone, Copy, Default)]
pub struct QueryResult {
    pub available: bool,
    pub value: u64,
}

/// A `VkQueryPool` (MoltenVK `MVKQueryPool` / `MVKTimestampQueryPool` / `MVKOcclusionQueryPool`): a
/// fixed array of typed query slots. Occlusion/pipeline-statistics results are a bounded synchronous
/// model (no real GPU sample counts); timestamps are a host-monotonic serial.
pub struct QueryPoolRec {
    pub query_type: i32, // VkQueryType: 0 = OCCLUSION, 1 = PIPELINE_STATISTICS, 2 = TIMESTAMP
    pub count: u32,
    pub results: Vec<QueryResult>,
}

/// A `VkPipelineCache` (MoltenVK `MVKPipelineCache`): an opaque serializable blob. We model it as an
/// owned byte buffer with the spec-defined `VkPipelineCacheHeaderVersionOne` header so
/// `vkGetPipelineCacheData` round-trips and `vkMergePipelineCaches` is observable.
#[derive(Default)]
pub struct PipelineCacheRec {
    pub data: Vec<u8>,
}

/// One entry of a `VkDescriptorUpdateTemplate` (MoltenVK `MVKDescriptorUpdateTemplate`): where in the
/// pushed data blob each descriptor lives and which `(binding, arrayElement)` it targets.
#[derive(Clone, Copy)]
pub struct DescriptorTemplateEntry {
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub descriptor_count: u32,
    pub descriptor_type: i32,
    pub offset: usize,
    pub stride: usize,
}

/// A `VkDescriptorUpdateTemplate` (Vulkan 1.1): the immutable entry table `vkUpdateDescriptorSetWithTemplate`
/// walks to read descriptors out of the app's data blob at fixed offsets/strides.
pub struct DescriptorUpdateTemplateRec {
    pub entries: Vec<DescriptorTemplateEntry>,
}

/// A `VkSamplerYcbcrConversion` (Vulkan 1.1 / MoltenVK `MVKSamplerYcbcrConversion`): an opaque handle
/// wrapping the requested YCbCr model/range/swizzle. We do not materialize YCbCr *formats*, so the
/// object exists (lifetime is observable) but only the identity/pass-through case is meaningful.
pub struct SamplerYcbcrConversionRec {
    pub format: i32,
}

/// A device-side query/event/buffer-write op recorded into a command buffer and applied at
/// `vkQueueSubmit` completion (our host replay is synchronous). Kept out of the `Vec<Enc>` encoder so
/// the shipped `Cmd::Submit` byte stream for an existing draw/dispatch input is unchanged.
#[derive(Clone, Debug)]
pub enum DeferredOp {
    /// `vkCmdResetQueryPool` — clear `[first, first+count)` to unavailable/zero.
    QueryReset { pool: u64, first: u32, count: u32 },
    /// `vkCmdEndQuery` (occlusion / pipeline-statistics) — mark the slot available with a bounded value.
    QueryEnd { pool: u64, query: u32, value: u64 },
    /// `vkCmdWriteTimestamp` — mark the slot available with a host-monotonic timestamp serial.
    QueryTimestamp { pool: u64, query: u32 },
    /// `vkCmdSetEvent` / `vkCmdResetEvent` — set/clear an event on completion.
    Event { event: u64, set: bool },
    /// `vkCmdCopyQueryPoolResults` — on completion write the pool's `[first, first+count)` results into
    /// the destination buffer (IR `WriteBuffer`) with the requested element size, stride and flags.
    CopyResults {
        pool: u64,
        first: u32,
        count: u32,
        dst_ir: u32,
        dst_offset: u64,
        dst_size: u64,
        stride: u64,
        wide: bool,        // VK_QUERY_RESULT_64_BIT
        with_availability: bool, // VK_QUERY_RESULT_WITH_AVAILABILITY_BIT
    },
}

// ---- the global state ----------------------------------------------------------------------------

#[derive(Default)]
pub struct VkState {
    next_ir: u32,
    next_handle: u64,
    pub ir_log: Vec<Cmd>,
    pub buffers: HashMap<u64, BufferRec>,
    pub memories: HashMap<u64, MemRec>,
    pub shaders: HashMap<u64, ShaderRec>,
    pub pipelines: HashMap<u64, PipelineRec>,
    pub pipeline_layouts: HashMap<u64, PipelineLayoutRec>,
    pub images: HashMap<u64, ImageRec>,
    pub image_views: HashMap<u64, ImageViewRec>,
    pub dsets: HashMap<u64, DsetRec>,
    pub descriptor_set_layouts: HashMap<u64, DescriptorSetLayoutRec>,
    pub descriptor_pools: HashMap<u64, DescriptorPoolRec>,
    pub render_passes: HashMap<u64, RenderPassRec>,
    pub framebuffers: HashMap<u64, FramebufferRec>,
    pub cmdbufs: HashMap<usize, CmdBufRec>,
    pub fences: HashMap<u64, FenceRec>, // fence handle -> guest-side fence state
    pub semaphores: HashMap<u64, SemaphoreRec>, // binary semaphore handle -> signaled state
    pub events: HashMap<u64, EventRec>, // event handle -> guest-side signaled state
    pub samplers: HashMap<u64, SamplerRec>, // sampler handle -> IR sampler id
    pub buffer_views: HashMap<u64, BufferViewRec>, // buffer-view handle -> typed buffer window
    pub query_pools: HashMap<u64, QueryPoolRec>, // query-pool handle -> typed slot array
    pub pipeline_caches: HashMap<u64, PipelineCacheRec>, // pipeline-cache handle -> serialized blob
    pub descriptor_update_templates: HashMap<u64, DescriptorUpdateTemplateRec>, // 1.1 update templates
    pub ycbcr_conversions: HashMap<u64, SamplerYcbcrConversionRec>, // 1.1 sampler ycbcr conversions
    /// Vulkan 1.3 private-data slots (handle set) + the per-`(slot, objectType, objectHandle)` u64 payload.
    pub private_data_slots: std::collections::HashSet<u64>,
    pub private_data: HashMap<(u64, i32, u64), u64>,
    pub surfaces: HashMap<u64, SurfaceRec>,
    pub swapchains: HashMap<u64, SwapchainRec>,
    /// Lazily-opened host GPU-exec channel (only when `$DD_GPU_EXEC` is set — the live guest path).
    pub exec: Option<hl_shim::transport::ExecConn>,
    /// Cursor into `ir_log` up to which frames have been shipped to the host at `vkQueuePresentKHR`.
    pub present_flushed: usize,
}

impl VkState {
    /// Allocate a fresh IR object id (buffer/shader/pipeline/bind-group/texture/fence — one shared
    /// namespace; the host backend keys per-kind so cross-kind overlap is irrelevant).
    pub fn alloc_ir(&mut self) -> u32 {
        self.next_ir += 1;
        self.next_ir
    }

    /// Allocate a fresh non-dispatchable Vulkan handle (a monotonic u64, never 0 == VK_NULL_HANDLE).
    pub fn alloc_handle(&mut self) -> u64 {
        self.next_handle += 1;
        0x1000_0000 + self.next_handle
    }

    /// Append a resource-level IR command to the log.
    pub fn record(&mut self, cmd: Cmd) {
        self.ir_log.push(cmd);
    }

    /// Borrow a command buffer's record ONLY if it is currently `Recording` — the Vulkan rule that a
    /// `vkCmd*` outside an active `vkBegin/End` recording is invalid and must not mutate the buffer
    /// (MoltenVK `MVKCommandBuffer::addCommand` rejects commands unless `_canAcceptCommands`). Returns
    /// `None` (command dropped) otherwise, so out-of-recording commands cannot corrupt the stream.
    pub fn recording_mut(&mut self, key: usize) -> Option<&mut CmdBufRec> {
        match self.cmdbufs.get_mut(&key) {
            Some(cb) if cb.state == CommandBufferState::Recording => Some(cb),
            _ => None,
        }
    }

    /// Re-upload every currently-mapped, buffer-bound memory to the host as a `WriteBuffer` — the
    /// per-frame flush of persistently-mapped HOST_COHERENT buffers (e.g. vkcube's rotating MVP UBO),
    /// which are written each frame with NO vkUnmapMemory. Called at `vkQueueSubmit`.
    pub fn flush_mapped(&mut self) {
        let uploads: Vec<(u32, Vec<u8>)> = self
            .memories
            .values()
            .filter(|m| m.mapped)
            .filter_map(|m| {
                let bh = m.bound_buffer?;
                let b = self.buffers.get(&bh)?;
                let n = (b.size as usize).min(m.data.len());
                Some((b.ir_id, m.data[..n].to_vec()))
            })
            .collect();
        for (id, data) in uploads {
            self.ir_log.push(Cmd::WriteBuffer { id, offset: 0, data });
        }
    }
}

fn cell() -> &'static Mutex<VkState> {
    static S: OnceLock<Mutex<VkState>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(VkState::default()))
}

/// Lock the global recording state. All `vk*` bodies funnel through this.
pub fn lock() -> MutexGuard<'static, VkState> {
    cell().lock().unwrap_or_else(|e| e.into_inner())
}

/// Drain and return the recorded IR command stream (the test/host-exec seam). Also clears the
/// per-command-buffer recordings so a subsequent run starts clean.
pub fn take_ir() -> Vec<Cmd> {
    let mut s = lock();
    let ir = std::mem::take(&mut s.ir_log);
    s.cmdbufs.clear();
    ir
}

/// Crate-wide test serialization lock. The `vk*` bodies funnel through one process-global [`VkState`]
/// (`ir_log`, per-object maps), so tests that mutate or assert on it must not run concurrently — in
/// particular the WSI present test asserts `present_flushed == ir_log.len()`, which any concurrent
/// `vkQueueSubmit`/`vkCreateShaderModule` would break. All ir-log-mutating test modules take this one
/// lock (the previous per-module locks did not serialize across modules).
#[cfg(test)]
pub static TEST_SERIAL: Mutex<()> = Mutex::new(());

/// Reset ALL recording state (tests run serially against this process-global).
pub fn reset() {
    *lock() = VkState::default();
}

/// DD_SHIM_DEBUG entry-point tracer (localizes which real body an app is in). Cheap when off.
#[inline]
pub fn trace(name: &str) {
    if std::env::var_os("DD_SHIM_DEBUG").is_some() {
        eprintln!("[dd-shim-vk] -> {name}");
    }
}
