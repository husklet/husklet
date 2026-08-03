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
        s.device_ref()
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
        s.device_ref()
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
        s.device_ref()
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
        let device = state.device_mut().unwrap();
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

#[test]
fn external_semaphore_properties_are_exactly_opaque_fd() {
    let info = VkPhysicalDeviceExternalSemaphoreInfo {
        s_type: 0,
        p_next: core::ptr::null(),
        handle_type: VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT,
    };
    let mut properties = VkExternalSemaphoreProperties {
        s_type: 0,
        p_next: core::ptr::null_mut(),
        export_from_imported_handle_types: u32::MAX,
        compatible_handle_types: u32::MAX,
        external_semaphore_features: u32::MAX,
    };
    crate::devgroup::vkGetPhysicalDeviceExternalSemaphoreProperties(
        core::ptr::null_mut(),
        &info as *const _ as *const c_void,
        &mut properties as *mut _ as *mut c_void,
    );
    assert_eq!(properties.export_from_imported_handle_types, 1);
    assert_eq!(properties.compatible_handle_types, 1);
    assert_eq!(properties.external_semaphore_features, 3);

    let unsupported = VkPhysicalDeviceExternalSemaphoreInfo {
        handle_type: 2,
        ..info
    };
    crate::devgroup::vkGetPhysicalDeviceExternalSemaphorePropertiesKHR(
        core::ptr::null_mut(),
        &unsupported as *const _ as *const c_void,
        &mut properties as *mut _ as *mut c_void,
    );
    assert_eq!(properties.export_from_imported_handle_types, 0);
    assert_eq!(properties.compatible_handle_types, 0);
    assert_eq!(properties.external_semaphore_features, 0);
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
            .device_ref()
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
            .device_ref()
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

/// A MIRRORED `vkCmdBlitImage` region is NORMALIZED and its flip is kept; an EMPTY one is still skipped.
///
/// Vulkan expresses a flipped blit by putting `offsets[1]` before `offsets[0]` on an axis, and it is
/// legal. The IR's origin and extent are unsigned, so this shim has to normalize the two corners with a
/// min/max before it has a rect at all — and the comparison that normalization performs IS the flip. It
/// used to be discarded: first by the same `continue` that skips an empty region (the command produced
/// nothing, reported nothing, and left the destination stale), then by an honest but unnecessary
/// `Unsupported` refusal. `BlitRect` keeps it, and `Mirror::net` combines the two sides.
///
/// The EMPTY region is the control, and it is what makes this discriminating: an empty region is
/// genuinely nothing to do, and a `BlitRect` that returned a rect for it would turn every application
/// that submits one into a zero-extent blit the host refuses.
///
/// The NET rule is the other half. Inverting BOTH sides of an axis mirrors twice and is the identity, so
/// a driver that took the source's inversion alone would flip images no application asked to flip.
#[test]
fn a_mirrored_blit_region_keeps_its_flip_and_an_empty_one_is_skipped() {
    use crate::transfer::BlitRect;
    use hl_gpu::protocol::model::descriptor::{Extent3d, Mirror, Origin3d};

    let offsets = |x0: i32, y0: i32, x1: i32, y1: i32| {
        [
            VkOffset3D { x: x0, y: y0, z: 0 },
            VkOffset3D { x: x1, y: y1, z: 1 },
        ]
    };

    // Control: an empty rect on either axis is nothing to do and must not become a region.
    assert!(
        BlitRect::of(&offsets(2, 0, 2, 4)).is_none(),
        "a zero-width region is nothing to do"
    );
    assert!(
        BlitRect::of(&offsets(0, 3, 4, 3)).is_none(),
        "a zero-height region is nothing to do"
    );

    // A forward rect: origin at the low corner, extent the span, no flip.
    let forward = BlitRect::of(&offsets(1, 2, 5, 8)).expect("a non-empty region");
    assert_eq!(
        (forward.origin, forward.extent),
        (
            Origin3d { x: 1, y: 2, z: 0 },
            Extent3d { width: 4, height: 6, depth: 1 }
        )
    );
    assert_eq!(forward.inverted, Mirror::NONE);

    // An inverted rect: the SAME origin and extent — that is why the flip cannot live in them — plus the
    // comparison the min/max performed.
    let inverted = BlitRect::of(&offsets(5, 8, 1, 2)).expect("a non-empty region");
    assert_eq!(
        (inverted.origin, inverted.extent),
        (
            Origin3d { x: 1, y: 2, z: 0 },
            Extent3d { width: 4, height: 6, depth: 1 }
        )
    );
    assert_eq!(inverted.inverted, Mirror { x: true, y: true, z: false });

    // One axis each, to prove the two are independent rather than moving together.
    let flip_x = BlitRect::of(&offsets(5, 2, 1, 8)).expect("a non-empty region");
    assert_eq!(flip_x.inverted, Mirror { x: true, y: false, z: false });
    let flip_y = BlitRect::of(&offsets(1, 8, 5, 2)).expect("a non-empty region");
    assert_eq!(flip_y.inverted, Mirror { x: false, y: true, z: false });

    // The net rule: source XOR destination. Both inverted is the identity.
    assert_eq!(
        Mirror::net(inverted.inverted, inverted.inverted),
        Mirror::NONE,
        "inverting both sides of an axis mirrors twice and must not flip the image"
    );
    assert_eq!(
        Mirror::net(flip_x.inverted, forward.inverted),
        Mirror { x: true, y: false, z: false },
        "one inverted side is a real flip"
    );
    assert_eq!(
        Mirror::net(flip_x.inverted, flip_y.inverted),
        Mirror { x: true, y: true, z: false },
        "the two axes combine independently"
    );
    // The DEPTH axis obeys the same net rule, and is asserted separately because nothing above can see
    // it: every rect so far has a forward z span, so a `net` that dropped z entirely would agree with
    // all of them. Both halves are needed — that one inverted side flips, and that two cancel.
    let flip_z = BlitRect::of(&[
        VkOffset3D { x: 1, y: 2, z: 4 },
        VkOffset3D { x: 5, y: 8, z: 0 },
    ])
    .expect("a non-empty region");
    assert_eq!(
        Mirror::net(flip_z.inverted, forward.inverted),
        Mirror {
            x: false,
            y: false,
            z: true
        },
        "one inverted depth span is a real depth flip"
    );
    assert_eq!(
        Mirror::net(flip_z.inverted, flip_z.inverted),
        Mirror::NONE,
        "inverting the depth span on BOTH sides mirrors twice and must not flip the image"
    );

    // A legal 2D region's z offsets are 0 and 1, which normalize to slice 0, one slice deep. That is
    // asserted above as part of `forward.origin`/`forward.extent` rather than as a special case, which
    // is the point: depth is now the same kind of axis as x and y.
    //
    // The DEPTH span and flip are derived from the same offset pair as the other two axes. Every case
    // above pins `z: false` and `depth: 1` from a forward, one-slice z span, which is the control: this
    // is the only offset pair in the test whose z is both inverted and wider than one slice, so a
    // derivation that ignored z would fail here alone.
    let depth_flipped = BlitRect::of(&[
        VkOffset3D { x: 1, y: 2, z: 4 },
        VkOffset3D { x: 5, y: 8, z: 0 },
    ])
    .expect("a non-empty region");
    assert_eq!(
        depth_flipped.inverted,
        Mirror {
            x: false,
            y: false,
            z: true
        },
        "an inverted depth span is a depth flip, and must not be read as an x or y flip"
    );
    assert_eq!(
        (depth_flipped.origin, depth_flipped.extent),
        (
            Origin3d { x: 1, y: 2, z: 0 },
            Extent3d { width: 4, height: 6, depth: 4 }
        ),
        "an inverted z span normalizes to the same origin and extent as a forward one; the flip lives \
         in `inverted`, not in the span"
    );

    // A ZERO z span is an empty region and is skipped, exactly as a zero-width one is. Before depth was
    // normalized it could not be: z had no extent to be zero, and `(0, 0)` was refused as a 3D region —
    // the same answer a legal depth-spanning blit got.
    assert!(
        BlitRect::of(&[
            VkOffset3D { x: 1, y: 2, z: 3 },
            VkOffset3D { x: 5, y: 8, z: 3 },
        ])
        .is_none(),
        "a zero-depth region is nothing to do"
    );
}
