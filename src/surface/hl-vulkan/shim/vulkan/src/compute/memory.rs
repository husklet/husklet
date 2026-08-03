use super::*;
use hl_gpu::transport::adapter::unix::OpaqueResourceFd;

// ==================================================================================================
// memory + buffers
// ==================================================================================================

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
    ShimState::with_sink(
        |dev, sink| match create::create_buffer(dev, sink, ci.usage, ci.size) {
            Ok(h) => {
                if !p_buffer.is_null() {
                    unsafe { *p_buffer = h };
                }
                VK_SUCCESS
            }
            Err(e) => Status::from_error(&e),
        },
    )
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

pub extern "C" fn vkDestroyBuffer(_device: *mut c_void, buffer: u64, _p_allocator: *const c_void) {
    ShimState::with_sink(|dev, sink| {
        let _ = create::destroy_buffer(dev, sink, buffer);
    });
}

pub extern "C" fn vkGetBufferMemoryRequirements(
    _device: *mut c_void,
    buffer: u64,
    p_memory_requirements: *mut c_void,
) {
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements).as_mut() })
    else {
        return;
    };
    let size = ShimState::with_sink(|dev, _| dev.buffers.get(&buffer).map(|b| b.size).unwrap_or(0))
        .unwrap_or(0);
    out.size = size;
    out.alignment = 256;
    // Every advertised memory type can back this buffer (all our memory is host RAM): expose the full
    // set so gpu-alloc picks the type matching the buffer's usage. See PhysicalDeviceDesc::memory_types.
    out.memory_type_bits = StateStore::with(|s| s.physical_device().all_memory_type_bits());
}

