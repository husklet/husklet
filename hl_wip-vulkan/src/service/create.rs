//! Resource creation — the `vkCreate*` / `vkAllocate*` lowering.
//!
//! Ported from `hl-shim-vk/src/{instance.rs,memory.rs,pipeline.rs,descriptor.rs}`. Instance/device
//! creation is pure object-model (no IR). Buffer/image/sampler/shader/pipeline/fence creation each
//! mints an hl-GPU id and submits the matching [`Cmd`]; device-memory allocation + binding is host
//! bookkeeping (the bytes upload lazily at `vkQueueSubmit`, see [`super::submit`]). The keystone: a
//! `VkShaderModule`'s SPIR-V forwards to [`Cmd::CreateShader`] with NO translation
//! ([`crate::adapter::spirv`]).

use crate::adapter::spirv;
use crate::model::descriptor::{
    is_buffer_descriptor, DescriptorPoolRec, DescriptorTemplateEntry, DescriptorUpdateTemplateRec,
    DsetRec, LayoutBinding, SetLayoutRec, TemplateBufferInfo,
    VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
};
use crate::model::instance::Instance;
use crate::model::memory::{
    buffer_usage_from_vk, is_render_target, tex_format_from_vk, texture_usage_from_vk, BufferRec,
    ImageRec, MemRec, SamplerRec,
};
use crate::model::pipeline::{PipelineCacheRec, PipelineKind, PipelineLayoutRec, PipelineRec, ShaderRec};
use crate::model::queue::FenceRec;
use crate::*;
use hl_gpu::protocol::model::descriptor::{
    BlendState, BufferDesc, ComputePipelineDesc, DepthState, RenderPipelineDesc, SamplerDesc,
    ShaderRef, VertexLayout,
};
use hl_gpu::protocol::model::enums::{AddressMode, Filter, TextureFormat, Topology};
use hl_gpu::{BufferId, Cmd, CommandSink, GpuError, Result};
use hl_log::tag;

// ---- instance / device (pure object model — no IR) -----------------------------------------------

/// `vkCreateInstance` — build the instance exposing the hl physical device.
pub fn create_instance(app_api_version: u32) -> Instance {
    Instance::new(app_api_version)
}

/// `vkCreateDevice` — build a logical device over the instance's physical device.
pub fn create_device(instance: &Instance) -> Device {
    Device::new(instance.physical_device.clone())
}

// ---- buffers / device memory ---------------------------------------------------------------------

/// `vkCreateBuffer` — mint an hl-GPU buffer id, translate the usage, and submit [`Cmd::CreateBuffer`].
/// `vk_usage` is a raw `VkBufferUsageFlags` bitset.
pub fn create_buffer(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vk_usage: u32,
    size: u64,
) -> Result<VkBuffer> {
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    let usage = buffer_usage_from_vk(vk_usage);
    sink.submit(&[Cmd::CreateBuffer(ir_id, BufferDesc { size, usage, label: format!("vkbuf{ir_id}") })])?;
    dev.buffers.insert(handle, BufferRec { ir_id, size, usage, bound_mem: None, bound_offset: 0 });
    Ok(handle)
}

/// `vkDestroyBuffer` — destroy the backing hl-GPU buffer. No-op on `VK_NULL_HANDLE`/unknown handle.
pub fn destroy_buffer(dev: &mut Device, sink: &mut dyn CommandSink, buffer: VkBuffer) -> Result<()> {
    if let Some(b) = dev.buffers.remove(&buffer) {
        sink.submit(&[Cmd::DestroyBuffer(b.ir_id)])?;
    }
    Ok(())
}

/// `vkAllocateMemory` — allocate `size` bytes of host-visible unified memory (zeroed). No IR: the bytes
/// upload to the host lazily (mapped-flush at `vkQueueSubmit`).
pub fn allocate_memory(dev: &mut Device, size: u64) -> VkDeviceMemory {
    let handle = dev.alloc_handle();
    dev.memories.insert(
        handle,
        MemRec { data: vec![0u8; size as usize], size, bound_buffers: Vec::new(), mapped: false, pending_flush: None },
    );
    handle
}

