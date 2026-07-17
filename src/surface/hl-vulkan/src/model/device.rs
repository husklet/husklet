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
use super::descriptor::{DescriptorPoolRec, DescriptorUpdateTemplateRec, DsetRec, SetLayoutRec};
use super::instance::PhysicalDeviceDesc;
use super::memory::{BufferRec, ImageRec, MemRec, SamplerRec};
use super::pipeline::{PipelineCacheRec, PipelineLayoutRec, PipelineRec, ShaderRec};
use super::queue::{FenceRec, Queue, SurfaceRec, SwapchainRec};
use super::sync::{EventRec, QueryPoolRec, SemaphoreRec};
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
    pub descriptor_update_templates:
        HashMap<VkDescriptorUpdateTemplate, DescriptorUpdateTemplateRec>,
    pub pipeline_caches: HashMap<VkPipelineCache, PipelineCacheRec>,
    pub command_buffers: HashMap<VkCommandBuffer, CmdBufRec>,
    pub fences: HashMap<VkFence, FenceRec>,
    pub semaphores: HashMap<VkSemaphore, SemaphoreRec>,
    pub events: HashMap<VkEvent, EventRec>,
    pub query_pools: HashMap<VkQueryPool, QueryPoolRec>,
    pub surfaces: HashMap<VkSurfaceKHR, SurfaceRec>,
    pub swapchains: HashMap<VkSwapchainKHR, SwapchainRec>,

    /// `VkImage` handle → its last-recorded `VkImageLayout` (raw). Layout is implicit in the hl-GPU IR
    /// (the executor needs no explicit transitions), so this is correctness bookkeeping only: a
    /// `vkCmdPipelineBarrier` records the transition here and emits no IR. Ported (simplified) from
    /// `hl-shim-vk`'s per-subresource layout tracker.
    pub image_layouts: HashMap<VkImage, i32>,

    // ---- id counters (monotonic) ----
    /// hl-GPU IR object-id counter — one shared namespace across every resource kind (the host backend
    /// keys per-kind, so cross-kind overlap is irrelevant). Ported from `VkState::next_ir`.
    next_ir: u32,
    /// Non-dispatchable Vulkan-handle counter (a monotonic `u64`, never 0 == `VK_NULL_HANDLE`).
    next_handle: u64,
    /// Timeline value for the next fence signal (monotonic across the device).
    fence_value: u64,
    /// Host-monotonic serial handed out by `vkCmdWriteTimestamp` (strictly increasing; not wall-clock).
    timestamp: u64,
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
            descriptor_update_templates: HashMap::new(),
            pipeline_caches: HashMap::new(),
            command_buffers: HashMap::new(),
            fences: HashMap::new(),
            semaphores: HashMap::new(),
            events: HashMap::new(),
            query_pools: HashMap::new(),
            surfaces: HashMap::new(),
            swapchains: HashMap::new(),
            image_layouts: HashMap::new(),
            next_ir: 0,
            next_handle: 0,
            fence_value: 1,
            timestamp: 1,
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

    /// The next host-monotonic timestamp serial (`vkCmdWriteTimestamp`). Strictly increasing — the only
    /// guarantee an app may rely on across two timestamps in submission order.
    pub fn next_timestamp(&mut self) -> u64 {
        let v = self.timestamp;
        self.timestamp += 1;
        v
    }

    // ---- convenience helpers ----------------------------------------------------------------------

    /// The `(ir buffer id, buffer offset, bytes)` host→device upload for every buffer-bound memory that
    /// needs one this submit: either it is currently mapped (the per-frame flush of persistently-mapped
    /// HOST_COHERENT buffers — e.g. vkcube's rotating MVP UBO, written each frame with NO
    /// `vkUnmapMemory`), OR it has a captured `pending_flush` from a `vkUnmapMemory` /
    /// `vkFlushMappedMemoryRanges` (the app staged bytes and unmapped BEFORE submitting — those writes
    /// must still reach the device). Emitted as `Cmd::WriteBuffer` at `vkQueueSubmit` by
    /// [`crate::service::submit`], which then calls [`Self::clear_pending_uploads`].
    ///
    /// The two paths are coalesced: each memory yields AT MOST ONE upload, so a buffer that is submitted
    /// while still mapped (with a pending range also set) is never written twice. A still-mapped memory
    /// keeps the original whole-buffer-from-offset-0 flush byte-for-byte; a pending-only (unmapped)
    /// memory honors its captured range, intersected with the bound buffer's footprint in the allocation
    /// (the same math as `service::create::read_mapped`, offset `mem - bound_offset`). Unbound host-only
    /// staging is never uploaded (no device buffer). Ported from `VkState::flush_mapped` (kept `Cmd`-free
    /// here — the model never builds a `Cmd`).
    pub fn mapped_uploads(&self) -> Vec<(u32, u64, Vec<u8>)> {
        let mut out = Vec::new();
        for m in self.memories.values() {
            // A single allocation routinely backs MANY buffers (the sub-allocating arena of
            // gpu-alloc/VMA — e.g. blade/GPUI binds hundreds of buffers into one HOST_COHERENT block).
            // Flush EVERY bound buffer against its own footprint; tracking only one silently dropped the
            // upload of all the others (their device bytes stayed zero — a blank frame).
            let mem_len = m.data.len() as u64;
            for b in m.bound_buffers.iter().filter_map(|h| self.buffers.get(h)) {
                if m.mapped {
                    // Still-mapped coherent flush: the whole buffer, read from its footprint in the
                    // allocation `[bound_offset, bound_offset + size)` and uploaded to buffer offset 0.
                    let start = b.bound_offset.min(mem_len);
                    let end = b.bound_offset.saturating_add(b.size).min(mem_len);
                    if end <= start {
                        continue; // the buffer's footprint lies entirely past the allocation
                    }
                    out.push((b.ir_id, 0u64, m.data[start as usize..end as usize].to_vec()));
                    continue;
                }
                // Unmapped, but a pending host→device upload was captured — flush the honored range.
                let Some((offset, size)) = m.pending_flush else {
                    continue;
                };
                let map_start = offset.min(mem_len);
                let map_end = if size == u64::MAX {
                    mem_len
                } else {
                    offset.saturating_add(size).min(mem_len)
                };
                // Intersect the dirtied range with the buffer's footprint in the allocation.
                let start = map_start.max(b.bound_offset);
                let end = map_end.min(b.bound_offset.saturating_add(b.size));
                if end <= start {
                    continue; // the pending range does not overlap this bound buffer
                }
                let buf_off = start - b.bound_offset;
                out.push((
                    b.ir_id,
                    buf_off,
                    m.data[start as usize..end as usize].to_vec(),
                ));
            }
        }
        out
    }

    /// Clear every captured `pending_flush` after a `vkQueueSubmit` has flushed it — the pending
    /// host→device upload is one-shot (it reaches the device exactly once, at the next submit). The
    /// still-mapped `mapped` flag is untouched (a persistently-mapped buffer keeps re-flushing).
    pub fn clear_pending_uploads(&mut self) {
        for m in self.memories.values_mut() {
            m.pending_flush = None;
        }
    }
}
