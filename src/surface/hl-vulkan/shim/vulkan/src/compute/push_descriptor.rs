//! `VK_KHR_push_descriptor` / core Vulkan 1.4 push descriptors, and the `VK_KHR_maintenance6` struct
//! forms promoted with them.
//!
//! A pushed descriptor set is command-buffer state rather than an app-owned object, so the shim mints one
//! descriptor set per `(command buffer, set number)` from the layout the pipeline layout declares for that
//! set, applies the writes through the ordinary `vkUpdateDescriptorSets` path, and binds it at that index.
//! The result is byte-identical to the equivalent allocate + update + bind sequence, which is what the
//! spec defines push descriptors to be. Consecutive pushes accumulate into the same set; re-recording the
//! command buffer forgets it.
//!
//! These seven commands were previously silent `void` no-ops labelled "extension not advertised" while the
//! driver advertised core 1.4, which mandates them — a lie a `void` command cannot even report.

use super::*;

/// `VK_PIPELINE_BIND_POINT_GRAPHICS`. The struct forms carry `stageFlags` instead of a bind point, and the
/// bind-group lowering keys on the set index alone, so the positional bodies take this as their bind point.
const VK_PIPELINE_BIND_POINT_GRAPHICS: i32 = 0;

/// `VkPushDescriptorSetInfo` (core 1.4 / `VK_KHR_maintenance6`). Layout from `vk.xml`.
#[repr(C)]
pub struct VkPushDescriptorSetInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub stage_flags: u32,
    pub layout: u64,
    pub set: u32,
    pub descriptor_write_count: u32,
    pub p_descriptor_writes: *const c_void,
}

/// `VkPushDescriptorSetWithTemplateInfo` (core 1.4 / `VK_KHR_maintenance6`). Layout from `vk.xml`.
#[repr(C)]
pub struct VkPushDescriptorSetWithTemplateInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub descriptor_update_template: u64,
    pub layout: u64,
    pub set: u32,
    pub p_data: *const c_void,
}

/// `VkBindDescriptorSetsInfo` (core 1.4 / `VK_KHR_maintenance6`). Layout from `vk.xml`.
#[repr(C)]
pub struct VkBindDescriptorSetsInfo {
    pub s_type: i32,
    pub p_next: *const c_void,
    pub stage_flags: u32,
    pub layout: u64,
    pub first_set: u32,
    pub descriptor_set_count: u32,
    pub p_descriptor_sets: *const u64,
    pub dynamic_offset_count: u32,
    pub p_dynamic_offsets: *const u32,
}

/// The set a command buffer's pushes at `set` accumulate into.
pub(crate) struct PushSet;

impl PushSet {
    /// The descriptor set for `(cb, set)`, allocating it from `layout`'s declared set layout on first use.
    /// `None` when the pipeline layout is unknown or declares no layout at that set index — the same
    /// invalid usage `vkCmdBindDescriptorSets` ignores.
    fn resolve(cb: VkCbHandle, layout: u64, set: u32) -> Option<u64> {
        StateStore::with(|state| {
            if let Some(existing) = state.push_descriptor_sets.get(&(cb, set)) {
                return Some(*existing);
            }
            let device = state.device.as_mut()?;
            let set_layout = *device
                .pipeline_layouts
                .get(&layout)?
                .set_layouts
                .get(set as usize)?;
            if state.push_descriptor_pool == 0 {
                // maxSets == 0 is unbounded in the model, which is what push descriptors need: their
                // count is bounded by the command buffers in flight, not by an app-declared pool size.
                state.push_descriptor_pool = device.create_descriptor_pool(0);
            }
            let pool = state.push_descriptor_pool;
            let device = state.device.as_mut()?;
            let handle = create::allocate_descriptor_set(device, pool, set_layout, set).ok()?;
            state.push_descriptor_sets.insert((cb, set), handle);
            Some(handle)
        })
    }

