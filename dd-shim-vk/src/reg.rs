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
    pub bound_buffer: Option<u64>,
    /// Currently mapped (vkMapMemory without vkUnmapMemory). HOST_COHERENT memory is often mapped
    /// persistently and written every frame WITHOUT an unmap (vkcube's rotating MVP uniform buffer),
    /// so such buffers must be re-uploaded to the host each submit — see `flush_mapped` at vkQueueSubmit.
    pub mapped: bool,
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

pub struct ImageRec {
    pub ir_id: u32,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub is_render_target: bool,
}

pub struct ImageViewRec {
    pub image: u64,
}

/// A descriptor set: `set` number + `binding -> (buffer handle, offset, range)` from
/// `vkUpdateDescriptorSets`. Resolved to an IR bind group at `vkCmdBindDescriptorSets`.
pub struct DsetRec {
    pub set: u32,
    pub buffers: HashMap<u32, (u64, u64, u64)>,
}

/// A render pass: the single color attachment's format + load/clear/store (the subset bring-up needs).
pub struct RenderPassRec {
    pub color_format: TextureFormat,
    pub color_load_clear: bool,
    pub clear: [f32; 4],
    pub color_store: bool,
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
}

/// A `VkSwapchainKHR`: its presentable images + geometry + the round-robin acquire cursor.
pub struct SwapchainRec {
    pub surface: u64,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub images: Vec<SwapImage>,
    pub next: usize,
}

/// Command-buffer recording state.
#[derive(Default)]
pub struct CmdBufRec {
    pub enc: Vec<Enc>,
    pub bound_pipeline: Option<u32>,
    pub bound_pipeline_kind: Option<u8>, // 0 = graphics, 1 = compute
    pub pending_bind_groups: Vec<(u32, u32)>, // (set index, bind-group IR id)
    pub in_render_pass: bool,
    pub pipeline_set_in_pass: bool,
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
    pub render_passes: HashMap<u64, RenderPassRec>,
    pub framebuffers: HashMap<u64, FramebufferRec>,
    pub cmdbufs: HashMap<usize, CmdBufRec>,
    pub fences: HashMap<u64, u32>, // fence handle -> IR fence id
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
