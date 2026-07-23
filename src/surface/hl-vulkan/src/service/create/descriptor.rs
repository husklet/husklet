//! Descriptor layouts, pools, sets, writes, and update templates.

use crate::model::descriptor::{
    DescriptorPoolRec, DescriptorTemplateEntry, DescriptorType, DescriptorUpdateTemplateRec,
    DsetRec, LayoutBinding, SetLayoutRec, TemplateBufferInfo,
    VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
};
use crate::*;
use hl_gpu::{GpuError, Result};

// ---- descriptor sets -----------------------------------------------------------------------------

/// `vkCreateDescriptorSetLayout` — record the immutable binding table. No IR.
impl Device {
    pub fn create_descriptor_set_layout(
        &mut self,
        bindings: Vec<LayoutBinding>,
    ) -> VkDescriptorSetLayout {
        let handle = self.alloc_handle();
        self.set_layouts.insert(handle, SetLayoutRec { bindings });
        handle
    }

    /// `vkCreateDescriptorPool` — record the pool capacity. No IR.
    pub fn create_descriptor_pool(&mut self, max_sets: u32) -> VkDescriptorPool {
        let handle = self.alloc_handle();
        self.descriptor_pools.insert(
            handle,
            DescriptorPoolRec {
                max_sets,
                allocated: 0,
            },
        );
        handle
    }
}

/// `vkAllocateDescriptorSets` (one set) — allocate a set of `layout` from `pool`. Errors
/// (`VK_ERROR_OUT_OF_POOL_MEMORY` analogue) if the pool's `max_sets` quota is exhausted. No IR — the
/// IR bind group is built later at `vkCmdBindDescriptorSets` ([`crate::service::record`]).
pub fn allocate_descriptor_set(
    dev: &mut Device,
    pool: VkDescriptorPool,
    layout: VkDescriptorSetLayout,
    set_index: u32,
) -> Result<VkDescriptorSet> {
    let p = dev
        .descriptor_pools
        .get_mut(&pool)
        .ok_or(GpuError::Invalid(
            "vkAllocateDescriptorSets: unknown VkDescriptorPool",
        ))?;
    if p.max_sets != 0 && p.allocated >= p.max_sets {
        return Err(GpuError::ResourceLimit(
            "vkAllocateDescriptorSets: pool out of sets",
        ));
    }
    p.allocated += 1;
    let handle = dev.alloc_handle();
    dev.descriptor_sets
        .insert(handle, DsetRec::new(set_index, layout, pool));
    Ok(handle)
}

/// `vkUpdateDescriptorSets` (one buffer write) — record a `binding -> (buffer, offset, range)` entry on
/// the set. Errors on an unknown set. No IR (resolved at bind time).
pub fn update_descriptor_buffer(
    dev: &mut Device,
    set: VkDescriptorSet,
    binding: u32,
    buffer: VkBuffer,
    offset: u64,
    range: u64,
) -> Result<()> {
    let rec = dev.descriptor_sets.get_mut(&set).ok_or(GpuError::Invalid(
        "vkUpdateDescriptorSets: unknown VkDescriptorSet",
    ))?;
    rec.buffers.insert(binding, (buffer, offset, range));
    Ok(())
}

/// `vkUpdateDescriptorSets` (one image/sampler write) — record a sampled-image / sampler descriptor on
/// the set at `binding`. `image` is the `VkImage` a `SAMPLED_IMAGE`/`COMBINED_IMAGE_SAMPLER` write's
/// `imageView` resolves to (the shim owns the view→image mapping); `sampler` is the `VkSampler` a
/// `SAMPLER`/`COMBINED_IMAGE_SAMPLER` write carries. Either may be `None` (a separate SAMPLED_IMAGE or
/// SAMPLER write). Present fields overwrite; absent fields leave any prior value (so separate image + sampler
/// writes to the same binding compose). Errors on an unknown set. No IR (resolved at bind time).
pub fn update_descriptor_image(
    dev: &mut Device,
    set: VkDescriptorSet,
    binding: u32,
    image: Option<VkImage>,
    sampler: Option<VkSampler>,
) -> Result<()> {
    let rec = dev.descriptor_sets.get_mut(&set).ok_or(GpuError::Invalid(
        "vkUpdateDescriptorSets: unknown VkDescriptorSet",
    ))?;
    let entry = rec.images.entry(binding).or_default();
    if image.is_some() {
        entry.image = image;
    }
    if sampler.is_some() {
        entry.sampler = sampler;
    }
    Ok(())
}

// ---- descriptor update templates -----------------------------------------------------------------