/// `vkBindBufferMemory` — bind `memory` to `buffer` at `offset`. Errors on an unknown handle. No IR
/// (binding is bookkeeping; the buffer already has its backing hl-GPU id).
pub fn bind_buffer_memory(
    dev: &mut Device,
    buffer: VkBuffer,
    memory: VkDeviceMemory,
    offset: u64,
) -> Result<()> {
    if !dev.memories.contains_key(&memory) {
        return Err(GpuError::Invalid("vkBindBufferMemory: unknown VkDeviceMemory"));
    }
    let b = dev
        .buffers
        .get_mut(&buffer)
        .ok_or(GpuError::Invalid("vkBindBufferMemory: unknown VkBuffer"))?;
    b.bound_mem = Some(memory);
    b.bound_offset = offset;
    // Record this buffer as bound into the allocation (many buffers may share one allocation — the
    // sub-allocating arena pattern; see `MemRec::bound_buffers`). Dedup so a re-bind of the same buffer
    // (legal before first use) does not duplicate its flush.
    let bound = &mut dev.memories.get_mut(&memory).unwrap().bound_buffers;
    if !bound.contains(&buffer) {
        bound.push(buffer);
    }
    Ok(())
}

/// `vkMapMemory` — mark `memory` mapped (its bytes now flush to the host at each submit). Errors on an
/// unknown handle.
pub fn map_memory(dev: &mut Device, memory: VkDeviceMemory) -> Result<()> {
    dev.memories
        .get_mut(&memory)
        .ok_or(GpuError::Invalid("vkMapMemory: unknown VkDeviceMemory"))?
        .mapped = true;
    Ok(())
}

/// Write `bytes` into a mapped memory at `offset` (the app's `memcpy` into the mapped pointer). Errors
/// on an unknown/short range.
pub fn write_mapped(
    dev: &mut Device,
    memory: VkDeviceMemory,
    offset: u64,
    bytes: &[u8],
) -> Result<()> {
    let m = dev
        .memories
        .get_mut(&memory)
        .ok_or(GpuError::Invalid("write_mapped: unknown VkDeviceMemory"))?;
    let end = offset as usize + bytes.len();
    if end > m.data.len() {
        return Err(GpuError::OutOfBounds);
    }
    m.data[offset as usize..end].copy_from_slice(bytes);
    Ok(())
}

/// The `VkDeviceSize` sentinel meaning "to the end of the allocation" (`VK_WHOLE_SIZE`).
const VK_WHOLE_SIZE: u64 = u64::MAX;

/// Refresh a host-visible mapped memory's staging bytes with the CURRENT device contents of the buffer
/// bound into it, reading them back over the sink — the device→host path, the SAME
/// [`CommandSink::read_buffer`] that serves cuda's `cuMemcpyDtoH` and GL's `glReadPixels`. This is what
/// makes GPU output observable through the mapped pointer: the staging bytes are the app's own last
/// upload, so without a readback a reader sees only its stale writes, never what the device computed.
///
/// `offset`/`size` bound the refreshed region (`size == VK_WHOLE_SIZE` → to the end of the allocation),
/// matching the `vkMapMemory` / `VkMappedMemoryRange` the app requested; only the sub-range that actually
/// overlaps the bound buffer's footprint in the allocation is read (the buffer offset is `mem_offset -
/// bound_offset`). Memory with NO bound buffer is host-only staging with no readable device source, so it
/// is left exactly as-is — data is never faked. Errors only if the sink's readback transport itself fails;
/// an unknown/unbound memory is a no-op success.
pub fn read_mapped(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    memory: VkDeviceMemory,
    offset: u64,
    size: u64,
) -> Result<()> {
    // Resolve EVERY buffer bound into this allocation + its footprint (many buffers may share one arena;
    // see `MemRec::bound_buffers`), plus the staging length.
    let Some((buffers, mem_len)) = dev.memories.get(&memory).map(|m| {
        let bufs: Vec<(u32, u64, u64)> = m
            .bound_buffers
            .iter()
            .filter_map(|h| dev.buffers.get(h).map(|b| (b.ir_id, b.size, b.bound_offset)))
            .collect();
        (bufs, m.data.len() as u64)
    }) else {
        return Ok(()); // unknown memory
    };
    if buffers.is_empty() {
        return Ok(()); // host-only staging with no device buffer to read back
    }

    // The requested map range, clamped to the allocation.
    let map_start = offset.min(mem_len);
    let map_end = if size == VK_WHOLE_SIZE { mem_len } else { offset.saturating_add(size).min(mem_len) };
    // Refresh each bound buffer whose footprint [bound_offset, bound_offset + buf_size) overlaps the range.
    for (ir_id, buf_size, bound_offset) in buffers {
        let start = map_start.max(bound_offset);
        let end = map_end.min(bound_offset.saturating_add(buf_size));
        if end <= start {
            continue; // this buffer's footprint does not overlap the mapped range
        }
        let read_off = start - bound_offset;
        let len = (end - start) as usize;
        let bytes = sink.read_buffer(BufferId(ir_id), read_off, len)?;
        let m = dev.memories.get_mut(&memory).expect("memory validated above");
        let dst = start as usize;
        let n = bytes.len().min(m.data.len().saturating_sub(dst));
        m.data[dst..dst + n].copy_from_slice(&bytes[..n]);
    }
    Ok(())
}

