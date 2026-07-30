//! Converted dynamic-state, address, debug, and device-group command behavior.

use super::*;

/// A single-field `Vk*Info` head whose only field after `pNext` is a `u64` handle.
#[repr(C)]
struct HandleInfo {
    s_type: i32,
    _pad: u32,
    p_next: *const c_void,
    handle: u64,
}

#[test]
fn extended_dynamic_state_is_recorded() {
    let _g = test_guard();
    let (cb, handle) = recording_command_buffer();
    crate::dynstate::vkCmdSetCullMode(cb, 2);
    crate::dynstate::vkCmdSetFrontFace(cb, 1);
    crate::dynstate::vkCmdSetPrimitiveTopology(cb, 3);
    crate::dynstate::vkCmdSetDepthTestEnable(cb, 1);
    crate::dynstate::vkCmdSetDepthWriteEnable(cb, 1);
    crate::dynstate::vkCmdSetRasterizerDiscardEnable(cb, 1);
    crate::dynstate::vkCmdSetStencilOp(cb, 0x1, 4, 5, 6, 7);
    crate::dynstate::vkCmdSetRasterizationSamplesEXT(cb, 4);
    crate::dynstate::vkCmdSetLogicOpEnableEXT(cb, 1);
    let enables: [u32; 2] = [1, 0];
    crate::dynstate::vkCmdSetColorBlendEnableEXT(cb, 0, 2, enables.as_ptr() as *const c_void);

    let ds = crate::state::StateStore::with(|s| {
        s.device
            .as_ref()
            .unwrap()
            .command_buffers
            .get(&handle)
            .unwrap()
            .dynamic
            .clone()
    });
    assert_eq!(ds.cull_mode, 2);
    assert_eq!(ds.front_face, 1);
    assert_eq!(ds.primitive_topology, 3);
    assert!(ds.depth_test_enable);
    assert!(ds.depth_write_enable);
    assert!(ds.rasterizer_discard_enable);
    assert_eq!(ds.stencil_op_front, (4, 5, 6, 7));
    assert_eq!(ds.stencil_op_back, (0, 0, 0, 0));
    assert_eq!(ds.rasterization_samples, 4);
    assert!(ds.logic_op_enable);
    assert_eq!(ds.color_blend_enables, vec![1, 0]);
}

#[test]
fn viewport_with_count_and_bind_vertex_buffers2_lower_to_ir() {
    let _g = test_guard();
    let (cb, handle) = recording_command_buffer();
    let viewports = [VkViewport {
        x: 0.0,
        y: 0.0,
        width: 64.0,
        height: 48.0,
        min_depth: 0.0,
        max_depth: 1.0,
    }];
    crate::dynstate::vkCmdSetViewportWithCount(cb, 1, viewports.as_ptr() as *const c_void);
    let count = crate::state::StateStore::with(|s| {
        use hl_gpu::protocol::model::command::Enc;
        s.device
            .as_ref()
            .unwrap()
            .command_buffers
            .get(&handle)
            .unwrap()
            .enc
            .iter()
            .filter(|entry| matches!(entry, Enc::SetViewport { .. }))
            .count()
    });
    assert_eq!(count, 1, "vkCmdSetViewportWithCount must record a viewport");
}

#[test]
fn dispatch_base_lowers_to_dispatch() {
    let _g = test_guard();
    let (cb, handle) = recording_command_buffer();
    crate::devgroup::vkCmdDispatchBase(cb, 0, 0, 0, 4, 5, 6);
    let dispatched = crate::state::StateStore::with(|s| {
        use hl_gpu::protocol::model::command::Enc;
        s.device
            .as_ref()
            .unwrap()
            .command_buffers
            .get(&handle)
            .unwrap()
            .enc
            .iter()
            .any(|entry| matches!(entry, Enc::Dispatch { x: 4, y: 5, z: 6 }))
    });
    assert!(dispatched, "vkCmdDispatchBase must record the group counts");
    crate::devgroup::vkCmdSetDeviceMask(cb, 1);
}

