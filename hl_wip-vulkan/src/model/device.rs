//! [`Device`] — the per-`VkDevice` aggregate the Vulkan driver operates on.
//!
//! It owns the whole Vulkan object model for one logical device: the physical-device descriptor, the
//! per-kind handle tables (buffers, memories, images, samplers, shaders, pipelines, descriptor sets,
//! command buffers, fences, WSI objects), its lone queue, and every id counter. Ported from
//! `hl-shim-vk/src/reg.rs` (`VkState`) — the id-minting semantics (a shared monotonic IR-id counter
//! across every resource kind, monotonic non-dispatchable handles) are kept so the emitted IR is
//! identical.
//!
//! The device builds NO `Cmd`s and submits nothing; it only mints ids and records bookkeeping. The
//! [`crate::service`] layer calls these methods, then submits the lowered commands through a
//! [`hl_gpu::CommandSink`].

use super::command::CmdBufRec;
use super::descriptor::{DescriptorPoolRec, DsetRec, SetLayoutRec};
use super::instance::PhysicalDeviceDesc;
use super::memory::{BufferRec, ImageRec, MemRec, SamplerRec};
use super::pipeline::{PipelineLayoutRec, PipelineRec, ShaderRec};
use super::queue::{FenceRec, Queue, SurfaceRec, SwapchainRec};
use crate::*;
use std::collections::HashMap;

/// The per-device aggregate: the object model + the id counters the lowering mutates.
pub struct Device {
    pub physical_device: PhysicalDeviceDesc,
    pub queue: Queue,

    // ---- handle tables (non-dispatchable handle -> record) ----
    pub buffers: HashMap<VkBuffer, BufferRec>,
    pub memories: HashMap<VkDeviceMemory, MemRec>,
    pub images: HashMap<VkImage, ImageRec>,
    pub samplers: HashMap<VkSampler, SamplerRec>,
    pub shaders: HashMap<VkShaderModule, ShaderRec>,
    pub pipelines: HashMap<VkPipeline, PipelineRec>,
    pub pipeline_layouts: HashMap<VkPipelineLayout, PipelineLayoutRec>,
    pub set_layouts: HashMap<VkDescriptorSetLayout, SetLayoutRec>,
    pub descriptor_pools: HashMap<VkDescriptorPool, DescriptorPoolRec>,
    pub descriptor_sets: HashMap<VkDescriptorSet, DsetRec>,
    pub command_buffers: HashMap<VkCommandBuffer, CmdBufRec>,
    pub fences: HashMap<VkFence, FenceRec>,
    pub surfaces: HashMap<VkSurfaceKHR, SurfaceRec>,
    pub swapchains: HashMap<VkSwapchainKHR, SwapchainRec>,

    // ---- id counters (monotonic) ----
    /// hl-GPU IR object-id counter — one shared namespace across every resource kind (the host backend
    /// keys per-kind, so cross-kind overlap is irrelevant). Ported from `VkState::next_ir`.
    next_ir: u32,
    /// Non-dispatchable Vulkan-handle counter (a monotonic `u64`, never 0 == `VK_NULL_HANDLE`).
    next_handle: u64,
    /// Timeline value for the next fence signal (monotonic across the device).
    fence_value: u64,
}

impl Device {
    /// Create a fresh logical device over `physical_device`, with one primary queue and empty tables.
    pub fn new(physical_device: PhysicalDeviceDesc) -> Self {
        Self {
            physical_device,
            queue: Queue::primary(),
            buffers: HashMap::new(),
            memories: HashMap::new(),
            images: HashMap::new(),
            samplers: HashMap::new(),
            shaders: HashMap::new(),
            pipelines: HashMap::new(),
            pipeline_layouts: HashMap::new(),
            set_layouts: HashMap::new(),
            descriptor_pools: HashMap::new(),
            descriptor_sets: HashMap::new(),
            command_buffers: HashMap::new(),
            fences: HashMap::new(),
            surfaces: HashMap::new(),
            swapchains: HashMap::new(),
            next_ir: 0,
            next_handle: 0,
            fence_value: 1,
        }
    }

    // ---- id minting -------------------------------------------------------------------------------

    /// Allocate a fresh IR object id (buffer/shader/pipeline/bind-group/texture/fence/surface — one
    /// shared namespace). Ported from `VkState::alloc_ir` (pre-increment: first id is 1).
    pub fn alloc_ir(&mut self) -> u32 {
        self.next_ir += 1;
        self.next_ir
    }

    /// Allocate a fresh non-dispatchable Vulkan handle (a monotonic `u64`, never `VK_NULL_HANDLE`).
    /// Ported from `VkState::alloc_handle`.
    pub fn alloc_handle(&mut self) -> u64 {
        self.next_handle += 1;
        0x1000_0000 + self.next_handle
    }

    /// The next timeline value to signal/wait a fence at (monotonic across the device).
    pub fn next_fence_value(&mut self) -> u64 {
        let v = self.fence_value;
        self.fence_value += 1;
        v
    }

    // ---- convenience helpers ----------------------------------------------------------------------

    /// The (ir buffer id, offset=0, bytes) upload for every currently-mapped, buffer-bound memory — the
    /// per-frame flush of persistently-mapped HOST_COHERENT buffers (e.g. vkcube's rotating MVP UBO,
    /// written each frame with NO `vkUnmapMemory`). Emitted as `Cmd::WriteBuffer` at `vkQueueSubmit`
    /// by [`crate::service::submit`]. Ported from `VkState::flush_mapped` (kept `Cmd`-free here — the
    /// model never builds a `Cmd`).
    pub fn mapped_uploads(&self) -> Vec<(u32, u64, Vec<u8>)> {
        self.memories
            .values()
            .filter(|m| m.mapped)
            .filter_map(|m| {
                let bh = m.bound_buffer?;
                let b = self.buffers.get(&bh)?;
                let n = (b.size as usize).min(m.data.len());
                Some((b.ir_id, 0u64, m.data[..n].to_vec()))
            })
            .collect()
    }
}
