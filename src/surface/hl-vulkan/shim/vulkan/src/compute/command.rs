use super::*;

// ==================================================================================================
// command pool + buffers + recording + submit
// ==================================================================================================
pub extern "C" fn vkCreateCommandPool(
    _device: *mut c_void,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_command_pool: *mut u64,
) -> VkResult {
    // No dedicated pool object in the model; hand back a fresh opaque handle (command buffers are
    // allocated straight off the device).
    let h = ShimState::with_sink(|dev, _| dev.alloc_handle());
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

pub extern "C" fn vkDestroyCommandPool(
    _device: *mut c_void,
    _command_pool: u64,
    _p_allocator: *const c_void,
) {
}

pub extern "C" fn vkAllocateCommandBuffers(
    _device: *mut c_void,
    p_allocate_info: *const c_void,
    p_command_buffers: *mut *mut c_void,
) -> VkResult {
    let Some(ai) = (unsafe { (p_allocate_info as *const VkCommandBufferAllocateInfo).as_ref() })
    else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_command_buffers.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let n = ai.command_buffer_count as usize;
    let out = unsafe { std::slice::from_raw_parts_mut(p_command_buffers, n) };
    for slot in out.iter_mut().take(n) {
        let handle = match ShimState::with_sink(|dev, _| dev.allocate_command_buffer()) {
            Some(h) => h,
            None => return VK_ERROR_INITIALIZATION_FAILED,
        };
        *slot = CommandBuffer::from_handle(handle);
    }
    VK_SUCCESS
}

pub extern "C" fn vkBeginCommandBuffer(
    command_buffer: *mut c_void,
    p_begin_info: *const c_void,
) -> VkResult {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // A command buffer recorded WITHOUT `ONE_TIME_SUBMIT` is re-submittable every frame (vkcube's per-image
    // draw pattern); one with it is single-use. Read the flag from the begin info (absent/null ⇒ 0).
    let one_time_submit =
        match unsafe { (p_begin_info as *const VkCommandBufferBeginInfo).as_ref() } {
            Some(bi) => bi.flags & VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT != 0,
            None => false,
        };
    // A push-descriptor set does not survive re-recording (`VK_KHR_push_descriptor` / core 1.4).
    super::push_descriptor::PushSet::forget(cb);
    ShimState::with_sink(|dev, _| {
        ResultStatus::from_gpu(dev.begin_command_buffer(cb, one_time_submit))
    })
    .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

pub extern "C" fn vkEndCommandBuffer(command_buffer: *mut c_void) -> VkResult {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    ShimState::with_sink(|dev, _| ResultStatus::from_gpu(dev.end_command_buffer(cb)))
        .unwrap_or(VK_ERROR_INITIALIZATION_FAILED)
}

pub extern "C" fn vkCmdBindPipeline(
    command_buffer: *mut c_void,
    _pipeline_bind_point: i32,
    pipeline: u64,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_sink(|dev, _| {
        let _ = record::cmd_bind_pipeline(dev, cb, pipeline);
    });
}

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
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    let sets: Vec<u64> = if p_descriptor_sets.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_descriptor_sets, descriptor_set_count as usize) }
            .to_vec()
    };
    let dyn_offsets: Vec<u32> = if p_dynamic_offsets.is_null() || dynamic_offset_count == 0 {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(p_dynamic_offsets, dynamic_offset_count as usize) }
            .to_vec()
    };
    ShimState::with_sink(|dev, sink| {
        let _ = record::cmd_bind_descriptor_sets(dev, sink, cb, first_set, &sets, &dyn_offsets);
    });
}

pub extern "C" fn vkCmdDispatch(
    command_buffer: *mut c_void,
    group_count_x: u32,
    group_count_y: u32,
    group_count_z: u32,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_sink(|dev, _| {
        let _ = record::cmd_dispatch(dev, cb, group_count_x, group_count_y, group_count_z);
    });
}

/// `vkCmdDispatchIndirect` — validate the indirect buffer, read its `VkDispatchIndirectCommand{x,y,z}`
/// workgroup counts out of the host-visible backing, and lower to the same compute-pass `Dispatch{x,y,z}`
/// the equivalent `vkCmdDispatch` would emit; erroring only on a bad buffer.
pub extern "C" fn vkCmdDispatchIndirect(command_buffer: *mut c_void, buffer: u64, offset: u64) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    ShimState::with_sink(|dev, _| {
        let _ = record::cmd_dispatch_indirect(dev, cb, buffer, offset);
    });
}

/// `vkCmdExecuteCommands` — replay recorded secondary command buffers into this primary (their encoder
/// ops, deferred device ops, and inline buffer writes spliced in order). Every secondary must be a valid,
/// `Executable` command buffer.
pub extern "C" fn vkCmdExecuteCommands(
    command_buffer: *mut c_void,
    command_buffer_count: u32,
    p_command_buffers: *const *mut c_void,
) {
    let Some(primary) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_command_buffers.is_null() || command_buffer_count == 0 {
        return;
    }
    let raw =
        unsafe { std::slice::from_raw_parts(p_command_buffers, command_buffer_count as usize) };
    // Unwrap each dispatchable secondary to its hl_vulkan u64 handle (skip any null slot).
    let secondaries: Vec<VkCbHandle> = raw
        .iter()
        .filter_map(|&p| unsafe { CommandBuffer::handle(p) })
        .collect();
    ShimState::with_sink(|dev, _| {
        let _ = record::cmd_execute_commands(dev, primary, &secondaries);
    });
}