#[test]
fn buffer_device_address_is_stable_nonzero_and_distinct_per_buffer() {
    let _g = test_guard();
    let mut device: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut device,
        ),
        VK_SUCCESS
    );
    let (first, second) = crate::state::StateStore::with(|state| {
        use hl_vulkan::model::memory::BufferRec;
        let device = state.device.as_mut().unwrap();
        let insert = |device: &mut hl_vulkan::Device, size: u64| {
            let handle = device.alloc_handle();
            let ir_id = device.alloc_ir();
            device.buffers.insert(
                handle,
                BufferRec {
                    ir_id,
                    size,
                    usage: 0,
                    bound_mem: None,
                    bound_offset: 0,
                },
            );
            handle
        };
        (insert(device, 1024), insert(device, 2048))
    });
    let address = |handle: u64| {
        let info = HandleInfo {
            s_type: 0,
            _pad: 0,
            p_next: core::ptr::null(),
            handle,
        };
        crate::address::vkGetBufferDeviceAddress(device, &info as *const _ as *const c_void)
    };
    let first_address = address(first);
    let second_address = address(second);
    assert_ne!(first_address, 0);
    assert_ne!(second_address, 0);
    assert_ne!(first_address, second_address);
    assert_eq!(first_address, address(first));
    let info = HandleInfo {
        s_type: 0,
        _pad: 0,
        p_next: core::ptr::null(),
        handle: first,
    };
    assert_eq!(
        crate::address::vkGetBufferDeviceAddressKHR(device, &info as *const _ as *const c_void),
        first_address
    );
    assert_eq!(
        crate::address::vkGetBufferDeviceAddressEXT(device, &info as *const _ as *const c_void),
        first_address
    );
    assert_eq!(address(0xDEAD_BEEF), 0);
}

#[test]
fn debug_utils_object_name_is_stored() {
    let _g = test_guard();
    let name = std::ffi::CString::new("my-object").unwrap();
    #[repr(C)]
    struct NameInfo {
        s_type: i32,
        _pad0: u32,
        p_next: *const c_void,
        object_type: i32,
        _pad1: u32,
        object_handle: u64,
        p_object_name: *const core::ffi::c_char,
    }
    let info = NameInfo {
        s_type: 0,
        _pad0: 0,
        p_next: core::ptr::null(),
        object_type: 9,
        _pad1: 0,
        object_handle: 0xABCD,
        p_object_name: name.as_ptr(),
    };
    assert_eq!(
        crate::debug::vkSetDebugUtilsObjectNameEXT(
            core::ptr::null_mut(),
            &info as *const _ as *const c_void,
        ),
        VK_SUCCESS
    );
    let stored =
        crate::state::StateStore::with(|state| state.debug_object_names.get(&(9, 0xABCD)).cloned());
    assert_eq!(stored.as_deref(), Some("my-object"));
    let mut messenger = 0;
    assert_eq!(
        crate::debug::vkCreateDebugUtilsMessengerEXT(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut messenger as *mut u64 as *mut c_void,
        ),
        VK_SUCCESS
    );
    assert_ne!(messenger, 0);
    crate::debug::vkDestroyDebugUtilsMessengerEXT(
        core::ptr::null_mut(),
        messenger,
        core::ptr::null(),
    );
}

#[test]
fn external_buffer_properties_report_no_handle_types() {
    let mut properties = [0u64; 8];
    properties[2] = u64::MAX;
    crate::devgroup::vkGetPhysicalDeviceExternalBufferProperties(
        core::ptr::null_mut(),
        core::ptr::null(),
        properties.as_mut_ptr() as *mut c_void,
    );
    assert_eq!(properties[2], 0);
}