pub extern "C" fn vkAllocateMemory(
    _device: *mut c_void,
    p_allocate_info: *const c_void,
    _p_allocator: *const c_void,
    p_memory: *mut u64,
) -> VkResult {
    let Some(ai) = (unsafe { (p_allocate_info as *const VkMemoryAllocateInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // `allocate_memory` is fallible: a zero/over-heap `allocationSize` surfaces as the honest VkResult
    // (`VK_ERROR_OUT_OF_DEVICE_MEMORY` for an over-budget request), never a fake success.
    let export_info =
        unsafe { ExtensionChain::find(ai.p_next, VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO) };
    let export_types = if export_info.is_null() {
        0
    } else {
        unsafe { (*(export_info as *const VkExportMemoryAllocateInfo)).handle_types }
    };
    if export_types & !VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT != 0 {
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    }
    match ShimState::with_sink(|dev, _| {
        let memory = dev.allocate_memory(ai.allocation_size)?;
        dev.set_memory_export_handle_types(memory, export_types)?;
        Ok(memory)
    }) {
        Some(Ok(handle)) => {
            if !p_memory.is_null() {
                unsafe { *p_memory = handle };
            }
            VK_SUCCESS
        }
        Some(Err(e)) => Status::from_error(&e),
        None => VK_ERROR_INITIALIZATION_FAILED,
    }
}

/// Export one whole, dedicated buffer allocation as a sealed guest-local opaque fd token.
pub extern "C" fn vkGetMemoryFdKHR(
    _device: *mut c_void,
    p_get_fd_info: *const c_void,
    p_fd: *mut c_void,
) -> VkResult {
    let (Some(info), Some(output)) = (
        unsafe { (p_get_fd_info as *const VkMemoryGetFdInfoKHR).as_ref() },
        unsafe { (p_fd as *mut i32).as_mut() },
    ) else {
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    };
    *output = -1;
    if info.handle_type != VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_FD_BIT {
        return VK_ERROR_INVALID_EXTERNAL_HANDLE;
    }
    let export = match ShimState::with_sink(|dev, sink| {
        create::export_memory_buffer(dev, sink, info.memory)
    }) {
        Some(Ok(export)) => export,
        Some(Err(error)) => return Status::from_error(&error),
        None => return VK_ERROR_INITIALIZATION_FAILED,
    };
    match OpaqueResourceFd::create(export) {
        Ok(token) => {
            *output = token.into_raw_fd();
            VK_SUCCESS
        }
        Err(_) => VK_ERROR_INVALID_EXTERNAL_HANDLE,
    }
}

/// Vulkan import is intentionally absent; opaque FDs exported by this driver are CUDA-import only.
pub extern "C" fn vkGetMemoryFdPropertiesKHR(
    _device: *mut c_void,
    _handle_type: i32,
    _fd: i32,
    p_properties: *mut c_void,
) -> VkResult {
    if let Some(properties) = unsafe { (p_properties as *mut VkMemoryFdPropertiesKHR).as_mut() } {
        properties.memory_type_bits = 0;
    }
    VK_ERROR_INVALID_EXTERNAL_HANDLE
}

pub extern "C" fn vkFreeMemory(_device: *mut c_void, memory: u64, _p_allocator: *const c_void) {
    ShimState::with_sink(|dev, _| {
        dev.memories.remove(&memory);
    });
}

pub extern "C" fn vkBindBufferMemory(
    _device: *mut c_void,
    buffer: u64,
    memory: u64,
    memory_offset: u64,
) -> VkResult {
    ShimState::with_sink(|dev, _| {
        ResultStatus::from_gpu(create::bind_buffer_memory(
            dev,
            buffer,
            memory,
            memory_offset,
        ))
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

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
    let r = ShimState::with_sink(|dev, sink| {
        if dev.map_memory(memory).is_err() {
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
        hl_log::hl_error!(hl_log::tag::SHIM, "vkMapMemory mem={memory:#x} -> {:?}", r);
    }
    r
}

pub extern "C" fn vkUnmapMemory(_device: *mut c_void, memory: u64) {
    ShimState::with_sink(|dev, _| dev.unmap_memory(memory));
}

// ---- bind-memory-2 / memory-requirements-2 (core 1.1 / KHR) — delegate to the v1 bodies -----------

/// `vkBindBufferMemory2` — bind each `VkBindBufferMemoryInfo` via the v1 [`vkBindBufferMemory`] body.
/// Returns the first binding error (else `VK_SUCCESS`).
pub extern "C" fn vkBindBufferMemory2(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    if p_bind_infos.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let infos = unsafe {
        std::slice::from_raw_parts(
            p_bind_infos as *const VkBindBufferMemoryInfo,
            bind_info_count as usize,
        )
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
pub extern "C" fn vkBindBufferMemory2KHR(
    device: *mut c_void,
    bind_info_count: u32,
    p_bind_infos: *const c_void,
) -> VkResult {
    vkBindBufferMemory2(device, bind_info_count, p_bind_infos)
}

/// `vkGetBufferMemoryRequirements2` — read `VkBufferMemoryRequirementsInfo2` and fill the base
/// `VkMemoryRequirements` via the v1 [`vkGetBufferMemoryRequirements`] body, then answer the chained outputs.
pub extern "C" fn vkGetBufferMemoryRequirements2(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    let Some(info) = (unsafe { (p_info as *const VkBufferMemoryRequirementsInfo2).as_ref() })
    else {
        return;
    };
    let Some(out) = (unsafe { (p_memory_requirements as *mut VkMemoryRequirements2).as_mut() })
    else {
        return;
    };
    vkGetBufferMemoryRequirements(
        device,
        info.buffer,
        &mut out.memory_requirements as *mut _ as *mut c_void,
    );
    // The chain is not "preserved" by leaving it alone: everything on it is an OUTPUT this driver owes
    // the caller, and skipping it returns the caller's uninitialised stack as an answer.
    out.answer_chain();
}

/// `vkGetBufferMemoryRequirements2KHR` — the `VK_KHR_get_memory_requirements2` alias.
pub extern "C" fn vkGetBufferMemoryRequirements2KHR(
    device: *mut c_void,
    p_info: *const c_void,
    p_memory_requirements: *mut c_void,
) {
    vkGetBufferMemoryRequirements2(device, p_info, p_memory_requirements)
}

/// `vkMapMemory2` reads the maintenance5 aggregate and delegates to `vkMapMemory`.
pub extern "C" fn vkMapMemory2(
    device: *mut c_void,
    p_memory_map_info: *const c_void,
    pp_data: *mut *mut c_void,
) -> VkResult {
    let Some(info) = (unsafe { (p_memory_map_info as *const VkMemoryMapInfo).as_ref() }) else {
        return VK_ERROR_MEMORY_MAP_FAILED;
    };
    vkMapMemory(
        device,
        info.memory,
        info.offset,
        info.size,
        info.flags,
        pp_data,
    )
}

/// `vkMapMemory2KHR` — the `VK_KHR_map_memory2` alias.
pub extern "C" fn vkMapMemory2KHR(
    device: *mut c_void,
    p_memory_map_info: *const c_void,
    pp_data: *mut *mut c_void,
) -> VkResult {
    vkMapMemory2(device, p_memory_map_info, pp_data)
}

/// `vkUnmapMemory2` (maintenance5) — read the `VkMemoryUnmapInfo` aggregate and delegate to `vkUnmapMemory`.
pub extern "C" fn vkUnmapMemory2(
    device: *mut c_void,
    p_memory_unmap_info: *const c_void,
) -> VkResult {
    let Some(info) = (unsafe { (p_memory_unmap_info as *const VkMemoryUnmapInfo).as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    vkUnmapMemory(device, info.memory);
    VK_SUCCESS
}

/// `vkUnmapMemory2KHR` — the `VK_KHR_map_memory2` alias.
pub extern "C" fn vkUnmapMemory2KHR(
    device: *mut c_void,
    p_memory_unmap_info: *const c_void,
) -> VkResult {
    vkUnmapMemory2(device, p_memory_unmap_info)
}