    /// Forget every set pushed into `cb`. Called from `vkBeginCommandBuffer`: push-descriptor state does
    /// not survive re-recording, so a stale descriptor must not be replayed into the next recording.
    pub(crate) fn forget(cb: VkCbHandle) {
        StateStore::with(|state| {
            state
                .push_descriptor_sets
                .retain(|(owner, _), _| *owner != cb)
        });
    }
}

/// `vkCmdPushDescriptorSet` (core 1.4, promoted from `VK_KHR_push_descriptor`) — apply `pDescriptorWrites`
/// to this command buffer's set at `set` and bind it there. Writes name no `dstSet` of their own (the spec
/// requires the field be ignored), so each is retargeted onto the pushed set and applied through the
/// identical `vkUpdateDescriptorSets` lowering.
pub extern "C" fn vkCmdPushDescriptorSet(
    command_buffer: *mut c_void,
    pipeline_bind_point: i32,
    layout: u64,
    set: u32,
    descriptor_write_count: u32,
    p_descriptor_writes: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_descriptor_writes.is_null() || descriptor_write_count == 0 {
        return;
    }
    let Some(pushed) = PushSet::resolve(cb, layout, set) else {
        return;
    };
    let writes = unsafe {
        std::slice::from_raw_parts(
            p_descriptor_writes as *const VkWriteDescriptorSet,
            descriptor_write_count as usize,
        )
    };
    let retargeted: Vec<VkWriteDescriptorSet> = writes
        .iter()
        .map(|write| VkWriteDescriptorSet {
            s_type: write.s_type,
            p_next: write.p_next,
            dst_set: pushed,
            dst_binding: write.dst_binding,
            dst_array_element: write.dst_array_element,
            descriptor_count: write.descriptor_count,
            descriptor_type: write.descriptor_type,
            p_image_info: write.p_image_info,
            p_buffer_info: write.p_buffer_info,
            p_texel_buffer_view: write.p_texel_buffer_view,
        })
        .collect();
    vkUpdateDescriptorSets(
        core::ptr::null_mut(),
        retargeted.len() as u32,
        retargeted.as_ptr() as *const c_void,
        0,
        core::ptr::null(),
    );
    vkCmdBindDescriptorSets(
        command_buffer,
        pipeline_bind_point,
        layout,
        set,
        1,
        &pushed,
        0,
        core::ptr::null(),
    );
}

/// `vkCmdPushDescriptorSetKHR` — the pre-promotion `VK_KHR_push_descriptor` spelling.
pub extern "C" fn vkCmdPushDescriptorSetKHR(
    command_buffer: *mut c_void,
    pipeline_bind_point: i32,
    layout: u64,
    set: u32,
    descriptor_write_count: u32,
    p_descriptor_writes: *const c_void,
) {
    vkCmdPushDescriptorSet(
        command_buffer,
        pipeline_bind_point,
        layout,
        set,
        descriptor_write_count,
        p_descriptor_writes,
    )
}

/// `vkCmdPushDescriptorSetWithTemplate` (core 1.4) — the same push, with the descriptors read out of the
/// app's `pData` blob through a descriptor update template. Forwards to the identical
/// `vkUpdateDescriptorSetWithTemplate` lowering, then binds the pushed set.
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate(
    command_buffer: *mut c_void,
    descriptor_update_template: u64,
    layout: u64,
    set: u32,
    p_data: *const c_void,
) {
    let Some(cb) = (unsafe { CommandBuffer::handle(command_buffer) }) else {
        return;
    };
    if p_data.is_null() {
        return;
    }
    let Some(pushed) = PushSet::resolve(cb, layout, set) else {
        return;
    };
    vkUpdateDescriptorSetWithTemplate(
        core::ptr::null_mut(),
        pushed,
        descriptor_update_template,
        p_data,
    );
    vkCmdBindDescriptorSets(
        command_buffer,
        VK_PIPELINE_BIND_POINT_GRAPHICS,
        layout,
        set,
        1,
        &pushed,
        0,
        core::ptr::null(),
    );
}