/// `vkUnmapMemory` — clear the mapped flag, but CAPTURE the still-mapped bytes as a pending host→device
/// upload so the app's writes survive the unmap. Without this, a real app doing map → write → UNMAP
/// (staging before submit) would silently drop its upload: the still-mapped submit flush no longer sees
/// the memory, so the bytes never reach the device. The whole allocation is marked dirty `(0,
/// VK_WHOLE_SIZE)`; the next `vkQueueSubmit` flushes it (intersected with the bound buffer's footprint)
/// as a `Cmd::WriteBuffer` and clears the record. Unbound host-only staging has no device buffer to
/// upload to, so nothing is captured (a truthful no-op). Coalesced with the still-mapped path so a
/// buffer that is submitted while still mapped is never written twice.
pub fn unmap_memory(dev: &mut Device, memory: VkDeviceMemory) {
    if let Some(m) = dev.memories.get_mut(&memory) {
        m.mapped = false;
        if !m.bound_buffers.is_empty() {
            m.pending_flush = Some((0, VK_WHOLE_SIZE));
        }
    }
}

/// Capture a dirtied mapped range `(offset, size)` as a pending host→device upload that must reach the
/// device at the next `vkQueueSubmit` even if the app unmaps first — the `vkFlushMappedMemoryRanges`
/// signal for the non-coherent contract. Only buffer-bound memory is captured (unbound host-only
/// staging has no device buffer, so it stays a truthful no-op); an unknown handle is ignored. Any range
/// already pending this submit is widened to cover both, so a sub-range flush followed by an unmap (or
/// another flush) never loses the earlier bytes.
pub fn capture_pending_upload(dev: &mut Device, memory: VkDeviceMemory, offset: u64, size: u64) {
    if let Some(m) = dev.memories.get_mut(&memory) {
        if !m.bound_buffers.is_empty() {
            m.pending_flush = Some(match m.pending_flush {
                Some(prev) => widen_range(prev, (offset, size)),
                None => (offset, size),
            });
        }
    }
}

/// Merge two `(offset, size)` upload ranges into one that covers both (`size == VK_WHOLE_SIZE` extends
/// to the end of the allocation, so any whole-size operand yields a whole-size result from the smaller
/// offset). The result is always a superset of both inputs — the flush intersects it with the buffer's
/// footprint at submit, so over-covering is safe (`data` is the source of truth) and never drops bytes.
fn widen_range(a: (u64, u64), b: (u64, u64)) -> (u64, u64) {
    let start = a.0.min(b.0);
    if a.1 == VK_WHOLE_SIZE || b.1 == VK_WHOLE_SIZE {
        return (start, VK_WHOLE_SIZE);
    }
    let end = a.0.saturating_add(a.1).max(b.0.saturating_add(b.1));
    (start, end - start)
}

// ---- images / samplers ---------------------------------------------------------------------------

/// `vkCreateImage` — mint an hl-GPU texture id, translate format/usage, and submit [`Cmd::CreateTexture`].
/// `vk_format` is a raw `VkFormat`; `vk_usage` a raw `VkImageUsageFlags`.
pub fn create_image(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    width: u32,
    height: u32,
    vk_format: u32,
    vk_usage: u32,
) -> Result<VkImage> {
    use hl_gpu::protocol::model::descriptor::TextureDesc;
    use hl_gpu::protocol::model::enums::TextureDim;
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    let format = tex_format_from_vk(vk_format);
    let usage = texture_usage_from_vk(vk_usage);
    sink.submit(&[Cmd::CreateTexture(
        ir_id,
        TextureDesc {
            width,
            height,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format,
            usage,
            label: format!("vkimg{ir_id}"),
        },
    )])?;
    dev.images.insert(
        handle,
        ImageRec { ir_id, width, height, format, usage, is_render_target: is_render_target(vk_usage) },
    );
    Ok(handle)
}

