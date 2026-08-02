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
use super::descriptor::{
    BufferViewRec, DescriptorPoolRec, DescriptorUpdateTemplateRec, DsetRec, SetLayoutRec,
};
use super::instance::PhysicalDeviceDesc;
use super::memory::{BufferRec, ImageRec, MemRec, SamplerRec};
use super::pipeline::{PipelineCacheRec, PipelineLayoutRec, PipelineRec, ShaderRec};
use super::queue::{FenceRec, Queue, SurfaceRec, SwapchainRec};
use super::sync::{EventRec, QueryPoolRec, SemaphoreRec};
use crate::*;
use hl_gpu::{GpuError, Result};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

/// The object-id namespace of one hl-GPU connection.
///
/// Several Vulkan logical devices can share one transport connection. Their Vulkan handles and object
/// tables remain device-local, but every IR id sent over that connection must be unique. Cloning this
/// value gives another device access to the same monotonic namespace.
#[derive(Clone, Default)]
pub struct IrIds(Arc<AtomicU32>);

impl IrIds {
    fn next(&self) -> u32 {
        // A connection cannot retain four billion live protocol objects: the runtime's residency limit
        // rejects it orders of magnitude earlier. Exhaustion therefore means the namespace invariant
        // itself was violated, not recoverable Vulkan input.
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("hl-GPU IR object id namespace exhausted")
            + 1
    }
}

#[cfg(test)]
mod ir_id_tests {
    use super::{Device, IrIds};
    use crate::PhysicalDeviceDesc;

    #[test]
    fn sibling_devices_on_one_connection_mint_distinct_ir_ids() {
        let ids = IrIds::default();
        let physical = PhysicalDeviceDesc::hl_default();

        // The CTS max-concurrent cases repeatedly construct logical devices in one process lineage.
        // Exercise that scale here: a device-local counter would mint id 1 on every iteration.
        for expected in 1..=16_384 {
            let mut device = Device::with_ir_ids(physical.clone(), ids.clone());
            assert_eq!(device.alloc_ir(), expected);
        }
    }

    #[test]
    fn standalone_devices_keep_an_independent_namespace() {
        let mut first = Device::new(PhysicalDeviceDesc::hl_default());
        let mut second = Device::new(PhysicalDeviceDesc::hl_default());

        assert_eq!(first.alloc_ir(), 1);
        assert_eq!(second.alloc_ir(), 1);
    }
}

/// The per-device aggregate: the object model + the id counters the lowering mutates.
pub struct Device {
    pub physical_device: PhysicalDeviceDesc,
    pub queue: Queue,

