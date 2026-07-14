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