/// `vkCreateSampler` — translate the filter/address state and submit [`Cmd::CreateSampler`]. The `vk_*`
/// arguments are raw Vulkan enum values (`VkFilter`, `VkSamplerMipmapMode`, `VkSamplerAddressMode`).
pub fn create_sampler(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vk_min_filter: u32,
    vk_mag_filter: u32,
    vk_mipmap_mode: u32,
    vk_address_uvw: [u32; 3],
) -> VkSampler {
    let desc = SamplerDesc {
        min_filter: ir_filter(vk_min_filter),
        mag_filter: ir_filter(vk_mag_filter),
        mip_filter: ir_filter(vk_mipmap_mode), // VkSamplerMipmapMode shares NEAREST=0/LINEAR=1
        address_u: ir_address(vk_address_uvw[0]),
        address_v: ir_address(vk_address_uvw[1]),
        address_w: ir_address(vk_address_uvw[2]),
    };
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    // Submit is infallible against the recording sink; a real sink would surface a transport error.
    let _ = sink.submit(&[Cmd::CreateSampler(ir_id, desc)]);
    dev.samplers.insert(handle, SamplerRec { ir_id });
    handle
}

/// `VkFilter` (0 = NEAREST, 1 = LINEAR) → hl-GPU [`Filter`].
fn ir_filter(v: u32) -> Filter {
    if v == 1 {
        Filter::Linear
    } else {
        Filter::Nearest
    }
}

/// `VkSamplerAddressMode` → hl-GPU [`AddressMode`] (CLAMP_TO_BORDER / MIRROR_CLAMP fold to the nearest
/// supported neighbour — a bounded translation, ported from `memory.rs::ir_address`).
fn ir_address(v: u32) -> AddressMode {
    match v {
        0 => AddressMode::Repeat,                                     // REPEAT
        1 | 4 => AddressMode::MirrorRepeat,                           // MIRRORED_REPEAT / MIRROR_CLAMP
        _ => AddressMode::ClampToEdge,                                // CLAMP_TO_EDGE / CLAMP_TO_BORDER
    }
}

// ---- shader modules / pipelines ------------------------------------------------------------------

/// `vkCreateShaderModule` from a `pCode` byte image — validate the SPIR-V header, parse its
/// `OpEntryPoint` names, and submit [`Cmd::CreateShader`] with the words forwarded VERBATIM.
pub fn create_shader_module(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    code: &[u8],
) -> Result<VkShaderModule> {
    let words = spirv::words_from_bytes(code)?;
    create_shader_module_words(dev, sink, words)
}

/// `vkCreateShaderModule` from SPIR-V words directly (the `pCode` already reinterpreted as `u32`s).
pub fn create_shader_module_words(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    words: Vec<u32>,
) -> Result<VkShaderModule> {
    spirv::validate(&words)?;
    let entries = spirv::entry_points(&words);
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[spirv::create_shader(ir_id, words.clone())])?;
    hl_log::hl_debug!(tag::VULKAN, "shader ir={} words={} entries={}", ir_id, words.len(), entries.len());
    dev.shaders.insert(handle, ShaderRec { ir_id, spirv: words, entries });
    Ok(handle)
}

/// `vkCreateComputePipelines` (one pipeline) — resolve the compute stage's module + entry and submit
/// [`Cmd::CreateComputePipeline`]. Errors (`VK_ERROR_UNKNOWN` analogue) if the module or entry is
/// missing — no id-zero default. Ported from `pipeline.rs::vkCreateComputePipelines`.
pub fn create_compute_pipeline(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    shader: VkShaderModule,
    entry: &str,
) -> Result<VkPipeline> {
    let shader_ir = {
        let sh = dev
            .shaders
            .get(&shader)
            .ok_or(GpuError::Invalid("vkCreateComputePipelines: unknown VkShaderModule"))?;
        if !sh.has_entry(entry) {
            return Err(GpuError::Invalid("vkCreateComputePipelines: entry point not in module"));
        }
        sh.ir_id
    };
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[Cmd::CreateComputePipeline(
        ir_id,
        ComputePipelineDesc {
            compute: ShaderRef { module: shader_ir, entry: entry.to_string() },
            label: format!("vkcpipe{ir_id}"),
        },
    )])?;
    hl_log::hl_debug!(tag::VULKAN, "pipeline kind=compute ir={} shader={} entry={}", ir_id, shader_ir, entry);
    dev.pipelines.insert(handle, PipelineRec { ir_id, kind: PipelineKind::Compute });
    Ok(handle)
}