    // ---- handle tables (non-dispatchable handle -> record) ----
    pub buffers: HashMap<VkBuffer, BufferRec>,
    pub buffer_views: HashMap<VkBufferView, BufferViewRec>,
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
    ir_ids: IrIds,
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
        Self::with_ir_ids(physical_device, IrIds::default())
    }

    /// Create a logical device in an existing hl-GPU connection's object-id namespace.
    pub fn with_ir_ids(physical_device: PhysicalDeviceDesc, ir_ids: IrIds) -> Self {
        Self {
            physical_device,
            queue: Queue::primary(),
            buffers: HashMap::new(),
            buffer_views: HashMap::new(),
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
            ir_ids,
            next_handle: 0,
            fence_value: 1,
            timestamp: 1,
        }
    }

    // ---- id minting -------------------------------------------------------------------------------

    /// Allocate a fresh IR object id (buffer/shader/pipeline/bind-group/texture/fence/surface — one
    /// shared namespace). Ported from `VkState::alloc_ir` (pre-increment: first id is 1).
    pub fn alloc_ir(&mut self) -> u32 {
        self.ir_ids.next()
    }

    /// Allocate a fresh non-dispatchable Vulkan handle (a monotonic `u64`, never `VK_NULL_HANDLE`).
    /// Ported from `VkState::alloc_handle`.
    pub fn alloc_handle(&mut self) -> u64 {
        self.next_handle += 1;
        0x1000_0000 + self.next_handle
    }

    /// Mints an `Initial` command buffer owned by this device.
    pub fn allocate_command_buffer(&mut self) -> VkCommandBuffer {
        let handle = self.alloc_handle();
        self.command_buffers.insert(handle, CmdBufRec::initial());
        handle
    }

    /// Starts recording an owned command buffer.
    pub fn begin_command_buffer(
        &mut self,
        command_buffer: VkCommandBuffer,
        one_time_submit: bool,
    ) -> Result<()> {
        self.command_buffers
            .get_mut(&command_buffer)
            .ok_or(GpuError::Invalid(
                "vkBeginCommandBuffer: unknown VkCommandBuffer",
            ))?
            .begin(one_time_submit);
        Ok(())
    }

    /// Completes recording an owned command buffer.
    pub fn end_command_buffer(&mut self, command_buffer: VkCommandBuffer) -> Result<()> {
        self.command_buffers
            .get_mut(&command_buffer)
            .ok_or(GpuError::Invalid(
                "vkEndCommandBuffer: unknown VkCommandBuffer",
            ))?
            .end()
    }

    /// Resolve a recording command buffer for a command the specification confines to OUTSIDE a render
    /// pass.
    ///
    /// The executor refuses any transfer-shaped operation encoded between `BeginRenderPass` and
    /// `EndRenderPass` — it used to drop them and report success — so recording one there would fail the
    /// entire submit, taking unrelated correct work with it and naming nothing useful. Catching it here
    /// keeps the blast radius at the one command that was misused and lets `vkEndCommandBuffer` say
    /// which.
    pub(crate) fn require_recording_outside_pass(
        &mut self,
        command_buffer: VkCommandBuffer,
        command: &'static str,
    ) -> Result<&mut CmdBufRec> {
        let rec = self.require_recording(command_buffer)?;
        if rec.in_render_pass {
            return Err(GpuError::Invalid(command));
        }
        Ok(rec)
    }

    /// Latch a `vkCmd*` recorder's result onto its command buffer.
    ///
    /// Every `vkCmd*` returns `void`, so this is where a recording failure has to go: the buffer becomes
    /// invalid and `vkEndCommandBuffer` reports it. Call sites used to discard the `Result` outright,
    /// which is why a command buffer that had silently dropped work still ended successfully.
    pub fn latch<T>(&mut self, command_buffer: VkCommandBuffer, result: Result<T>) {
        if let Err(error) = result {
            // Say WHY, here, at the one choke point every `vkCmd*` refusal passes through.
            //
            // The reason is carried in the error and then thrown away: the caller sees only
            // `vkEndCommandBuffer` returning a code, one buffer-wide verdict for whichever command
            // failed first. That made a conformance result unattributable — 1,172 refused cases in
            // `dEQP-VK.api.copy_and_blit` on 2026-08-01 had to have their causes DERIVED by replaying
            // the driver's own predicates against the case names, because a probe with every guest tag
            // open captured nothing at all. Three distinct capabilities were hiding behind one error
            // code, and one of them was found only because 256 cases refused to fit the other two.
            //
            // `error` level because a release build compiles out everything below it, and this is
            // precisely the line someone needs from a shipped driver.
            hl_log::hl_verdict!(
                hl_log::tag::VULKAN,
                "command_buffer.refused",
                buffer = command_buffer,
                reason = %error;
                "command buffer {:#x} refused at record time: {}",
                command_buffer,
                error
            );
            if let Some(rec) = self.command_buffers.get_mut(&command_buffer) {
                rec.fail(error);
            }
        }
    }

    /// Resolves an owned command buffer and requires its recording state.
    pub(crate) fn require_recording(
        &mut self,
        command_buffer: VkCommandBuffer,
    ) -> Result<&mut CmdBufRec> {
        self.command_buffers
            .get_mut(&command_buffer)
            .ok_or(GpuError::Invalid("vkCmd*: unknown VkCommandBuffer"))?
            .require_recording()
    }

    /// Polls an owned fence without blocking.
    pub fn is_fence_signaled(&self, fence: VkFence) -> Result<bool> {
        Ok(self
            .fences
            .get(&fence)
            .ok_or(GpuError::Invalid("vkGetFenceStatus: unknown VkFence"))?
            .signaled)
    }

    /// Resets an owned fence for a later submission.
    pub fn reset_fence(&mut self, fence: VkFence) -> Result<()> {
        self.fences
            .get_mut(&fence)
            .ok_or(GpuError::Invalid("vkResetFences: unknown VkFence"))?
            .reset();
        Ok(())
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