/// `vkCmdPushDescriptorSetWithTemplateKHR` — the pre-promotion `VK_KHR_push_descriptor` spelling.
pub extern "C" fn vkCmdPushDescriptorSetWithTemplateKHR(
    command_buffer: *mut c_void,
    descriptor_update_template: u64,
    layout: u64,
    set: u32,
    p_data: *const c_void,
) {
    vkCmdPushDescriptorSetWithTemplate(
        command_buffer,
        descriptor_update_template,
        layout,
        set,
        p_data,
    )
}

/// `vkCmdPushDescriptorSet2` (core 1.4) — the struct form, carrying no new capability.
pub extern "C" fn vkCmdPushDescriptorSet2(
    command_buffer: *mut c_void,
    p_push_descriptor_set_info: *const c_void,
) {
    let Some(info) =
        (unsafe { (p_push_descriptor_set_info as *const VkPushDescriptorSetInfo).as_ref() })
    else {
        return;
    };
    vkCmdPushDescriptorSet(
        command_buffer,
        VK_PIPELINE_BIND_POINT_GRAPHICS,
        info.layout,
        info.set,
        info.descriptor_write_count,
        info.p_descriptor_writes,
    );
}

/// `vkCmdPushDescriptorSet2KHR` — the pre-promotion `VK_KHR_maintenance6` spelling.
pub extern "C" fn vkCmdPushDescriptorSet2KHR(
    command_buffer: *mut c_void,
    p_push_descriptor_set_info: *const c_void,
) {
    vkCmdPushDescriptorSet2(command_buffer, p_push_descriptor_set_info)
}

/// `vkCmdPushDescriptorSetWithTemplate2` (core 1.4) — the struct form of the template push.
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2(
    command_buffer: *mut c_void,
    p_info: *const c_void,
) {
    let Some(info) = (unsafe { (p_info as *const VkPushDescriptorSetWithTemplateInfo).as_ref() })
    else {
        return;
    };
    vkCmdPushDescriptorSetWithTemplate(
        command_buffer,
        info.descriptor_update_template,
        info.layout,
        info.set,
        info.p_data,
    );
}

/// `vkCmdPushDescriptorSetWithTemplate2KHR` — the pre-promotion `VK_KHR_maintenance6` spelling.
pub extern "C" fn vkCmdPushDescriptorSetWithTemplate2KHR(
    command_buffer: *mut c_void,
    p_info: *const c_void,
) {
    vkCmdPushDescriptorSetWithTemplate2(command_buffer, p_info)
}

/// `vkCmdBindDescriptorSets2` (core 1.4, promoted from `VK_KHR_maintenance6`) — `vkCmdBindDescriptorSets`
/// with its arguments in a struct. It was a silent no-op, so every set a client bound this way was
/// dropped and the draw read whatever was bound before.
pub extern "C" fn vkCmdBindDescriptorSets2(
    command_buffer: *mut c_void,
    p_bind_descriptor_sets_info: *const c_void,
) {
    let Some(info) =
        (unsafe { (p_bind_descriptor_sets_info as *const VkBindDescriptorSetsInfo).as_ref() })
    else {
        return;
    };
    vkCmdBindDescriptorSets(
        command_buffer,
        VK_PIPELINE_BIND_POINT_GRAPHICS,
        info.layout,
        info.first_set,
        info.descriptor_set_count,
        info.p_descriptor_sets,
        info.dynamic_offset_count,
        info.p_dynamic_offsets,
    );
}

/// `vkCmdBindDescriptorSets2KHR` — the pre-promotion `VK_KHR_maintenance6` spelling.
pub extern "C" fn vkCmdBindDescriptorSets2KHR(
    command_buffer: *mut c_void,
    p_bind_descriptor_sets_info: *const c_void,
) {
    vkCmdBindDescriptorSets2(command_buffer, p_bind_descriptor_sets_info)
}
