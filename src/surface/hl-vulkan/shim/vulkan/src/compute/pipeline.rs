use super::*;

// ==================================================================================================
// shaders + pipelines
// ==================================================================================================

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
    ShimState::with_sink(
        |dev, sink| match create::create_shader_module(dev, sink, code) {
            Ok(h) => {
                if !p_shader_module.is_null() {
                    unsafe { *p_shader_module = h };
                }
                VK_SUCCESS
            }
            Err(e) => Status::from_error(&e),
        },
    )
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

pub extern "C" fn vkDestroyShaderModule(
    _device: *mut c_void,
    shader_module: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, sink| {
        let _ = create::destroy_shader_module(dev, sink, shader_module);
    });
}

pub extern "C" fn vkCreatePipelineLayout(
    _device: *mut c_void,
    p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_pipeline_layout: *mut u64,
) -> VkResult {
    let Some(ci) = (unsafe { (p_create_info as *const VkPipelineLayoutCreateInfo).as_ref() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let set_layouts: Vec<u64> = if ci.p_set_layouts.is_null() || ci.set_layout_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(ci.p_set_layouts, ci.set_layout_count as usize) }
            .to_vec()
    };
    let h = ShimState::with_sink(|dev, _| dev.create_pipeline_layout(set_layouts));
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

pub extern "C" fn vkDestroyPipelineLayout(
    _device: *mut c_void,
    pipeline_layout: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, _| {
        dev.pipeline_layouts.remove(&pipeline_layout);
    });
}

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
        let entry = unsafe { EntryPoint::read(ci.stage.p_name) };
        let r = ShimState::with_sink(|dev, sink| {
            create::create_compute_pipeline_with_layout(dev, sink, module, entry, Some(ci.layout))
        })
        .unwrap_or(Err(hl_gpu::GpuError::Invalid(
            "vkCreateComputePipelines: no device",
        )));
        match r {
            Ok(h) => out[i] = h,
            Err(e) => {
                result = Status::from_error(&e);
            }
        }
    }
    result
}

pub extern "C" fn vkDestroyPipeline(
    _device: *mut c_void,
    pipeline: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, sink| {
        let _ = create::destroy_pipeline(dev, sink, pipeline);
    });
}

// ==================================================================================================
// pipeline cache (modeled: a valid, versioned header; hl-GPU forwards SPIR-V, so no host binary)
// ==================================================================================================

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
    let initial: Vec<u8> =
        match unsafe { (p_create_info as *const VkPipelineCacheCreateInfo).as_ref() } {
            Some(ci) if !ci.p_initial_data.is_null() && ci.initial_data_size > 0 => unsafe {
                std::slice::from_raw_parts(ci.p_initial_data as *const u8, ci.initial_data_size)
            }
            .to_vec(),
            _ => Vec::new(),
        };
    match ShimState::with_sink(|dev, _| create::PipelineCache::create(dev, &initial)) {
        Some(handle) => {
            unsafe { *p_pipeline_cache = handle };
            VK_SUCCESS
        }
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

pub extern "C" fn vkDestroyPipelineCache(
    _device: *mut c_void,
    pipeline_cache: u64,
    _p_allocator: *const c_void,
) {
    ShimState::with_sink(|dev, _| create::PipelineCache::destroy(dev, pipeline_cache));
}

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
    match ShimState::with_sink(|dev, _| create::PipelineCache::merge(dev, dst_cache, &srcs)) {
        Some(Ok(())) => VK_SUCCESS,
        Some(Err(e)) => Status::from_error(&e),
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// `vkGetPipelineCacheData` — write the serialized cache blob (a spec-valid header). The two-call size
/// query (`pData` NULL) reports the length; a short buffer truncates with `VK_INCOMPLETE`.
pub extern "C" fn vkGetPipelineCacheData(
    _device: *mut c_void,
    pipeline_cache: u64,
    p_data_size: *mut usize,
    p_data: *mut c_void,
) -> VkResult {
    if p_data_size.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let data = match ShimState::with_sink(|dev, _| create::PipelineCache::data(dev, pipeline_cache))
    {
        Some(Ok(d)) => d,
        Some(Err(e)) => return Status::from_error(&e),
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
