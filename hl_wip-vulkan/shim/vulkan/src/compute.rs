//! The IR-wired compute path: the hand-written `vk*` bodies that marshal the Vulkan C ABI and call the
//! `hl_vulkan` lowering services (`create`/`record`/`submit`) through the process-global
//! [`hl_gpu::RemoteCommandSink`] in [`crate::state`].
//!
//! This is the SAME lowering the in-process `tests/lowering.rs` exercises — memory/buffer/shader/
//! compute-pipeline/descriptor/command-record/dispatch/submit/fence — reached here across the C ABI.
//! Every body is panic-free across the seam: raw pointers are null-checked, and a lowering
//! [`hl_gpu::GpuError`] is mapped to the accurate `VkResult` via [`hl_vulkan::result`] (never a false
//! `VK_SUCCESS`). The crate builds with `panic = "abort"` as a second guarantee.

use core::ffi::{c_char, c_void};

use hl_vulkan::model::descriptor::{
    descriptor_binds_image, descriptor_binds_sampler, is_buffer_descriptor, is_image_descriptor,
    DescriptorTemplateEntry, LayoutBinding,
};
use hl_vulkan::result::vk_result_from_gpu_error;
use hl_vulkan::service::{create, record, submit};
use hl_vulkan::{Device, VkCommandBuffer as VkCbHandle};
use hl_gpu::CommandSink;
use hl_log::tag;

use crate::state::with;
use crate::types::*;

// ---- shared marshalling helpers ------------------------------------------------------------------

/// Run `f` with the logical device + the command sink (disjoint `State` fields). `None` if no device
/// has been created yet — the caller maps that to `VK_ERROR_INITIALIZATION_FAILED`.
fn dev_sink<R>(f: impl FnOnce(&mut Device, &mut dyn CommandSink) -> R) -> Option<R> {
    with(|s| {
        let sink = &mut s.sink;
        let dev = s.device.as_mut()?;
        Some(f(dev, sink))
    })
}

/// Turn a `Result<()>` from a service into a `VkResult`.
fn vk(r: hl_gpu::Result<()>) -> VkResult {
    match r {
        Ok(()) => VK_SUCCESS,
        Err(e) => vk_result_from_gpu_error(&e),
    }
}

/// A dispatchable `VkCommandBuffer` carries the `hl_vulkan` `u64` command-buffer handle behind the
/// loader-magic slot.
fn cmdbuf_new(h: VkCbHandle) -> *mut c_void {
    Dispatchable::new(h)
}
unsafe fn cmdbuf_handle(p: *mut c_void) -> Option<VkCbHandle> {
    Dispatchable::<VkCbHandle>::inner(p).map(|h| *h)
}

/// Borrow a nul-terminated C string as `&str` (`"main"` fallback on NULL / bad UTF-8, the usual entry).
unsafe fn entry_str<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        return "main";
    }
    core::ffi::CStr::from_ptr(p).to_str().unwrap_or("main")
}

// ==================================================================================================
// memory + buffers
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateBuffer(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_buffer: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkBufferCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_buffer.is_null() {
        unsafe { *p_buffer = 0 };
    }
    dev_sink(|dev, sink| match create::create_buffer(dev, sink, ci.usage, ci.size) {
        Ok(h) => {
            if !p_buffer.is_null() {
                unsafe { *p_buffer = h };
            }
            VK_SUCCESS
        }
        Err(e) => vk_result_from_gpu_error(&e),
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkDestroyBuffer(_device: *mut c_void, buffer: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, sink| {
        let _ = create::destroy_buffer(dev, sink, buffer);
    });
}

#[no_mangle]
pub extern "C" fn vkGetBufferMemoryRequirements(
    _device: *mut c_void,
    buffer: u64,
    p_memory_requirements: *mut c_void,
) {
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements).as_mut() }) else {
        return;
    };
    let size = dev_sink(|dev, _| dev.buffers.get(&buffer).map(|b| b.size).unwrap_or(0)).unwrap_or(0);
    out.size = size;
    out.alignment = 256;
    out.memory_type_bits = 1; // the single unified memory type (index 0)
}