/// `vkCreateGraphicsPipelines` (one pipeline) — resolve the vertex (+ optional fragment) stage(s), carry
/// the `VkPipelineVertexInputState` vertex-buffer layout(s), and submit [`Cmd::CreateRenderPipeline`] with
/// one color target per entry in `color_formats`. Ported from `pipeline.rs::vkCreateGraphicsPipelines`
/// (the bring-up subset: no blend/depth). `vertex_layouts` are the translated
/// `VkVertexInputBindingDescription`s (slot-0 layout is what the host rasterizer fetches positions from).
///
/// `color_formats` carries one format per color attachment — sourced from the bound `VkRenderPass`'s
/// attachments in the classic path, or from the pipeline's `VkPipelineRenderingCreateInfo::pColorAttachmentFormats`
/// pNext in the dynamic-rendering path (a null `renderPass`). An empty slice is valid (a depth-only /
/// no-color pipeline), yielding a pipeline with no color targets.
///
/// `depth` is the pipeline's depth-test state (format + write-enable + compare op) when
/// `VkPipelineDepthStencilStateCreateInfo::depthTestEnable` is set, else `None`. A `Some(..)` pipeline
/// MUST be drawn in a pass carrying a matching depth attachment (wgpu enforces this) — the shim threads
/// that attachment through the dynamic-rendering `vkCmdBeginRendering` depth target. Without this the
/// depth-stencil state was dropped (`depth: None` hardcoded) and every depth-tested draw ran with the
/// test disabled, so a far primitive could never be occluded by a nearer one.
///
/// `blend` is the pipeline's fixed-function color-blend state (src/dst factors + ops) when the color
/// attachment's `VkPipelineColorBlendAttachmentState::blendEnable` is set, else `None`. It is applied to
/// every color target. Without this the blend state was dropped (`blend: None` hardcoded) and a
/// translucent (alpha-over) draw OVERWROTE the destination instead of compositing over it.
pub fn create_graphics_pipeline(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vertex: (VkShaderModule, &str),
    fragment: Option<(VkShaderModule, &str)>,
    vertex_layouts: Vec<VertexLayout>,
    color_formats: Vec<TextureFormat>,
    depth: Option<DepthState>,
    blend: Option<BlendState>,
) -> Result<VkPipeline> {
    use hl_gpu::protocol::model::descriptor::ColorTargetState;
    let resolve = |dev: &Device, (module, entry): (VkShaderModule, &str)| -> Result<ShaderRef> {
        let sh = dev
            .shaders
            .get(&module)
            .ok_or(GpuError::Invalid("vkCreateGraphicsPipelines: unknown VkShaderModule"))?;
        if !sh.has_entry(entry) {
            return Err(GpuError::Invalid("vkCreateGraphicsPipelines: entry point not in module"));
        }
        Ok(ShaderRef { module: sh.ir_id, entry: entry.to_string() })
    };
    let vertex_ref = resolve(dev, vertex)?;
    let fragment_ref = fragment.map(|f| resolve(dev, f)).transpose()?;
    let color_targets = color_formats
        .into_iter()
        .map(|format| ColorTargetState { format, blend: blend.clone(), write_mask: 0xf })
        .collect::<Vec<_>>();
    let color_targets_len = color_targets.len();
    let has_fragment = fragment_ref.is_some();
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[Cmd::CreateRenderPipeline(
        ir_id,
        RenderPipelineDesc {
            vertex: vertex_ref,
            fragment: fragment_ref,
            vertex_buffers: vertex_layouts,
            color_targets,
            depth,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            label: format!("vkgpipe{ir_id}"),
        },
    )])?;
    hl_log::hl_debug!(tag::VULKAN, "pipeline kind=graphics ir={} frag={} targets={}", ir_id, has_fragment, color_targets_len);
    dev.pipelines.insert(handle, PipelineRec { ir_id, kind: PipelineKind::Graphics });
    Ok(handle)
}

/// `vkCreatePipelineLayout` — record the composed set-layouts. No IR (bindings arrive with the sets).
pub fn create_pipeline_layout(dev: &mut Device, set_layouts: Vec<VkDescriptorSetLayout>) -> VkPipelineLayout {
    let handle = dev.alloc_handle();
    dev.pipeline_layouts.insert(handle, PipelineLayoutRec { set_layouts });
    handle
}

// ---- descriptor sets -----------------------------------------------------------------------------

/// `vkCreateDescriptorSetLayout` — record the immutable binding table. No IR.
pub fn create_descriptor_set_layout(dev: &mut Device, bindings: Vec<LayoutBinding>) -> VkDescriptorSetLayout {
    let handle = dev.alloc_handle();
    dev.set_layouts.insert(handle, SetLayoutRec { bindings });
    handle
}

/// `vkCreateDescriptorPool` — record the pool capacity. No IR.
pub fn create_descriptor_pool(dev: &mut Device, max_sets: u32) -> VkDescriptorPool {
    let handle = dev.alloc_handle();
    dev.descriptor_pools.insert(handle, DescriptorPoolRec { max_sets, allocated: 0 });
    handle
}

