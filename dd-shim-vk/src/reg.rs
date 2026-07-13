//! The dd-shim-vk recording registry: the process-global state that turns a stream of `vk*` calls
//! into a `dd_gpu::ir` command stream (the shared contract `dd-shim-common` re-exports).
//!
//! ## Execution model (mirrors dd-shim-cuda's embedded-backend seam)
//! Every resource-creating `vk*` call (`vkCreateBuffer`, `vkCreateShaderModule`, `vkCreate*Pipelines`,
//! …) allocates an IR object id and appends the matching [`Cmd`] to [`VkState::ir_log`]; every
//! `vkCmd*` records an [`Enc`] into the target command buffer; `vkQueueSubmit` wraps the recorded
//! encoder in a [`Cmd::Submit`]. The accumulated IR is the exact stream the host SPIR-V→Metal executor
//! (`dd-gpu-wgpu`, proven in `spirv_compute.rs`/`spirv_triangle.rs`) replays — in production shipped
//! over `dd_shim_common::transport` to `$DD_GPU_EXEC`; in the validation tests drained via
//! [`take_ir`] and replayed on a real-Metal `WgpuBackend` (the test playing the host exec service,
//! exactly as dd-shim-cuda's tests replay on an embedded backend).
//!
//! The Vulkan→IR mapping is ported from MoltenVK (the canonical Vulkan-over-Metal driver); each
//! entry-point module cites the specific `MVK*` source it mirrors.

use dd_shim_common::ir::*;
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

pub struct ShaderRec {
    pub ir_id: u32,
    pub spirv: Vec<u32>,
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
    pub surface: dd_shim_common::transport::Surface,
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
    pub surfaces: HashMap<u64, SurfaceRec>,
    pub swapchains: HashMap<u64, SwapchainRec>,
    /// Lazily-opened host GPU-exec channel (only when `$DD_GPU_EXEC` is set — the live guest path).
    pub exec: Option<dd_shim_common::transport::ExecConn>,
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