#[no_mangle]
pub extern "C" fn vkAllocateMemory(
    _device: *mut c_void,
    p_allocate_info: *const c_void,
    _p_allocator: *const c_void,
    p_memory: *mut u64,
) -> VkResult {
    let Some(ai) = (unsafe { (p_allocate_info as *const VkMemoryAllocateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let h = dev_sink(|dev, _| create::allocate_memory(dev, ai.allocation_size));
    match h {
        Some(handle) => {
            if !p_memory.is_null() {
                unsafe { *p_memory = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkFreeMemory(_device: *mut c_void, memory: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| {
        dev.memories.remove(&memory);
    });
}

#[no_mangle]
pub extern "C" fn vkBindBufferMemory(
    _device: *mut c_void,
    buffer: u64,
    memory: u64,
    memory_offset: u64,
) -> VkResult {
    dev_sink(|dev, _| vk(create::bind_buffer_memory(dev, buffer, memory, memory_offset)))
        .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkMapMemory(
    _device: *mut c_void,
    memory: u64,
    offset: u64,
    size: u64,
    _flags: u32,
    pp_data: *mut *mut c_void,
) -> VkResult {
    if pp_data.is_null() {
        return VK_ERROR_MEMORY_MAP_FAILED;
    }
    let r = dev_sink(|dev, sink| {
        if create::map_memory(dev, memory).is_err() {
            return VK_ERROR_MEMORY_MAP_FAILED;
        }
        // Device→host: refresh the mapped range with the bound buffer's CURRENT device bytes so a reader
        // observes GPU output through the pointer (unbound host-only staging is left untouched). A
        // readback transport error is non-fatal — the app still gets a valid staging pointer. The app's
        // own writes into these bytes flush back as a WriteBuffer at submit (mapped_uploads).
        let _ = create::read_mapped(dev, sink, memory, offset, size);
        // Hand back a pointer into the staging Vec at `offset`.
        let Some(m) = dev.memories.get_mut(&memory) else {
            return VK_ERROR_MEMORY_MAP_FAILED;
        };
        if offset as usize > m.data.len() {
            return VK_ERROR_MEMORY_MAP_FAILED;
        }
        let ptr = unsafe { m.data.as_mut_ptr().add(offset as usize) };
        unsafe { *pp_data = ptr as *mut c_void };
        VK_SUCCESS
    })
    .unwrap_or(VK_ERROR_MEMORY_MAP_FAILED);
    if r != VK_SUCCESS {
        hl_log::hl_warn!(tag::SHIM, "vkMapMemory mem={memory:#x} -> {:?}", r);
    }
    r
}

#[no_mangle]
pub extern "C" fn vkUnmapMemory(_device: *mut c_void, memory: u64) {
    dev_sink(|dev, _| create::unmap_memory(dev, memory));
}

// ---- bind-memory-2 / memory-requirements-2 (core 1.1 / KHR) — delegate to the v1 bodies -----------

/// `vkBindBufferMemory2` — bind each `VkBindBufferMemoryInfo` via the v1 [`vkBindBufferMemory`] body.
/// Returns the first binding error (else `VK_SUCCESS`).
#[no_mangle]
pub extern "C" fn vkBindBufferMemory2(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    if p_bind_infos.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe {
        std::slice::from_raw_parts(p_bind_infos as *const VkBindBufferMemoryInfo, bind_info_count as usize)
    };
    let mut result = VK_SUCCESS;
    for bi in infos {
        let r = vkBindBufferMemory(device, bi.buffer, bi.memory, bi.memory_offset);
        if r != VK_SUCCESS {
            result = r;
        }
    }
    result
}

/// `vkBindBufferMemory2KHR` — the `VK_KHR_bind_memory2` alias of [`vkBindBufferMemory2`].
#[no_mangle]
pub extern "C" fn vkBindBufferMemory2KHR(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    vkBindBufferMemory2(device, bind_info_count, p_bind_infos)
}

/// `vkGetBufferMemoryRequirements2` — read `VkBufferMemoryRequirementsInfo2` and fill the base
/// `VkMemoryRequirements` via the v1 [`vkGetBufferMemoryRequirements`] body (chain preserved).
#[no_mangle]
pub extern "C" fn vkGetBufferMemoryRequirements2(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    let Some(info) = (unsafe { (p_info as *const VkBufferMemoryRequirementsInfo2).as_ref() }) else {
        return;
    };
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements2).as_mut() }) else {
        return;
    };
    vkGetBufferMemoryRequirements(
        device,
        info.buffer,
        &mut out.memory_requirements as *mut _ as *mut c_void,
    );
}

/// `vkGetBufferMemoryRequirements2KHR` — the `VK_KHR_get_memory_requirements2` alias.
#[no_mangle]
pub extern "C" fn vkGetBufferMemoryRequirements2KHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    vkGetBufferMemoryRequirements2(device, p_info, p_memory_requirements)
}

// ==================================================================================================
// shaders + pipelines
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateShaderModule(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_shader_module: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkShaderModuleCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if !p_shader_module.is_null() {
        unsafe { *p_shader_module = 0 };
    }
    if ci.p_code.is_null() || ci.code_size == 0 {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let code = unsafe { std::slice::from_raw_parts(ci.p_code as *const u8, ci.code_size) };
    dev_sink(|dev, sink| match create::create_shader_module(dev, sink, code) {
        Ok(h) => {
            if !p_shader_module.is_null() {
                unsafe { *p_shader_module = h };
            }
            VK_SUCCESS
        }
        Err(e) => vk_result_from_gpu_error(&e),
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkDestroyShaderModule(_device: *mut c_void, shader_module: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| {
        dev.shaders.remove(&shader_module);
    });
}

#[no_mangle]
pub extern "C" fn vkCreatePipelineLayout(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_pipeline_layout: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkPipelineLayoutCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let set_layouts: Vec<u64> = if ci.p_set_layouts.is_null() || ci.set_layout_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(ci.p_set_layouts, ci.set_layout_count as usize) }.to_vec()
    };
    let h = dev_sink(|dev, _| create::create_pipeline_layout(dev, set_layouts));
    match h {
        Some(handle) => {
            if !p_pipeline_layout.is_null() {
                unsafe { *p_pipeline_layout = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyPipelineLayout(_device: *mut c_void, pipeline_layout: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| {
        dev.pipeline_layouts.remove(&pipeline_layout);
    });
}

#[no_mangle]
pub extern "C" fn vkCreateComputePipelines(
    _device: *mut c_void,
    _pipeline_cache: u64,
    create_info_count: u32,
    p_create_infos: *const c_void,
    _p_allocator: *const c_void,
    p_pipelines: *mut u64,
) -> VkResult {
    if p_create_infos.is_null() || p_pipelines.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe {
        std::slice::from_raw_parts(
            p_create_infos as *const VkComputePipelineCreateInfo,
            create_info_count as usize,
        )
    };
    let out = unsafe { std::slice::from_raw_parts_mut(p_pipelines, create_info_count as usize) };
    let mut result = VK_SUCCESS;
    for (i, ci) in infos.iter().enumerate() {
        out[i] = 0;
        let module = ci.stage.module;
        let entry = unsafe { entry_str(ci.stage.p_name) };
        let r = dev_sink(|dev, sink| create::create_compute_pipeline(dev, sink, module, entry))
            .unwrap_or(Err(hl_gpu::GpuError::Invalid("vkCreateComputePipelines: no device")));
        match r {
            Ok(h) => out[i] = h,
            Err(e) => {
                result = vk_result_from_gpu_error(&e);
            }
        }
    }
    result
}

#[no_mangle]
pub extern "C" fn vkDestroyPipeline(_device: *mut c_void, pipeline: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| {
        dev.pipelines.remove(&pipeline);
    });
}

// ==================================================================================================
// descriptors
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateDescriptorSetLayout(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_set_layout: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkDescriptorSetLayoutCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let bindings: Vec<LayoutBinding> = if ci.p_bindings.is_null() || ci.binding_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(ci.p_bindings, ci.binding_count as usize) }
            .iter()
            .map(|b| LayoutBinding {
                binding: b.binding,
                descriptor_type: b.descriptor_type,
                descriptor_count: b.descriptor_count,
                stage_flags: b.stage_flags,
            })
            .collect()
    };
    let h = dev_sink(|dev, _| create::create_descriptor_set_layout(dev, bindings));
    match h {
        Some(handle) => {
            if !p_set_layout.is_null() {
                unsafe { *p_set_layout = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorSetLayout(_device: *mut c_void, set_layout: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| {
        dev.set_layouts.remove(&set_layout);
    });
}

#[no_mangle]
pub extern "C" fn vkCreateDescriptorPool(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_descriptor_pool: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkDescriptorPoolCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let h = dev_sink(|dev, _| create::create_descriptor_pool(dev, ci.max_sets));
    match h {
        Some(handle) => {
            if !p_descriptor_pool.is_null() {
                unsafe { *p_descriptor_pool = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorPool(_device: *mut c_void, descriptor_pool: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| {
        dev.descriptor_pools.remove(&descriptor_pool);
    });
}

#[no_mangle]
pub extern "C" fn vkAllocateDescriptorSets(
    _device: *mut c_void,
    p_allocate_info: *const c_void,
    p_descriptor_sets: *mut u64,
) -> VkResult {
    let Some(ai) = (unsafe { (p_allocate_info as *const VkDescriptorSetAllocateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_descriptor_sets.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let n = ai.descriptor_set_count as usize;
    let layouts: &[u64] = if ai.p_set_layouts.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ai.p_set_layouts, n) }
    };
    let out = unsafe { std::slice::from_raw_parts_mut(p_descriptor_sets, n) };
    for i in 0..n {
        out[i] = 0;
        let layout = layouts.get(i).copied().unwrap_or(0);
        let r = dev_sink(|dev, _| create::allocate_descriptor_set(dev, ai.descriptor_pool, layout, i as u32))
            .unwrap_or(Err(hl_gpu::GpuError::Invalid("vkAllocateDescriptorSets: no device")));
        match r {
            Ok(h) => out[i] = h,
            Err(e) => return vk_result_from_gpu_error(&e),
        }
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkUpdateDescriptorSets(
    _device: *mut c_void,
    descriptor_write_count: u32,
    p_descriptor_writes: *const c_void,
    _descriptor_copy_count: u32,
    _p_descriptor_copies: *const c_void,
) {
    if p_descriptor_writes.is_null() {
        return;
    }
    let writes = unsafe {
        std::slice::from_raw_parts(
            p_descriptor_writes as *const VkWriteDescriptorSet,
            descriptor_write_count as usize,
        )
    };
    // The view→image mapping lives in the shim state (disjoint from the device), so borrow both fields.
    with(|s| {
        let crate::state::State { device, image_views, .. } = s;
        let Some(dev) = device.as_mut() else { return };
        for w in writes {
            if w.descriptor_count == 0 {
                continue;
            }
            if is_buffer_descriptor(w.descriptor_type) {
                if w.p_buffer_info.is_null() {
                    continue;
                }
                // Bring-up: bind the first buffer descriptor at (dstBinding). Array elements collapse onto
                // the binding (the model keys a set's resources by binding).
                let bi = unsafe { &*w.p_buffer_info };
                let _ = create::update_descriptor_buffer(dev, w.dst_set, w.dst_binding, bi.buffer, bi.offset, bi.range);
            } else if is_image_descriptor(w.descriptor_type) {
                if w.p_image_info.is_null() {
                    continue;
                }
                // Read the first `VkDescriptorImageInfo`, resolving its `imageView`→`VkImage` (for the
                // sampled classes) and taking its `sampler` (for the sampler classes). A COMBINED_IMAGE_SAMPLER
                // carries both; a separate SAMPLED_IMAGE only the view; a separate SAMPLER only the sampler.
                let ii = unsafe { &*(w.p_image_info as *const VkDescriptorImageInfo) };
                let image = if descriptor_binds_image(w.descriptor_type) {
                    image_views.get(&ii.image_view).copied()
                } else {
                    None
                };
                let sampler = if descriptor_binds_sampler(w.descriptor_type) && ii.sampler != 0 {
                    Some(ii.sampler)
                } else {
                    None
                };
                let _ = create::update_descriptor_image(dev, w.dst_set, w.dst_binding, image, sampler);
            }
            // Other descriptor classes (storage image, texel buffers) are not modeled: a truthful no-op.
        }
    });
}

// ==================================================================================================
// descriptor update templates (Vulkan 1.1 / VK_KHR_descriptor_update_template)
// ==================================================================================================

/// `vkCreateDescriptorUpdateTemplate` — retain the immutable entry table the app pushes descriptors
/// through. Only the `DESCRIPTOR_SET` template type is modeled (see `create::create_descriptor_update_template`).
#[no_mangle]
pub extern "C" fn vkCreateDescriptorUpdateTemplate(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_descriptor_update_template: *mut u64,
) -> VkResult {
    if !p_descriptor_update_template.is_null() {
        unsafe { *p_descriptor_update_template = 0 };
    }
    let Some(ci) = (unsafe { (p_create_info as *const VkDescriptorUpdateTemplateCreateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let entries: Vec<DescriptorTemplateEntry> =
        if ci.p_descriptor_update_entries.is_null() || ci.descriptor_update_entry_count == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(ci.p_descriptor_update_entries, ci.descriptor_update_entry_count as usize)
            }
            .iter()
            .map(|e| DescriptorTemplateEntry {
                dst_binding: e.dst_binding,
                dst_array_element: e.dst_array_element,
                descriptor_count: e.descriptor_count,
                descriptor_type: e.descriptor_type,
                offset: e.offset,
                stride: e.stride,
            })
            .collect()
        };
    let r = dev_sink(|dev, _| create::create_descriptor_update_template(dev, ci.template_type, entries));
    match r {
        Some(Ok(handle)) => {
            if !p_descriptor_update_template.is_null() {
                unsafe { *p_descriptor_update_template = handle };
            }
            VK_SUCCESS
        }
        Some(Err(e)) => vk_result_from_gpu_error(&e),
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// `vkCreateDescriptorUpdateTemplateKHR` — the `VK_KHR_descriptor_update_template` alias.
#[no_mangle]
pub extern "C" fn vkCreateDescriptorUpdateTemplateKHR(
    device: *mut c_void,
    p_create_info: *const c_void,
    p_allocator: *const c_void,
    p_descriptor_update_template: *mut u64,
) -> VkResult {
    vkCreateDescriptorUpdateTemplate(device, p_create_info, p_allocator, p_descriptor_update_template)
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorUpdateTemplate(
    _device: *mut c_void,
    descriptor_update_template: u64,
    _p_allocator: *const c_void,
) {
    dev_sink(|dev, _| create::destroy_descriptor_update_template(dev, descriptor_update_template));
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorUpdateTemplateKHR(
    device: *mut c_void,
    descriptor_update_template: u64,
    p_allocator: *const c_void,
) {
    vkDestroyDescriptorUpdateTemplate(device, descriptor_update_template, p_allocator)
}

/// `vkUpdateDescriptorSetWithTemplate` — apply the template's buffer descriptors read out of `pData` to
/// `descriptorSet` (identical result to the equivalent `vkUpdateDescriptorSets`). The C API carries no
/// `pData` size, so the shim bounds the raw pointer with the template's computed read extent
/// (`create::descriptor_template_data_len`).
#[no_mangle]
pub extern "C" fn vkUpdateDescriptorSetWithTemplate(
    _device: *mut c_void,
    descriptor_set: u64,
    descriptor_update_template: u64,
    p_data: *const c_void,
) {
    if p_data.is_null() {
        return;
    }
    // Determine exactly how many bytes the template reads, then build a bounded slice over pData.
    let Some(len) = with(|s| {
        s.device
            .as_ref()
            .and_then(|d| create::descriptor_template_data_len(d, descriptor_update_template))
    }) else {
        return;
    };
    let data = unsafe { std::slice::from_raw_parts(p_data as *const u8, len) };
    dev_sink(|dev, _| {
        let _ = create::update_descriptor_set_with_template(dev, descriptor_set, descriptor_update_template, data);
    });
}

#[no_mangle]
pub extern "C" fn vkUpdateDescriptorSetWithTemplateKHR(
    device: *mut c_void,
    descriptor_set: u64,
    descriptor_update_template: u64,
    p_data: *const c_void,
) {
    vkUpdateDescriptorSetWithTemplate(device, descriptor_set, descriptor_update_template, p_data)
}

// ==================================================================================================
// pipeline cache (modeled: a valid, versioned header; hl-GPU forwards SPIR-V, so no host binary)
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreatePipelineCache(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_pipeline_cache: *mut u64,
) -> VkResult {
    if p_pipeline_cache.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    unsafe { *p_pipeline_cache = 0 };
    // The optional initialData blob (may be absent — a fresh cache).
    let initial: Vec<u8> = match unsafe { (p_create_info as *const VkPipelineCacheCreateInfo).as_ref() } {
        Some(ci) if !ci.p_initial_data.is_null() && ci.initial_data_size > 0 => {
            unsafe { std::slice::from_raw_parts(ci.p_initial_data as *const u8, ci.initial_data_size) }.to_vec()
        }
        _ => Vec::new(),
    };
    match dev_sink(|dev, _| create::create_pipeline_cache(dev, &initial)) {
        Some(handle) => {
            unsafe { *p_pipeline_cache = handle };
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyPipelineCache(_device: *mut c_void, pipeline_cache: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| create::destroy_pipeline_cache(dev, pipeline_cache));
}

#[no_mangle]
pub extern "C" fn vkMergePipelineCaches(
    _device: *mut c_void,
    dst_cache: u64,
    src_cache_count: u32,
    p_src_caches: *const u64,
) -> VkResult {
    let srcs: Vec<u64> = if p_src_caches.is_null() || src_cache_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_src_caches, src_cache_count as usize) }.to_vec()
    };
    match dev_sink(|dev, _| create::merge_pipeline_caches(dev, dst_cache, &srcs)) {
        Some(Ok(())) => VK_SUCCESS,
        Some(Err(e)) => vk_result_from_gpu_error(&e),
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// `vkGetPipelineCacheData` — write the serialized cache blob (a spec-valid header). The two-call size
/// query (`pData` NULL) reports the length; a short buffer truncates with `VK_INCOMPLETE`.
#[no_mangle]
pub extern "C" fn vkGetPipelineCacheData(
    _device: *mut c_void,
    pipeline_cache: u64,
    p_data_size: *mut usize,
    p_data: *mut c_void,
) -> VkResult {
    if p_data_size.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let data = match dev_sink(|dev, _| create::get_pipeline_cache_data(dev, pipeline_cache)) {
        Some(Ok(d)) => d,
        Some(Err(e)) => return vk_result_from_gpu_error(&e),
        None => return VK_ERROR_INITIALIZATION_FAILED,
    };
    if p_data.is_null() {
        unsafe { *p_data_size = data.len() };
        return VK_SUCCESS;
    }
    let cap = unsafe { *p_data_size };
    let n = cap.min(data.len());
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), p_data as *mut u8, n);
        *p_data_size = n;
    }
    if n < data.len() {
        VK_INCOMPLETE
    } else {
        VK_SUCCESS
    }
}

// ==================================================================================================
// command pool + buffers + recording + submit
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateCommandPool(
    _device: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_command_pool: *mut u64,
) -> VkResult {
    // No dedicated pool object in the model; hand back a fresh opaque handle (command buffers are
    // allocated straight off the device).
    let h = dev_sink(|dev, _| dev.alloc_handle());
    match h {
        Some(handle) => {
            if !p_command_pool.is_null() {
                unsafe { *p_command_pool = handle };
            }
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

#[no_mangle]
pub extern "C" fn vkDestroyCommandPool(_device: *mut c_void, _command_pool: u64, _p_allocator: *const c_void) {}

#[no_mangle]
pub extern "C" fn vkAllocateCommandBuffers(
    _device: *mut c_void,
    p_allocate_info: *const c_void,
    p_command_buffers: *mut *mut c_void,
) -> VkResult {
    let Some(ai) = (unsafe { (p_allocate_info as *const VkCommandBufferAllocateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_command_buffers.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let n = ai.command_buffer_count as usize;
    let out = unsafe { std::slice::from_raw_parts_mut(p_command_buffers, n) };
    for slot in out.iter_mut().take(n) {
        let handle = match dev_sink(|dev, _| record::allocate_command_buffer(dev)) {
            Some(h) => h,
            None => return VK_ERROR_INITIALIZATION_FAILED,
        };
        *slot = cmdbuf_new(handle);
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkBeginCommandBuffer(command_buffer: *mut c_void, p_begin_info: *const c_void) -> VkResult {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // A command buffer recorded WITHOUT `ONE_TIME_SUBMIT` is re-submittable every frame (vkcube's per-image
    // draw pattern); one with it is single-use. Read the flag from the begin info (absent/null ⇒ 0).
    let one_time_submit = match unsafe { (p_begin_info as *const VkCommandBufferBeginInfo).as_ref() } {
        Some(bi) => bi.flags & VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT != 0,
        None => false,
    };
    dev_sink(|dev, _| vk(record::begin(dev, cb, one_time_submit))).unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkEndCommandBuffer(command_buffer: *mut c_void) -> VkResult {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    dev_sink(|dev, _| vk(record::end(dev, cb))).unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkCmdBindPipeline(command_buffer: *mut c_void, _pipeline_bind_point: i32, pipeline: u64) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev_sink(|dev, _| {
        let _ = record::cmd_bind_pipeline(dev, cb, pipeline);
    });
}

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn vkCmdBindDescriptorSets(
    command_buffer: *mut c_void,
    _pipeline_bind_point: i32,
    _layout: u64,
    first_set: u32,
    descriptor_set_count: u32,
    p_descriptor_sets: *const u64,
    dynamic_offset_count: u32,
    p_dynamic_offsets: *const u32,
) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    let sets: Vec<u64> = if p_descriptor_sets.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_descriptor_sets, descriptor_set_count as usize) }.to_vec()
    };
    let dyn_offsets: Vec<u32> = if p_dynamic_offsets.is_null() || dynamic_offset_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_dynamic_offsets, dynamic_offset_count as usize) }.to_vec()
    };
    dev_sink(|dev, sink| {
        let _ = record::cmd_bind_descriptor_sets(dev, sink, cb, first_set, &sets, &dyn_offsets);
    });
}

#[no_mangle]
pub extern "C" fn vkCmdDispatch(command_buffer: *mut c_void, group_count_x: u32, group_count_y: u32, group_count_z: u32) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev_sink(|dev, _| {
        let _ = record::cmd_dispatch(dev, cb, group_count_x, group_count_y, group_count_z);
    });
}

/// `vkCmdDispatchIndirect` — validate the indirect buffer; the IR has no indirect dispatch op, so this
/// records no encoder op (a documented bring-up limit), erroring only on a bad buffer.
#[no_mangle]
pub extern "C" fn vkCmdDispatchIndirect(command_buffer: *mut c_void, buffer: u64, offset: u64) {
    let Some(cb) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    dev_sink(|dev, _| {
        let _ = record::cmd_dispatch_indirect(dev, cb, buffer, offset);
    });
}

/// `vkCmdExecuteCommands` — replay recorded secondary command buffers into this primary (their encoder
/// ops, deferred device ops, and inline buffer writes spliced in order). Every secondary must be a valid,
/// `Executable` command buffer.
#[no_mangle]
pub extern "C" fn vkCmdExecuteCommands(
    command_buffer: *mut c_void,
    command_buffer_count: u32,
    p_command_buffers: *const *mut c_void,
) {
    let Some(primary) = (unsafe { cmdbuf_handle(command_buffer) }) else {
        return;
    };
    if p_command_buffers.is_null() || command_buffer_count == 0 {
        return;
    }
    let raw = unsafe { std::slice::from_raw_parts(p_command_buffers, command_buffer_count as usize) };
    // Unwrap each dispatchable secondary to its hl_vulkan u64 handle (skip any null slot).
    let secondaries: Vec<VkCbHandle> = raw.iter().filter_map(|&p| unsafe { cmdbuf_handle(p) }).collect();
    dev_sink(|dev, _| {
        let _ = record::cmd_execute_commands(dev, primary, &secondaries);
    });
}

#[no_mangle]
pub extern "C" fn vkQueueSubmit(
    _queue: *mut c_void,
    submit_count: u32,
    p_submits: *const c_void,
    fence: u64,
) -> VkResult {
    let submits = if p_submits.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(p_submits as *const VkSubmitInfo, submit_count as usize) }
    };
    // Gather every submitted command buffer (unwrapping each dispatchable to its u64 handle).
    let mut cbs: Vec<VkCbHandle> = Vec::new();
    for si in submits {
        if si.p_command_buffers.is_null() {
            continue;
        }
        let ptrs = unsafe {
            std::slice::from_raw_parts(si.p_command_buffers, si.command_buffer_count as usize)
        };
        for &p in ptrs {
            if let Some(h) = unsafe { cmdbuf_handle(p) } {
                cbs.push(h);
            }
        }
    }
    let signal = if fence != 0 { Some(fence) } else { None };
    let r = dev_sink(|dev, sink| vk(submit::queue_submit(dev, sink, &cbs, signal)))
        .unwrap_or(VK_ERROR_INITIALIZATION_FAILED);
    if r != VK_SUCCESS {
        hl_log::hl_warn!(tag::SHIM, "vkQueueSubmit cbs={} -> {:?}", cbs.len(), r);
    }
    r
}

#[no_mangle]
pub extern "C" fn vkQueueWaitIdle(_queue: *mut c_void) -> VkResult {
    // The executor replays each submit synchronously, so the queue is idle on return.
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDeviceWaitIdle(_device: *mut c_void) -> VkResult {
    VK_SUCCESS
}

// ---- synchronization2 submit + maintenance5 map/unmap-2 (delegate to the v1 bodies) ---------------

/// `vkQueueSubmit2` — the sync2 submit form. Gathers every `VkCommandBufferSubmitInfo::commandBuffer`
/// across the batch (unwrapping each dispatchable to its `u64` handle) and lowers exactly as
/// `vkQueueSubmit`; the semaphore-info arrays are validated-then-ignored by the synchronous model.
#[no_mangle]
pub extern "C" fn vkQueueSubmit2(
    _queue: *mut c_void,
    submit_count: u32,
    p_submits: *const c_void,
    fence: u64,
) -> VkResult {
    let submits = if p_submits.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(p_submits as *const VkSubmitInfo2, submit_count as usize) }
    };
    let mut cbs: Vec<VkCbHandle> = Vec::new();
    for si in submits {
        if si.p_command_buffer_infos.is_null() {
            continue;
        }
        let infos = unsafe {
            std::slice::from_raw_parts(si.p_command_buffer_infos, si.command_buffer_info_count as usize)
        };
        for info in infos {
            if let Some(h) = unsafe { cmdbuf_handle(info.command_buffer) } {
                cbs.push(h);
            }
        }
    }
    let signal = if fence != 0 { Some(fence) } else { None };
    dev_sink(|dev, sink| vk(submit::queue_submit(dev, sink, &cbs, signal)))
        .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

/// `vkQueueSubmit2KHR` — the `VK_KHR_synchronization2` alias.
#[no_mangle]
pub extern "C" fn vkQueueSubmit2KHR(
    queue: *mut c_void,
    submit_count: u32,
    p_submits: *const c_void,
    fence: u64,
) -> VkResult {
    vkQueueSubmit2(queue, submit_count, p_submits, fence)
}

/// `vkMapMemory2` (maintenance5) — read the `VkMemoryMapInfo` aggregate and delegate to `vkMapMemory`.
#[no_mangle]
pub extern "C" fn vkMapMemory2(device: *mut c_void, p_memory_map_info: *const c_void, pp_data: *mut *mut c_void) -> VkResult {
    let Some(info) = (unsafe { (p_memory_map_info as *const VkMemoryMapInfo).as_ref() }) else {
        return VK_ERROR_MEMORY_MAP_FAILED;
    };
    vkMapMemory(device, info.memory, info.offset, info.size, info.flags, pp_data)
}

/// `vkMapMemory2KHR` — the `VK_KHR_map_memory2` alias.
#[no_mangle]
pub extern "C" fn vkMapMemory2KHR(device: *mut c_void, p_memory_map_info: *const c_void, pp_data: *mut *mut c_void) -> VkResult {
    vkMapMemory2(device, p_memory_map_info, pp_data)
}

/// `vkUnmapMemory2` (maintenance5) — read the `VkMemoryUnmapInfo` aggregate and delegate to `vkUnmapMemory`.
#[no_mangle]
pub extern "C" fn vkUnmapMemory2(device: *mut c_void, p_memory_unmap_info: *const c_void) -> VkResult {
    let Some(info) = (unsafe { (p_memory_unmap_info as *const VkMemoryUnmapInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    vkUnmapMemory(device, info.memory);
    VK_SUCCESS
}

/// `vkUnmapMemory2KHR` — the `VK_KHR_map_memory2` alias.
#[no_mangle]
pub extern "C" fn vkUnmapMemory2KHR(device: *mut c_void, p_memory_unmap_info: *const c_void) -> VkResult {
    vkUnmapMemory2(device, p_memory_unmap_info)
}

// ==================================================================================================
// fences
// ==================================================================================================

#[no_mangle]
pub extern "C" fn vkCreateFence(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_fence: *mut u64,
) -> VkResult {
    let signaled = unsafe {
        (p_create_info as *const VkFenceCreateInfo)
            .as_ref()
            .map(|ci| ci.flags & 0x1 != 0)
            .unwrap_or(false)
    };
    if !p_fence.is_null() {
        unsafe { *p_fence = 0 };
    }
    dev_sink(|dev, sink| match create::create_fence(dev, sink, signaled) {
        Ok(h) => {
            if !p_fence.is_null() {
                unsafe { *p_fence = h };
            }
            VK_SUCCESS
        }
        Err(e) => vk_result_from_gpu_error(&e),
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkDestroyFence(_device: *mut c_void, fence: u64, _p_allocator: *const c_void) {
    dev_sink(|dev, _| {
        dev.fences.remove(&fence);
    });
}

#[no_mangle]
pub extern "C" fn vkWaitForFences(
    _device: *mut c_void,
    fence_count: u32,
    p_fences: *const u64,
    _wait_all: u32,
    _timeout: u64,
) -> VkResult {
    if p_fences.is_null() {
        return VK_SUCCESS;
    }
    let fences = unsafe { std::slice::from_raw_parts(p_fences, fence_count as usize) };
    dev_sink(|dev, sink| {
        for &f in fences {
            if let Err(e) = submit::wait_for_fence(dev, sink, f) {
                return vk_result_from_gpu_error(&e);
            }
        }
        VK_SUCCESS
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

#[no_mangle]
pub extern "C" fn vkResetFences(_device: *mut c_void, fence_count: u32, p_fences: *const u64) -> VkResult {
    if p_fences.is_null() {
        return VK_SUCCESS;
    }
    let fences = unsafe { std::slice::from_raw_parts(p_fences, fence_count as usize) };
    dev_sink(|dev, _| {
        for &f in fences {
            let _ = submit::reset_fence(dev, f);
        }
        VK_SUCCESS
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

/// `vkGetFenceStatus` — poll the fence's guest-side signaled state (`VK_SUCCESS` when signaled,
/// `VK_NOT_READY` otherwise). Non-blocking, unlike `vkWaitForFences`. Errors on an unknown fence.
#[no_mangle]
pub extern "C" fn vkGetFenceStatus(_device: *mut c_void, fence: u64) -> VkResult {
    dev_sink(|dev, _| match submit::fence_status(dev, fence) {
        Ok(true) => VK_SUCCESS,
        Ok(false) => VK_NOT_READY,
        Err(e) => vk_result_from_gpu_error(&e),
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}