/// `vkAllocateDescriptorSets` (one set) — allocate a set of `layout` from `pool`. Errors
/// (`VK_ERROR_OUT_OF_POOL_MEMORY` analogue) if the pool's `max_sets` quota is exhausted. No IR — the
/// IR bind group is built later at `vkCmdBindDescriptorSets` ([`super::record`]).
pub fn allocate_descriptor_set(
    dev: &mut Device,
    pool: VkDescriptorPool,
    layout: VkDescriptorSetLayout,
    set_index: u32,
) -> Result<VkDescriptorSet> {
    let p = dev
        .descriptor_pools
        .get_mut(&pool)
        .ok_or(GpuError::Invalid("vkAllocateDescriptorSets: unknown VkDescriptorPool"))?;
    if p.max_sets != 0 && p.allocated >= p.max_sets {
        return Err(GpuError::ResourceLimit("vkAllocateDescriptorSets: pool out of sets"));
    }
    p.allocated += 1;
    let handle = dev.alloc_handle();
    dev.descriptor_sets.insert(handle, DsetRec::new(set_index, layout, pool));
    Ok(handle)
}

/// `vkUpdateDescriptorSets` (one buffer write) — record a `binding -> (buffer, offset, range)` entry on
/// the set. Errors on an unknown set. No IR (resolved at bind time).
pub fn update_descriptor_buffer(
    dev: &mut Device,
    set: VkDescriptorSet,
    binding: u32,
    buffer: VkBuffer,
    offset: u64,
    range: u64,
) -> Result<()> {
    let rec = dev
        .descriptor_sets
        .get_mut(&set)
        .ok_or(GpuError::Invalid("vkUpdateDescriptorSets: unknown VkDescriptorSet"))?;
    rec.buffers.insert(binding, (buffer, offset, range));
    Ok(())
}

/// `vkUpdateDescriptorSets` (one image/sampler write) — record a sampled-image / sampler descriptor on
/// the set at `binding`. `image` is the `VkImage` a `SAMPLED_IMAGE`/`COMBINED_IMAGE_SAMPLER` write's
/// `imageView` resolves to (the shim owns the view→image mapping); `sampler` is the `VkSampler` a
/// `SAMPLER`/`COMBINED_IMAGE_SAMPLER` write carries. Either may be `None` (a separate SAMPLED_IMAGE or
/// SAMPLER write). Present fields overwrite; absent fields leave any prior value (so separate image + sampler
/// writes to the same binding compose). Errors on an unknown set. No IR (resolved at bind time).
pub fn update_descriptor_image(
    dev: &mut Device,
    set: VkDescriptorSet,
    binding: u32,
    image: Option<VkImage>,
    sampler: Option<VkSampler>,
) -> Result<()> {
    let rec = dev
        .descriptor_sets
        .get_mut(&set)
        .ok_or(GpuError::Invalid("vkUpdateDescriptorSets: unknown VkDescriptorSet"))?;
    let entry = rec.images.entry(binding).or_default();
    if image.is_some() {
        entry.image = image;
    }
    if sampler.is_some() {
        entry.sampler = sampler;
    }
    Ok(())
}

// ---- descriptor update templates -----------------------------------------------------------------

/// `vkCreateDescriptorUpdateTemplate(KHR)` — retain the immutable entry table (offset/stride/binding/
/// type) the app later pushes descriptors through. Only the `DESCRIPTOR_SET` template type is modeled
/// (push-descriptor templates need a bound pipeline layout the bring-up path lacks), so a different type
/// is a truthful `VK_ERROR_FEATURE_NOT_PRESENT` analogue. No IR. Ported from
/// `hl-shim-vk/src/descriptor.rs::vkCreateDescriptorUpdateTemplate`.
pub fn create_descriptor_update_template(
    dev: &mut Device,
    template_type: i32,
    entries: Vec<DescriptorTemplateEntry>,
) -> Result<VkDescriptorUpdateTemplate> {
    if template_type != VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET {
        return Err(GpuError::Unsupported(
            "vkCreateDescriptorUpdateTemplate: only DESCRIPTOR_SET templates are supported",
        ));
    }
    let handle = dev.alloc_handle();
    dev.descriptor_update_templates.insert(handle, DescriptorUpdateTemplateRec { entries });
    Ok(handle)
}

/// `vkDestroyDescriptorUpdateTemplate(KHR)` — drop the template. No-op on `VK_NULL_HANDLE`/unknown.
pub fn destroy_descriptor_update_template(dev: &mut Device, template: VkDescriptorUpdateTemplate) {
    dev.descriptor_update_templates.remove(&template);
}

