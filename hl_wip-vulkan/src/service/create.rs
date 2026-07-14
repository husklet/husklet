//! Resource creation — the `vkCreate*` / `vkAllocate*` lowering.
//!
//! Ported from `hl-shim-vk/src/{instance.rs,memory.rs,pipeline.rs,descriptor.rs}`. Instance/device
//! creation is pure object-model (no IR). Buffer/image/sampler/shader/pipeline/fence creation each
//! mints an hl-GPU id and submits the matching [`Cmd`]; device-memory allocation + binding is host
//! bookkeeping (the bytes upload lazily at `vkQueueSubmit`, see [`super::submit`]). The keystone: a
//! `VkShaderModule`'s SPIR-V forwards to [`Cmd::CreateShader`] with NO translation
//! ([`crate::adapter::spirv`]).

use crate::adapter::spirv;
use crate::model::descriptor::{DescriptorPoolRec, DsetRec, LayoutBinding, SetLayoutRec};
use crate::model::instance::Instance;
use crate::model::memory::{
    buffer_usage_from_vk, is_render_target, tex_format_from_vk, texture_usage_from_vk, BufferRec,
    ImageRec, MemRec, SamplerRec,
};
use crate::model::pipeline::{PipelineKind, PipelineLayoutRec, PipelineRec, ShaderRec};
use crate::model::queue::FenceRec;
use crate::*;
use hl_gpu::protocol::model::descriptor::{
    BufferDesc, ComputePipelineDesc, RenderPipelineDesc, SamplerDesc, ShaderRef, VertexLayout,
};
use hl_gpu::protocol::model::enums::{AddressMode, Filter, TextureFormat, Topology};
use hl_gpu::{Cmd, CommandSink, GpuError, Result};

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
        MemRec { data: vec![0u8; size as usize], size, bound_buffer: None, mapped: false },
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
    dev.memories.get_mut(&memory).unwrap().bound_buffer = Some(buffer);
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

/// `vkUnmapMemory` — clear the mapped flag (no further per-submit flush).
pub fn unmap_memory(dev: &mut Device, memory: VkDeviceMemory) {
    if let Some(m) = dev.memories.get_mut(&memory) {
        m.mapped = false;
    }
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
    dev.pipelines.insert(handle, PipelineRec { ir_id, kind: PipelineKind::Compute });
    Ok(handle)
}

/// `vkCreateGraphicsPipelines` (one pipeline) — resolve the vertex (+ optional fragment) stage(s), carry
/// the `VkPipelineVertexInputState` vertex-buffer layout(s), and submit [`Cmd::CreateRenderPipeline`]
/// with one color target of `color_format`. Ported from `pipeline.rs::vkCreateGraphicsPipelines` (the
/// bring-up subset: one color target, no blend/depth). `vertex_layouts` are the translated
/// `VkVertexInputBindingDescription`s (slot-0 layout is what the host rasterizer fetches positions from).
pub fn create_graphics_pipeline(
    dev: &mut Device,
    sink: &mut dyn CommandSink,
    vertex: (VkShaderModule, &str),
    fragment: Option<(VkShaderModule, &str)>,
    vertex_layouts: Vec<VertexLayout>,
    color_format: TextureFormat,
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
    let ir_id = dev.alloc_ir();
    let handle = dev.alloc_handle();
    sink.submit(&[Cmd::CreateRenderPipeline(
        ir_id,
        RenderPipelineDesc {
            vertex: vertex_ref,
            fragment: fragment_ref,
            vertex_buffers: vertex_layouts,
            color_targets: vec![ColorTargetState { format: color_format, blend: None, write_mask: 0xf }],
            depth: None,
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            label: format!("vkgpipe{ir_id}"),
        },
    )])?;
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
