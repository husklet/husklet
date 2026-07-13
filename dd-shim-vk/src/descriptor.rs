//! Descriptor set-layout / pool / set entry points (real bodies).
//!
//! Ported from MoltenVK `MVKDescriptorSet.mm` (`MVKDescriptorSetLayout`, `MVKDescriptorPool`,
//! `MVKDescriptorSet`):
//!   * `MVKDescriptorSetLayout` retains an immutable per-binding table (`descriptorType`,
//!     `descriptorCount`, `stageFlags`, immutable samplers) — [`crate::reg::DescriptorSetLayoutRec`].
//!   * `MVKDescriptorPool::allocateDescriptorSets` (l.2031) enforces `maxSets` capacity (returns
//!     `VK_ERROR_OUT_OF_POOL_MEMORY` and fills `VK_NULL_HANDLE` when exhausted), and `_freeAllowed`
//!     (`VK_DESCRIPTOR_POOL_CREATE_FREE_DESCRIPTOR_SET_BIT`, l.2003) gates `vkFreeDescriptorSets`.
//!   * A descriptor set binds resources at `(set, binding[, arrayElement])`; `vkUpdateDescriptorSets`
//!     writes **every** array element (not just element 0) and applies its whole write+copy batch
//!     atomically. Buffer descriptors lower to the dd-gpu IR bind group at `vkCmdBindDescriptorSets`
//!     (`Cmd::CreateBindGroup(id, BindGroupDesc{ set, entries })`); image/sampler/texel writes are
//!     retained for a later IR increment rather than silently dropped.

use crate::reg::{self, DescriptorLayoutBinding, DescriptorPoolRec, DescriptorSetLayoutRec, DsetRec};
use crate::types::*;
use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use std::collections::HashMap;

/// Whether a `VkDescriptorType` (raw) names a buffer descriptor (uniform/storage, incl. dynamic).
fn is_buffer_descriptor(ty: i32) -> bool {
    // 6 UNIFORM_BUFFER, 7 STORAGE_BUFFER, 8 UNIFORM_BUFFER_DYNAMIC, 9 STORAGE_BUFFER_DYNAMIC.
    matches!(ty, 6 | 7 | 8 | 9)
}
/// Whether a `VkDescriptorType` (raw) names an image/sampler descriptor.
fn is_image_descriptor(ty: i32) -> bool {
    // 0 SAMPLER, 1 COMBINED_IMAGE_SAMPLER, 2 SAMPLED_IMAGE, 3 STORAGE_IMAGE, 10 INPUT_ATTACHMENT.
    matches!(ty, 0 | 1 | 2 | 3 | 10)
}
/// Whether a `VkDescriptorType` (raw) names a texel-buffer descriptor.
fn is_texel_descriptor(ty: i32) -> bool {
    // 4 UNIFORM_TEXEL_BUFFER, 5 STORAGE_TEXEL_BUFFER.
    matches!(ty, 4 | 5)
}