/// `vkUpdateDescriptorSetWithTemplate(KHR)` — walk the template entries, reading each buffer descriptor
/// out of `data` at `entry.offset + i*entry.stride` and applying it to `set` **exactly as**
/// [`update_descriptor_buffer`] does (so a template update yields the same `binding -> (buffer, offset,
/// range)` table — and thus the same IR bind group — as the equivalent direct `vkUpdateDescriptorSets`).
/// Image/texel descriptors are not materialized in the compute path, so those entries are a truthful
/// no-op (mirroring the direct-write path). Errors on an unknown template or set; a short blob (an entry
/// reading past `data`) is a truthful out-of-bounds error, never a junk read. Ported from
/// `hl-shim-vk/src/descriptor.rs::vkUpdateDescriptorSetWithTemplate`.
pub fn update_descriptor_set_with_template(
    dev: &mut Device,
    set: VkDescriptorSet,
    template: VkDescriptorUpdateTemplate,
    data: &[u8],
) -> Result<()> {
    let entries = dev
        .descriptor_update_templates
        .get(&template)
        .ok_or(GpuError::Invalid("vkUpdateDescriptorSetWithTemplate: unknown VkDescriptorUpdateTemplate"))?
        .entries
        .clone();
    if !dev.descriptor_sets.contains_key(&set) {
        return Err(GpuError::Invalid("vkUpdateDescriptorSetWithTemplate: unknown VkDescriptorSet"));
    }
    for e in &entries {
        // Array elements fold onto the binding (the model keys a set's resources by binding), matching
        // `vkUpdateDescriptorSets`.
        if !is_buffer_descriptor(e.descriptor_type) {
            continue;
        }
        for i in 0..e.descriptor_count as usize {
            let base = e
                .offset
                .checked_add(i.checked_mul(e.stride).ok_or(GpuError::OutOfBounds)?)
                .ok_or(GpuError::OutOfBounds)?;
            let end = base.checked_add(core::mem::size_of::<TemplateBufferInfo>()).ok_or(GpuError::OutOfBounds)?;
            if end > data.len() {
                return Err(GpuError::OutOfBounds);
            }
            // The blob is app-provided bytes at an arbitrary offset — an unaligned read is required.
            let bi = unsafe { core::ptr::read_unaligned(data.as_ptr().add(base) as *const TemplateBufferInfo) };
            update_descriptor_buffer(dev, set, e.dst_binding, bi.buffer, bi.offset, bi.range)?;
        }
    }
    Ok(())
}

/// The number of bytes of the app's `pData` blob a template's BUFFER entries read — the max over every
/// buffer entry of `offset + (count-1)*stride + sizeof(VkDescriptorBufferInfo)`. The shim uses this to
/// build a correctly-bounded slice over the raw `pData` pointer (the C API carries no data size), so the
/// bounds check in [`update_descriptor_set_with_template`] is exact. `None` on an unknown template; `0`
/// if the template reads no buffer bytes.
pub fn descriptor_template_data_len(dev: &Device, template: VkDescriptorUpdateTemplate) -> Option<usize> {
    let rec = dev.descriptor_update_templates.get(&template)?;
    let mut max = 0usize;
    for e in &rec.entries {
        if !is_buffer_descriptor(e.descriptor_type) || e.descriptor_count == 0 {
            continue;
        }
        let last = (e.descriptor_count as usize - 1).saturating_mul(e.stride);
        let end = e.offset.saturating_add(last).saturating_add(core::mem::size_of::<TemplateBufferInfo>());
        max = max.max(end);
    }
    Some(max)
}

// ---- image subresource layout --------------------------------------------------------------------

/// A `VkSubresourceLayout` for one image subresource: byte `offset`/`size` of the subresource and the
/// `row_pitch` (bytes per row). The bring-up images are single-mip, single-layer 2D RGBA8 targets, so
/// array/depth pitch are 0. Values a linear-tiling app reads to `memcpy` into a mapped image.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SubresourceLayout {
    pub offset: u64,
    pub size: u64,
    pub row_pitch: u64,
    pub array_pitch: u64,
    pub depth_pitch: u64,
}

