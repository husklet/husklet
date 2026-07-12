//! Descriptor set-layout / pool / set entry points (real bodies).
//!
//! Ported from MoltenVK `MVKDescriptorSet.mm` / `MVKDescriptor.mm`: a descriptor set binds resources
//! at `(set, binding)`; `vkUpdateDescriptorSets` writes the buffer bindings. The dd-gpu IR expresses
//! the same thing as a bind group (`Cmd::CreateBindGroup(id, BindGroupDesc{ set, entries })`) where
//! each `BindEntry{ binding, resource: Buffer{ id, offset, size } }` is a `(set,binding)` passthrough —
//! exactly what the host App-pipeline path expects (naga derives the MSL buffer index from the SPIR-V
//! `(descriptorSet, binding)`, mirroring `SPIRVToMSLConverter`'s `MSLResourceBinding`). The bind group
//! itself is materialized at `vkCmdBindDescriptorSets` (see `crate::command`), once the set index is
//! known.

use crate::reg::{self, DsetRec};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use std::collections::HashMap;

#[no_mangle]
pub extern "C" fn vkCreateDescriptorSetLayout(
    _device: VkDevice,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_set_layout: *mut VkDescriptorSetLayout,
) -> VkResult {
    if let Some(out) = unsafe { p_set_layout.as_mut() } {
        *out = reg::lock().alloc_handle();
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorSetLayout(
    _device: VkDevice,
    _set_layout: VkDescriptorSetLayout,
    _p_allocator: *const c_void,
) {
}

#[no_mangle]
pub extern "C" fn vkCreateDescriptorPool(
    _device: VkDevice,
    _p_create_info: *const c_void,
    _p_allocator: *const c_void,
    p_descriptor_pool: *mut VkDescriptorPool,
) -> VkResult {
    if let Some(out) = unsafe { p_descriptor_pool.as_mut() } {
        *out = reg::lock().alloc_handle();
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorPool(
    _device: VkDevice,
    _descriptor_pool: VkDescriptorPool,
    _p_allocator: *const c_void,
) {
}

#[no_mangle]
pub extern "C" fn vkAllocateDescriptorSets(
    _device: VkDevice,
    p_allocate_info: *const vk::DescriptorSetAllocateInfo,
    p_descriptor_sets: *mut VkDescriptorSet,
) -> VkResult {
    let Some(ai) = (unsafe { p_allocate_info.as_ref() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if p_descriptor_sets.is_null() {
        return VK_ERROR_INITIALIZATION_FAILED;
    }
    let mut s = reg::lock();
    for i in 0..ai.descriptor_set_count as usize {
        let handle = s.alloc_handle();
        s.dsets.insert(
            handle,
            DsetRec {
                set: 0, // assigned at vkCmdBindDescriptorSets (firstSet + i)
                buffers: HashMap::new(),
            },
        );
        unsafe { *p_descriptor_sets.add(i) = handle };
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkFreeDescriptorSets(
    _device: VkDevice,
    _descriptor_pool: VkDescriptorPool,
    descriptor_set_count: u32,
    p_descriptor_sets: *const VkDescriptorSet,
) -> VkResult {
    if !p_descriptor_sets.is_null() {
        let mut s = reg::lock();
        for i in 0..descriptor_set_count as usize {
            let h = unsafe { *p_descriptor_sets.add(i) };
            s.dsets.remove(&h);
        }
    }
    VK_SUCCESS
}

/// Write buffer bindings into the target descriptor sets (the `(set,binding) -> buffer` table the
/// bind group is later built from). Image/texel writes are recorded in a later increment.
#[no_mangle]
pub extern "C" fn vkUpdateDescriptorSets(
    _device: VkDevice,
    descriptor_write_count: u32,
    p_descriptor_writes: *const vk::WriteDescriptorSet,
    _descriptor_copy_count: u32,
    _p_descriptor_copies: *const c_void,
) {
    if p_descriptor_writes.is_null() {
        return;
    }
    let writes =
        unsafe { core::slice::from_raw_parts(p_descriptor_writes, descriptor_write_count as usize) };
    let mut s = reg::lock();
    for w in writes {
        if w.p_buffer_info.is_null() || w.descriptor_count == 0 {
            continue;
        }
        let bufinfos =
            unsafe { core::slice::from_raw_parts(w.p_buffer_info, w.descriptor_count as usize) };
        // First array element per binding (arrays land in later increments).
        let bi = &bufinfos[0];
        let buffer_handle = bi.buffer.as_raw();
        let range = if bi.range == vk::WHOLE_SIZE {
            s.buffers.get(&buffer_handle).map(|b| b.size).unwrap_or(0)
        } else {
            bi.range
        };
        if let Some(d) = s.dsets.get_mut(&w.dst_set.as_raw()) {
            d.buffers.insert(w.dst_binding, (buffer_handle, bi.offset, range));
        }
    }
}
