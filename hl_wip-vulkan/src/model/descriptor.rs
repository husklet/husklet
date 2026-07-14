//! Descriptor-set-layout / descriptor-pool / descriptor-set records.
//!
//! Ported from `hl-shim-vk/src/{descriptor.rs,reg.rs}` (`MVKDescriptorSetLayout`/`MVKDescriptorPool`/
//! `MVKDescriptorSet`). A `VkDescriptorSet`'s `binding -> (buffer, offset, range)` table is written by
//! `vkUpdateDescriptorSets` and resolved to an IR bind group at `vkCmdBindDescriptorSets`
//! ([`crate::service::record`]) — dynamic offsets applied there.

use crate::{VkBuffer, VkDescriptorPool, VkDescriptorSetLayout};
use std::collections::HashMap;

/// `VkDescriptorType` (stable enum values from vk.xml) for the dynamic-buffer classification.
pub mod vk_descriptor_type {
    pub const UNIFORM_BUFFER: i32 = 6;
    pub const STORAGE_BUFFER: i32 = 7;
    pub const UNIFORM_BUFFER_DYNAMIC: i32 = 8;
    pub const STORAGE_BUFFER_DYNAMIC: i32 = 9;
}

/// Whether a `VkDescriptorType` is one of the buffer descriptors the bring-up compute path models (the
/// only class carried in [`DsetRec::buffers`]). Image/texel descriptors are not materialized here, so —
/// exactly as `vkUpdateDescriptorSets` does — a template applying them is a truthful no-op for that entry.
pub fn is_buffer_descriptor(descriptor_type: i32) -> bool {
    use vk_descriptor_type::*;
    matches!(
        descriptor_type,
        UNIFORM_BUFFER | STORAGE_BUFFER | UNIFORM_BUFFER_DYNAMIC | STORAGE_BUFFER_DYNAMIC
    )
}

/// `VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET` — the only template kind without
/// `VK_KHR_push_descriptor` (the push-descriptor kind needs a bound pipeline layout we do not model).
pub const VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET: i32 = 0;

/// One entry of a `VkDescriptorUpdateTemplate` (Vulkan 1.1 / MoltenVK `MVKDescriptorUpdateTemplate`):
/// where in the app's pushed data blob each descriptor lives (`offset` + per-element `stride`) and which
/// `(binding, arrayElement)` of which class it targets. Ported from `hl-shim-vk/src/reg.rs`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DescriptorTemplateEntry {
    pub dst_binding: u32,
    pub dst_array_element: u32,
    pub descriptor_count: u32,
    /// `VkDescriptorType` (raw).
    pub descriptor_type: i32,
    /// Byte offset of element 0 in the app's `pData` blob.
    pub offset: usize,
    /// Byte stride between consecutive array elements in the blob.
    pub stride: usize,
}

/// A `VkDescriptorUpdateTemplate` (Vulkan 1.1): the immutable entry table
/// `vkUpdateDescriptorSetWithTemplate` walks to read descriptors out of the app's data blob at fixed
/// offsets/strides and apply them to a set exactly as `vkUpdateDescriptorSets` would.
#[derive(Clone, PartialEq, Debug)]
pub struct DescriptorUpdateTemplateRec {
    pub entries: Vec<DescriptorTemplateEntry>,
}

/// `VkDescriptorBufferInfo` byte layout (`{ VkBuffer buffer; VkDeviceSize offset; VkDeviceSize range }`,
/// 24 bytes) — the stable ABI a buffer-class template entry reads out of the app's blob. Re-declared
/// clean-room so the driver layer parses the blob without the C-ABI struct (testable without FFI).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct TemplateBufferInfo {
    pub buffer: u64,
    pub offset: u64,
    pub range: u64,
}

/// One immutable binding of a `VkDescriptorSetLayout`. Mirrors `MVKDescriptorSetLayout` binding record.
#[derive(Clone, PartialEq, Debug)]
pub struct LayoutBinding {
    pub binding: u32,
    /// `VkDescriptorType` (raw).
    pub descriptor_type: i32,
    /// Array size (0 disables the binding).
    pub descriptor_count: u32,
    /// `VkShaderStageFlags`.
    pub stage_flags: u32,
}

/// A `VkDescriptorSetLayout`: its immutable binding table. Mirrors `MVKDescriptorSetLayout`.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct SetLayoutRec {
    pub bindings: Vec<LayoutBinding>,
}

impl SetLayoutRec {
    /// The dynamic-buffer bindings (UNIFORM/STORAGE_BUFFER_DYNAMIC), in ascending binding order — the
    /// order `vkCmdBindDescriptorSets`'s `pDynamicOffsets` are consumed in.
    pub fn dynamic_bindings(&self) -> Vec<u32> {
        use vk_descriptor_type::{STORAGE_BUFFER_DYNAMIC, UNIFORM_BUFFER_DYNAMIC};
        let mut v: Vec<u32> = self
            .bindings
            .iter()
            .filter(|b| {
                b.descriptor_type == UNIFORM_BUFFER_DYNAMIC
                    || b.descriptor_type == STORAGE_BUFFER_DYNAMIC
            })
            .map(|b| b.binding)
            .collect();
        v.sort_unstable();
        v
    }
}

/// A `VkDescriptorPool`: its capacity + live set count. Mirrors `MVKDescriptorPool`.
#[derive(Clone, PartialEq, Debug)]
pub struct DescriptorPoolRec {
    /// `VkDescriptorPoolCreateInfo::maxSets` (0 = no positive limit declared → not quota-capped).
    pub max_sets: u32,
    pub allocated: u32,
}

/// A `VkDescriptorSet`: the layout + owning pool it was allocated with, and the `binding -> (buffer,
/// offset, range)` table `vkUpdateDescriptorSets` writes. Resolved to an IR bind group at
/// `vkCmdBindDescriptorSets`. Mirrors `MVKDescriptorSet`.
#[derive(Clone, PartialEq, Debug)]
pub struct DsetRec {
    pub set: u32,
    pub layout: VkDescriptorSetLayout,
    pub pool: VkDescriptorPool,
    /// `binding -> (buffer handle, offset, range)`.
    pub buffers: HashMap<u32, (VkBuffer, u64, u64)>,
}

impl DsetRec {
    pub fn new(set: u32, layout: VkDescriptorSetLayout, pool: VkDescriptorPool) -> Self {
        Self { set, layout, pool, buffers: HashMap::new() }
    }
}