/// `vkCmdPushDescriptorSet` (core Vulkan 1.4, promoted from `VK_KHR_push_descriptor`) must apply the write
/// where the bind path reads it. It was a silent `void` no-op, so the descriptor vanished and the draw read
/// whatever was bound before — with no error possible, because the command returns nothing.
#[test]
fn push_descriptor_set_applies_its_write_to_the_bound_set() {
    let _g = test_guard();
    let (cb, cb_handle) = recording_command_buffer();
    let device = core::ptr::null_mut();

    let binding = VkDescriptorSetLayoutBinding {
        binding: 0,
        descriptor_type: hl_vulkan::model::descriptor::vk_descriptor_type::UNIFORM_BUFFER,
        descriptor_count: 1,
        stage_flags: 0,
        p_immutable_samplers: core::ptr::null(),
    };
    let layout_ci = VkDescriptorSetLayoutCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        binding_count: 1,
        p_bindings: &binding,
    };
    let mut set_layout: u64 = 0;
    assert_eq!(
        crate::compute::vkCreateDescriptorSetLayout(
            device,
            &layout_ci as *const _ as *const c_void,
            core::ptr::null(),
            &mut set_layout,
        ),
        VK_SUCCESS
    );
    let pipeline_ci = VkPipelineLayoutCreateInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        flags: 0,
        set_layout_count: 1,
        p_set_layouts: &set_layout,
        push_constant_range_count: 0,
        p_push_constant_ranges: core::ptr::null(),
    };
    let mut pipeline_layout: u64 = 0;
    assert_eq!(
        crate::compute::vkCreatePipelineLayout(
            device,
            &pipeline_ci as *const _ as *const c_void,
            core::ptr::null(),
            &mut pipeline_layout,
        ),
        VK_SUCCESS
    );
    // The descriptor write records a `(buffer, offset, range)` triple; the bind path resolves the handle
    // later, so no host GPU connection is needed to observe the write landing.
    let buffer: u64 = 0x1000;

    let buffer_info = VkDescriptorBufferInfo {
        buffer,
        offset: 64,
        range: 128,
    };
    let write = VkWriteDescriptorSet {
        s_type: 0,
        p_next: core::ptr::null(),
        // `dstSet` is ignored by a push, so a deliberately bogus one must not reach the model.
        dst_set: 0xDEAD_BEEF,
        dst_binding: 0,
        dst_array_element: 0,
        descriptor_count: 1,
        descriptor_type: hl_vulkan::model::descriptor::vk_descriptor_type::UNIFORM_BUFFER,
        p_image_info: core::ptr::null(),
        p_buffer_info: &buffer_info,
        p_texel_buffer_view: core::ptr::null(),
    };
    crate::compute::vkCmdPushDescriptorSet(
        cb,
        0,
        pipeline_layout,
        0,
        1,
        &write as *const _ as *const c_void,
    );

    let pushed = crate::state::StateStore::with(|state| {
        state.push_descriptor_sets.get(&(cb_handle, 0)).copied()
    })
    .expect("a push must mint a set for (command buffer, set 0)");
    let descriptor = crate::state::StateStore::with(|state| {
        state
            .device
            .as_ref()
            .unwrap()
            .descriptor_sets
            .get(&pushed)
            .unwrap()
            .buffers
            .get(&(0, 0))
            .copied()
    });
    assert_eq!(descriptor, Some((buffer, 64, 128)));

    // A second push accumulates into the SAME set, as the spec requires.
    let second = VkDescriptorBufferInfo {
        buffer,
        offset: 0,
        range: 32,
    };
    let write = VkWriteDescriptorSet {
        p_buffer_info: &second,
        ..write
    };
    crate::compute::vkCmdPushDescriptorSet(
        cb,
        0,
        pipeline_layout,
        0,
        1,
        &write as *const _ as *const c_void,
    );
    let (again, descriptor) = crate::state::StateStore::with(|state| {
        let again = state.push_descriptor_sets.get(&(cb_handle, 0)).copied();
        let descriptor = state
            .device
            .as_ref()
            .unwrap()
            .descriptor_sets
            .get(&pushed)
            .unwrap()
            .buffers
            .get(&(0, 0))
            .copied();
        (again, descriptor)
    });
    assert_eq!(again, Some(pushed));
    assert_eq!(descriptor, Some((buffer, 0, 32)));

    // Re-recording forgets the pushed set: push-descriptor state does not survive a begin.
    assert_eq!(
        crate::compute::vkBeginCommandBuffer(cb, core::ptr::null()),
        VK_SUCCESS
    );
    assert_eq!(
        crate::state::StateStore::with(|state| state
            .push_descriptor_sets
            .get(&(cb_handle, 0))
            .copied()),
        None
    );
}

/// `vkAcquireNextImage2KHR` belongs to `VK_KHR_swapchain`, which this driver advertises, so it must behave
/// as `vkAcquireNextImageKHR` does. It was a stub returning `VK_ERROR_EXTENSION_NOT_PRESENT` for an
/// extension that IS present. The two forms must agree exactly, including on an unknown swapchain.
#[test]
fn acquire_next_image2_reports_an_unknown_swapchain_like_the_positional_form() {
    let _g = test_guard();
    let mut device: *mut c_void = core::ptr::null_mut();
    assert_eq!(
        crate::device::vkCreateDevice(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut device,
        ),
        VK_SUCCESS
    );
    let info = crate::graphics::VkAcquireNextImageInfoKHR {
        s_type: 0,
        p_next: core::ptr::null(),
        swapchain: 0x1234,
        timeout: 0,
        semaphore: 0,
        fence: 0,
        device_mask: 1,
    };
    let mut index: u32 = 7;
    let mut positional: u32 = 7;

    let structured = crate::graphics::vkAcquireNextImage2KHR(
        device,
        &info as *const _ as *const c_void,
        &mut index,
    );

    assert_eq!(
        structured,
        crate::graphics::vkAcquireNextImageKHR(device, 0x1234, 0, 0, 0, &mut positional)
    );
    assert_ne!(structured, VK_SUCCESS);
    assert_eq!(index, 7);
}