/// `vkGetImageSubresourceLayout` — report the linear layout of `image`'s base subresource. The modeled
/// images are 4-byte-per-texel (RGBA8/BGRA8) single-mip 2D targets: `row_pitch = width*4`,
/// `size = row_pitch*height`, tightly packed from offset 0. Errors on an unknown image. Ported (for the
/// single-subresource model) from `hl-shim-vk`'s image-layout reporting.
pub fn image_subresource_layout(dev: &Device, image: VkImage) -> Result<SubresourceLayout> {
    let img = dev
        .images
        .get(&image)
        .ok_or(GpuError::Invalid("vkGetImageSubresourceLayout: unknown VkImage"))?;
    let row_pitch = img.width as u64 * 4;
    Ok(SubresourceLayout {
        offset: 0,
        size: row_pitch * img.height as u64,
        row_pitch,
        array_pitch: 0,
        depth_pitch: 0,
    })
}

// ---- pipeline cache (modeled: a valid, versioned header; no host binary to cache) ----------------

/// The 32-byte `VkPipelineCacheHeaderVersionOne` prefix a valid pipeline-cache blob begins with:
/// `{ u32 length=32; u32 version=1; u32 vendorID; u32 deviceID; u8 uuid[16] }` (all little-endian, from
/// vk.xml). The hl-GPU pipelines forward SPIR-V verbatim (no compiled host artifact), so a cache carries
/// only this header — enough that a loader/app re-reading it via `vkGetPipelineCacheData` accepts it.
pub fn pipeline_cache_header(dev: &Device) -> Vec<u8> {
    const HEADER_LEN: u32 = 32;
    const VK_PIPELINE_CACHE_HEADER_VERSION_ONE: u32 = 1;
    let pd = &dev.physical_device;
    let mut hdr = Vec::with_capacity(HEADER_LEN as usize);
    hdr.extend_from_slice(&HEADER_LEN.to_le_bytes());
    hdr.extend_from_slice(&VK_PIPELINE_CACHE_HEADER_VERSION_ONE.to_le_bytes());
    hdr.extend_from_slice(&pd.vendor_id.to_le_bytes());
    hdr.extend_from_slice(&pd.device_id.to_le_bytes());
    hdr.extend_from_slice(&pd.pipeline_cache_uuid);
    hdr
}

/// `vkCreatePipelineCache` — mint a cache holding a valid header (plus any app-provided `initial_data`,
/// retained verbatim for round-trip). No IR.
pub fn create_pipeline_cache(dev: &mut Device, initial_data: &[u8]) -> VkPipelineCache {
    // A well-formed `initialDataSize` blob is retained as-is; anything else falls back to a fresh header.
    let data = if initial_data.len() >= 32 { initial_data.to_vec() } else { pipeline_cache_header(dev) };
    let handle = dev.alloc_handle();
    dev.pipeline_caches.insert(handle, PipelineCacheRec { data });
    handle
}

/// `vkDestroyPipelineCache` — drop the cache. No-op on `VK_NULL_HANDLE`/unknown.
pub fn destroy_pipeline_cache(dev: &mut Device, cache: VkPipelineCache) {
    dev.pipeline_caches.remove(&cache);
}

/// `vkMergePipelineCaches` — merge `src` caches into `dst`. There is no compiled artifact to combine, so
/// this is a truthful no-op that validates the handles. Errors on an unknown `dst`/`src` cache.
pub fn merge_pipeline_caches(dev: &Device, dst: VkPipelineCache, srcs: &[VkPipelineCache]) -> Result<()> {
    if !dev.pipeline_caches.contains_key(&dst) {
        return Err(GpuError::Invalid("vkMergePipelineCaches: unknown dst VkPipelineCache"));
    }
    if !srcs.iter().all(|s| dev.pipeline_caches.contains_key(s)) {
        return Err(GpuError::Invalid("vkMergePipelineCaches: unknown src VkPipelineCache"));
    }
    Ok(())
}

/// `vkGetPipelineCacheData` — the serialized cache blob (a spec-valid header). Errors on an unknown cache.
pub fn get_pipeline_cache_data(dev: &Device, cache: VkPipelineCache) -> Result<Vec<u8>> {
    Ok(dev
        .pipeline_caches
        .get(&cache)
        .ok_or(GpuError::Invalid("vkGetPipelineCacheData: unknown VkPipelineCache"))?
        .data
        .clone())
}

// ---- fences --------------------------------------------------------------------------------------

/// `vkCreateFence` — mint an hl-GPU fence id and submit [`Cmd::CreateFence`]. `signaled` reflects
/// `VK_FENCE_CREATE_SIGNALED_BIT`.
pub fn create_fence(dev: &mut Device, sink: &mut dyn CommandSink, signaled: bool) -> Result<VkFence> {
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[Cmd::CreateFence(ir_id)])?;
    dev.fences.insert(handle, FenceRec::new(ir_id, signaled));
    Ok(handle)
}