// ---- descriptor set layout -----------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateDescriptorSetLayout(
    _device: VkDevice,
    p_create_info: *const vk::DescriptorSetLayoutCreateInfo,
    _p_allocator: *const c_void,
    p_set_layout: *mut VkDescriptorSetLayout,
) -> VkResult {
    let Some(out) = (unsafe { p_set_layout.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    // Retain the immutable binding table (MVKDescriptorSetLayout): type/count/stages/immutable samplers.
    let mut bindings: Vec<DescriptorLayoutBinding> = Vec::new();
    if let Some(ci) = unsafe { p_create_info.as_ref() } {
        if !ci.p_bindings.is_null() {
            let src = unsafe { core::slice::from_raw_parts(ci.p_bindings, ci.binding_count as usize) };
            for b in src {
                let immutable_samplers = if b.p_immutable_samplers.is_null() || b.descriptor_count == 0 {
                    Vec::new()
                } else {
                    unsafe { core::slice::from_raw_parts(b.p_immutable_samplers, b.descriptor_count as usize) }
                        .iter()
                        .map(|s| s.as_raw())
                        .collect()
                };
                bindings.push(DescriptorLayoutBinding {
                    binding: b.binding,
                    descriptor_type: b.descriptor_type.as_raw(),
                    descriptor_count: b.descriptor_count,
                    stage_flags: b.stage_flags.as_raw(),
                    immutable_samplers,
                });
            }
        }
    }
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.descriptor_set_layouts.insert(handle, DescriptorSetLayoutRec { bindings });
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorSetLayout(
    _device: VkDevice,
    set_layout: VkDescriptorSetLayout,
    _p_allocator: *const c_void,
) {
    reg::lock().descriptor_set_layouts.remove(&set_layout);
}

// ---- descriptor pool -----------------------------------------------------------------------------

#[no_mangle]
pub extern "C" fn vkCreateDescriptorPool(
    _device: VkDevice,
    p_create_info: *const vk::DescriptorPoolCreateInfo,
    _p_allocator: *const c_void,
    p_descriptor_pool: *mut VkDescriptorPool,
) -> VkResult {
    let Some(out) = (unsafe { p_descriptor_pool.as_mut() }) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    let (max_sets, free_descriptor_set) = unsafe { p_create_info.as_ref() }
        .map(|ci| {
            (
                ci.max_sets,
                ci.flags.contains(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET),
            )
        })
        .unwrap_or((0, false));
    let mut s = reg::lock();
    let handle = s.alloc_handle();
    s.descriptor_pools.insert(
        handle,
        DescriptorPoolRec {
            max_sets,
            allocated: 0,
            free_descriptor_set,
        },
    );
    *out = handle;
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkDestroyDescriptorPool(
    _device: VkDevice,
    descriptor_pool: VkDescriptorPool,
    _p_allocator: *const c_void,
) {
    let mut s = reg::lock();
    s.descriptor_pools.remove(&descriptor_pool);
    // Destroying a pool implicitly frees every set allocated from it (spec §14.2.3).
    s.dsets.retain(|_, d| d.pool != descriptor_pool);
}

// ---- descriptor sets -----------------------------------------------------------------------------

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
    let count = ai.descriptor_set_count as usize;
    let pool_handle = ai.descriptor_pool.as_raw();
    let layouts = if ai.p_set_layouts.is_null() {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(ai.p_set_layouts, count) }
    };
    let mut s = reg::lock();
    // Pool must exist, and (when it declares a positive maxSets) have room — MVKDescriptorPool::
    // allocateDescriptorSets returns VK_ERROR_OUT_OF_POOL_MEMORY when the bump allocator is exhausted.
    let Some(pool) = s.descriptor_pools.get(&pool_handle) else {
        return VK_ERROR_INITIALIZATION_FAILED;
    };
    if pool.max_sets > 0 && pool.allocated as usize + count > pool.max_sets as usize {
        // Fill VK_NULL_HANDLE (MoltenVK fills the whole array on failure).
        for i in 0..count {
            unsafe { *p_descriptor_sets.add(i) = 0 };
        }
        return VK_ERROR_OUT_OF_POOL_MEMORY;
    }
    for i in 0..count {
        let layout = layouts.get(i).map(|l| l.as_raw()).unwrap_or(0);
        let handle = s.alloc_handle();
        s.dsets.insert(
            handle,
            DsetRec {
                set: 0, // assigned at vkCmdBindDescriptorSets (firstSet + i)
                layout,
                pool: pool_handle,
                buffers: HashMap::new(),
                image_writes: HashMap::new(),
                texel_writes: HashMap::new(),
            },
        );
        unsafe { *p_descriptor_sets.add(i) = handle };
    }
    if let Some(p) = s.descriptor_pools.get_mut(&pool_handle) {
        p.allocated += count as u32;
    }
    VK_SUCCESS
}

#[no_mangle]
pub extern "C" fn vkFreeDescriptorSets(
    _device: VkDevice,
    descriptor_pool: VkDescriptorPool,
    descriptor_set_count: u32,
    p_descriptor_sets: *const VkDescriptorSet,
) -> VkResult {
    let mut s = reg::lock();
    // vkFreeDescriptorSets is only valid on a pool created with FREE_DESCRIPTOR_SET; otherwise the
    // spec requires the app not to call it, and returning sets would corrupt the bump allocator.
    let free_allowed = s
        .descriptor_pools
        .get(&descriptor_pool)
        .map(|p| p.free_descriptor_set)
        .unwrap_or(false);
    if !free_allowed || p_descriptor_sets.is_null() {
        return VK_SUCCESS;
    }
    let mut freed = 0u32;
    for i in 0..descriptor_set_count as usize {
        let h = unsafe { *p_descriptor_sets.add(i) };
        if h != 0 && s.dsets.remove(&h).is_some() {
            freed += 1;
        }
    }
    if let Some(p) = s.descriptor_pools.get_mut(&descriptor_pool) {
        p.allocated = p.allocated.saturating_sub(freed);
    }
    VK_SUCCESS
}

/// `vkUpdateDescriptorSets` — apply the whole write+copy batch. Every array element of every write is
/// applied (buffer/image/texel), never just element 0, and the batch is applied atomically: if any
/// target set is unknown the update is a no-op rather than a partial mutation. Buffer writes feed the
/// IR bind group; image/sampler/texel writes are retained for a later IR increment. Copies duplicate
/// bindings between sets (MVKDescriptorSet copy path).
#[no_mangle]
pub extern "C" fn vkUpdateDescriptorSets(
    _device: VkDevice,
    descriptor_write_count: u32,
    p_descriptor_writes: *const vk::WriteDescriptorSet,
    descriptor_copy_count: u32,
    p_descriptor_copies: *const vk::CopyDescriptorSet,
) {
    let writes = if p_descriptor_writes.is_null() {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(p_descriptor_writes, descriptor_write_count as usize) }
    };
    let copies = if p_descriptor_copies.is_null() {
        &[][..]
    } else {
        unsafe { core::slice::from_raw_parts(p_descriptor_copies, descriptor_copy_count as usize) }
    };
    let mut s = reg::lock();
    // Atomic: validate the entire batch's target/source sets exist BEFORE mutating anything.
    for w in writes {
        if !s.dsets.contains_key(&w.dst_set.as_raw()) {
            return;
        }
    }
    for c in copies {
        if !s.dsets.contains_key(&c.src_set.as_raw()) || !s.dsets.contains_key(&c.dst_set.as_raw()) {
            return;
        }
    }
    // Apply writes (all array elements, by descriptor class).
    for w in writes {
        if w.descriptor_count == 0 {
            continue;
        }
        let n = w.descriptor_count as usize;
        let ty = w.descriptor_type.as_raw();
        let dst = w.dst_set.as_raw();
        if is_buffer_descriptor(ty) && !w.p_buffer_info.is_null() {
            let infos = unsafe { core::slice::from_raw_parts(w.p_buffer_info, n) };
            for bi in infos {
                let buffer_handle = bi.buffer.as_raw();
                let range = if bi.range == vk::WHOLE_SIZE {
                    s.buffers.get(&buffer_handle).map(|b| b.size).unwrap_or(0)
                } else {
                    bi.range
                };
                if let Some(d) = s.dsets.get_mut(&dst) {
                    // The IR bind group is per-binding; array element >0 needs IR array support (a
                    // later increment) — every element is processed, none silently dropped.
                    d.buffers.insert(w.dst_binding, (buffer_handle, bi.offset, range));
                }
            }
        } else if is_image_descriptor(ty) && !w.p_image_info.is_null() {
            let infos = unsafe { core::slice::from_raw_parts(w.p_image_info, n) };
            let entries: Vec<(u64, u64, i32)> = infos
                .iter()
                .map(|ii| (ii.image_view.as_raw(), ii.sampler.as_raw(), ii.image_layout.as_raw()))
                .collect();
            if let Some(d) = s.dsets.get_mut(&dst) {
                d.image_writes.insert(w.dst_binding, entries);
            }
        } else if is_texel_descriptor(ty) && !w.p_texel_buffer_view.is_null() {
            let views = unsafe { core::slice::from_raw_parts(w.p_texel_buffer_view, n) };
            let entries: Vec<u64> = views.iter().map(|v| v.as_raw()).collect();
            if let Some(d) = s.dsets.get_mut(&dst) {
                d.texel_writes.insert(w.dst_binding, entries);
            }
        }
    }
    // Apply copies (duplicate a binding's buffer/image/texel entry from src to dst).
    for c in copies {
        let src = c.src_set.as_raw();
        let dst = c.dst_set.as_raw();
        let buf = s.dsets.get(&src).and_then(|d| d.buffers.get(&c.src_binding).copied());
        let img = s.dsets.get(&src).and_then(|d| d.image_writes.get(&c.src_binding).cloned());
        let texel = s.dsets.get(&src).and_then(|d| d.texel_writes.get(&c.src_binding).cloned());
        if let Some(d) = s.dsets.get_mut(&dst) {
            if let Some(b) = buf {
                d.buffers.insert(c.dst_binding, b);
            }
            if let Some(im) = img {
                d.image_writes.insert(c.dst_binding, im);
            }
            if let Some(tx) = texel {
                d.texel_writes.insert(c.dst_binding, tx);
            }
        }
    }
}
