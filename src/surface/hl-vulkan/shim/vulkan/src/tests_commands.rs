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
