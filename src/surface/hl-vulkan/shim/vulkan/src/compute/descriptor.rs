use super::*;

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
    let Some(ci) = (unsafe { (p_create_info as *const VkDescriptorSetLayoutCreateInfo).as_ref() })
    else {
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
    let h = ShimState::with_sink(|dev, _| dev.create_descriptor_set_layout(bindings));
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
pub extern "C" fn vkDestroyDescriptorSetLayout(
    _device: *mut c_void,
    set_layout: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, _| {
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
    let Some(ci) = (unsafe { (p_create_info as *const VkDescriptorPoolCreateInfo).as_ref() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let h = ShimState::with_sink(|dev, _| dev.create_descriptor_pool(ci.max_sets));
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
pub extern "C" fn vkDestroyDescriptorPool(
    _device: *mut c_void,
    descriptor_pool: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, _| {
        dev.descriptor_pools.remove(&descriptor_pool);
    });
}

#[no_mangle]
pub extern "C" fn vkAllocateDescriptorSets(
    _device: *mut c_void,
    p_allocate_info: *const c_void,
    p_descriptor_sets: *mut u64,
) -> VkResult {
    let Some(ai) = (unsafe { (p_allocate_info as *const VkDescriptorSetAllocateInfo).as_ref() })
    else {
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
        let r = ShimState::with_sink(|dev, _| {
            create::allocate_descriptor_set(dev, ai.descriptor_pool, layout, i as u32)
        })
        .unwrap_or(Err(hl_gpu::GpuError::Invalid(
            "vkAllocateDescriptorSets: no device",
        )));
        match r {
            Ok(h) => out[i] = h,
            // Pool exhaustion is the only `ResourceLimit` this path produces, and its spec code is
            // `VK_ERROR_OUT_OF_POOL_MEMORY` (which `allocate_descriptor_set`'s own doc comment names) — an
            // allocator like wgpu's RECOVERS from it by growing/adding a descriptor pool and retrying. The
            // generic error map instead returns `VK_ERROR_OUT_OF_DEVICE_MEMORY`, a FATAL device-OOM that
            // would make wgpu lose the device the moment a pool fills. Emit the correct, recoverable code.
            Err(hl_gpu::GpuError::ResourceLimit(_)) => return VK_ERROR_OUT_OF_POOL_MEMORY,
            Err(e) => return Status::from_error(&e),
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
    StateStore::with(|s| {
        let crate::state::State {
            device,
            image_views,
            ..
        } = s;
        let Some(dev) = device.as_mut() else { return };
        for w in writes {
            if w.descriptor_count == 0 {
                continue;
            }
            let descriptor_type = DescriptorType::from(w.descriptor_type);
            if descriptor_type.is_buffer() {
                if w.p_buffer_info.is_null() {
                    continue;
                }
                // Bring-up: bind the first buffer descriptor at (dstBinding). Array elements collapse onto
                // the binding (the model keys a set's resources by binding).
                let bi = unsafe { &*w.p_buffer_info };
                let _ = create::update_descriptor_buffer(
                    dev,
                    w.dst_set,
                    w.dst_binding,
                    bi.buffer,
                    bi.offset,
                    bi.range,
                );
            } else if descriptor_type.is_image() {
                if w.p_image_info.is_null() {
                    continue;
                }
                // Read the first `VkDescriptorImageInfo`, resolving its `imageView`→`VkImage` (for the
                // sampled classes) and taking its `sampler` (for the sampler classes). A COMBINED_IMAGE_SAMPLER
                // carries both; a separate SAMPLED_IMAGE only the view; a separate SAMPLER only the sampler.
                let ii = unsafe { &*(w.p_image_info as *const VkDescriptorImageInfo) };
                let image = if descriptor_type.binds_image() {
                    image_views.get(&ii.image_view).copied()
                } else {
                    None
                };
                let sampler = if descriptor_type.binds_sampler() && ii.sampler != 0 {
                    Some(ii.sampler)
                } else {
                    None
                };
                let _ =
                    create::update_descriptor_image(dev, w.dst_set, w.dst_binding, image, sampler);
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
    let Some(ci) =
        (unsafe { (p_create_info as *const VkDescriptorUpdateTemplateCreateInfo).as_ref() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let entries: Vec<DescriptorTemplateEntry> =
        if ci.p_descriptor_update_entries.is_null() || ci.descriptor_update_entry_count == 0 {
            Vec::new()
        } else {
            unsafe {
                std::slice::from_raw_parts(
                    ci.p_descriptor_update_entries,
                    ci.descriptor_update_entry_count as usize,
                )
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
    let r = ShimState::with_sink(|dev, _| {
        create::create_descriptor_update_template(dev, ci.template_type, entries)
    });
    match r {
        Some(Ok(handle)) => {
            if !p_descriptor_update_template.is_null() {
                unsafe { *p_descriptor_update_template = handle };
            }
            VK_SUCCESS
        }
        Some(Err(e)) => Status::from_error(&e),
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
    vkCreateDescriptorUpdateTemplate(
        device,
        p_create_info,
        p_allocator,
        p_descriptor_update_template,
    )
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorUpdateTemplate(
    _device: *mut c_void,
    descriptor_update_template: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, _| {
        dev.destroy_descriptor_update_template(descriptor_update_template)
    });
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
    let Some(len) = StateStore::with(|s| {
        s.device
            .as_ref()
            .and_then(|d| d.descriptor_template_data_len(descriptor_update_template))
    }) else {
        return;
    };
    let data = unsafe { std::slice::from_raw_parts(p_data as *const u8, len) };
    ShimState::with_sink(|dev, _| {
        let _ = create::update_descriptor_set_with_template(
            dev,
            descriptor_set,
            descriptor_update_template,
            data,
        );
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