/// `vkCreateDescriptorUpdateTemplate(KHR)` — retain the immutable entry table (offset/stride/binding/
/// type) the app later pushes descriptors through. Only the `DESCRIPTOR_SET` template type is modeled
/// (push-descriptor templates need a bound pipeline layout the bring-up path lacks), so a different type
/// is a truthful `VK_ERROR_FEATURE_NOT_PRESENT` analogue. No IR. Ported from
/// `hl-shim-vk/src/descriptor.rs::vkCreateDescriptorUpdateTemplate`.
pub fn create_descriptor_update_template(
    dev: &mut Device,
    template_type: i32,
    entries: Vec<DescriptorTemplateEntry>,
) -> Result<VkDescriptorUpdateTemplate> {
    if template_type != VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET {
        return Err(GpuError::Unsupported(
            "vkCreateDescriptorUpdateTemplate: only DESCRIPTOR_SET templates are supported",
        ));
    }
    let handle = dev.alloc_handle();
    dev.descriptor_update_templates
        .insert(handle, DescriptorUpdateTemplateRec { entries });
    Ok(handle)
}

/// `vkDestroyDescriptorUpdateTemplate(KHR)` — drop the template. No-op on `VK_NULL_HANDLE`/unknown.
impl Device {
    pub fn destroy_descriptor_update_template(&mut self, template: VkDescriptorUpdateTemplate) {
        self.descriptor_update_templates.remove(&template);
    }
}

/// `vkUpdateDescriptorSetWithTemplate(KHR)` — walk the template entries, reading each buffer descriptor
/// out of `data` at `entry.offset + i*entry.stride` and applying it to `set` **exactly as**
/// [`update_descriptor_buffer`] does (so a template update yields the same `binding -> (buffer, offset,
/// range)` table — and thus the same IR bind group — as the equivalent direct `vkUpdateDescriptorSets`).
/// Image/texel descriptors are not materialized in the compute path, so those entries are a truthful
/// no-op (mirroring the direct-write path). Errors on an unknown template or set; a short blob (an entry
/// reading past `data`) is a truthful out-of-bounds error, never a junk read. Ported from
/// `hl-shim-vk/src/descriptor.rs::vkUpdateDescriptorSetWithTemplate`.
pub fn update_descriptor_set_with_template(
    dev: &mut Device,
    set: VkDescriptorSet,
    template: VkDescriptorUpdateTemplate,
    data: &[u8],
) -> Result<()> {
    let entries = dev
        .descriptor_update_templates
        .get(&template)
        .ok_or(GpuError::Invalid(
            "vkUpdateDescriptorSetWithTemplate: unknown VkDescriptorUpdateTemplate",
        ))?
        .entries
        .clone();
    if !dev.descriptor_sets.contains_key(&set) {
        return Err(GpuError::Invalid(
            "vkUpdateDescriptorSetWithTemplate: unknown VkDescriptorSet",
        ));
    }
    for e in &entries {
        // Array elements fold onto the binding (the model keys a set's resources by binding), matching
        // `vkUpdateDescriptorSets`.
        if !DescriptorType::from(e.descriptor_type).is_buffer() {
            continue;
        }
        for i in 0..e.descriptor_count as usize {
            let base = e
                .offset
                .checked_add(i.checked_mul(e.stride).ok_or(GpuError::OutOfBounds)?)
                .ok_or(GpuError::OutOfBounds)?;
            let end = base
                .checked_add(core::mem::size_of::<TemplateBufferInfo>())
                .ok_or(GpuError::OutOfBounds)?;
            if end > data.len() {
                return Err(GpuError::OutOfBounds);
            }
            // The blob is app-provided bytes at an arbitrary offset — an unaligned read is required.
            let bi = unsafe {
                core::ptr::read_unaligned(data.as_ptr().add(base) as *const TemplateBufferInfo)
            };
            update_descriptor_buffer(dev, set, e.dst_binding, bi.buffer, bi.offset, bi.range)?;
        }
    }
    Ok(())
}

/// The number of bytes of the app's `pData` blob a template's BUFFER entries read — the max over every
/// buffer entry of `offset + (count-1)*stride + sizeof(VkDescriptorBufferInfo)`. The shim uses this to
/// build a correctly-bounded slice over the raw `pData` pointer (the C API carries no data size), so the
/// bounds check in [`update_descriptor_set_with_template`] is exact. `None` on an unknown template; `0`
/// if the template reads no buffer bytes.
impl Device {
    pub fn descriptor_template_data_len(
        &self,
        template: VkDescriptorUpdateTemplate,
    ) -> Option<usize> {
        let rec = self.descriptor_update_templates.get(&template)?;
        let mut max = 0usize;
        for e in &rec.entries {
            if !DescriptorType::from(e.descriptor_type).is_buffer() || e.descriptor_count == 0 {
                continue;
            }
            let last = (e.descriptor_count as usize - 1).saturating_mul(e.stride);
            let end = e
                .offset
                .saturating_add(last)
                .saturating_add(core::mem::size_of::<TemplateBufferInfo>());
            max = max.max(end);
        }
        Some(max)
    }
}
