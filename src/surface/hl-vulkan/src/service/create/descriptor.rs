//! Descriptor layouts, pools, sets, writes, and update templates.

use crate::model::descriptor::{
    DescriptorPoolRec, DescriptorTemplateEntry, DescriptorType, DescriptorUpdateTemplateRec,
    DsetRec, LayoutBinding, SetLayoutRec, TemplateBufferInfo,
    VK_DESCRIPTOR_UPDATE_TEMPLATE_TYPE_DESCRIPTOR_SET,
};
use crate::*;
use hl_gpu::{GpuError, Result};

/// Create an immutable formatted view over a buffer byte range. Vulkan requires
/// the offset to satisfy `minTexelBufferOffsetAlignment`; Husklet advertises 16.
pub fn create_buffer_view(
    dev: &mut Device,
    buffer: VkBuffer,
    format: hl_gpu::protocol::model::enums::TextureFormat,
    offset: u64,
    range: u64,
) -> Result<VkBufferView> {
    let buffer_size = dev
        .buffers
        .get(&buffer)
        .ok_or(GpuError::Invalid("vkCreateBufferView: unknown VkBuffer"))?
        .size;
    if offset % 16 != 0 || offset > buffer_size {
        return Err(GpuError::Invalid(
            "vkCreateBufferView: offset is misaligned or outside the buffer",
        ));
    }
    let range = if range == u64::MAX {
        buffer_size - offset
    } else {
        range
    };
    let texel = format.bytes_per_texel().ok_or(GpuError::Unsupported(
        "vkCreateBufferView: format has no plain texel layout",
    ))? as u64;
    if range == 0
        || range % texel != 0
        || offset
            .checked_add(range)
            .is_none_or(|end| end > buffer_size)
    {
        return Err(GpuError::Invalid(
            "vkCreateBufferView: range is empty, partial-texel, or outside the buffer",
        ));
    }
    if range / texel > u64::from(dev.physical_device.limits.max_texel_buffer_elements) {
        return Err(GpuError::OutOfBounds);
    }
    let handle = dev.alloc_handle();
    dev.buffer_views.insert(
        handle,
        crate::model::descriptor::BufferViewRec {
            buffer,
            format,
            offset,
            range,
        },
    );
    Ok(handle)
}

pub fn destroy_buffer_view(dev: &mut Device, view: VkBufferView) {
    dev.buffer_views.remove(&view);
}

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
    update_descriptor_buffer_element(dev, set, binding, 0, buffer, offset, range)
}

pub fn update_descriptor_buffer_element(
    dev: &mut Device,
    set: VkDescriptorSet,
    binding: u32,
    array_element: u32,
    buffer: VkBuffer,
    offset: u64,
    range: u64,
) -> Result<()> {
    validate_descriptor_element(dev, set, binding, array_element)?;
    let range = match dev.buffers.get(&buffer) {
        Some(buffer) => {
            if offset > buffer.size {
                return Err(GpuError::OutOfBounds);
            }
            let range = if range == u64::MAX { buffer.size - offset } else { range };
            if range == 0
                || offset
                    .checked_add(range)
                    .is_none_or(|end| end > buffer.size)
            {
                return Err(GpuError::OutOfBounds);
            }
            range
        }
        // A valid Vulkan call always names a live buffer. Preserve the recorder's historical explicit
        // handle staging for push-descriptor tests, but WHOLE_SIZE cannot be resolved without a size.
        None if range == u64::MAX => {
            return Err(GpuError::Invalid("vkUpdateDescriptorSets: unknown VkBuffer"));
        }
        None => range,
    };
    let rec = dev.descriptor_sets.get_mut(&set).ok_or(GpuError::Invalid(
        "vkUpdateDescriptorSets: unknown VkDescriptorSet",
    ))?;
    rec.buffers
        .insert((binding, array_element), (buffer, offset, range));
    Ok(())
}

pub fn update_descriptor_texel_buffer_element(
    dev: &mut Device,
    set: VkDescriptorSet,
    binding: u32,
    array_element: u32,
    view: VkBufferView,
) -> Result<()> {
    validate_descriptor_element(dev, set, binding, array_element)?;
    if !dev.buffer_views.contains_key(&view) {
        return Err(GpuError::Invalid(
            "vkUpdateDescriptorSets: unknown VkBufferView",
        ));
    }
    dev.descriptor_sets
        .get_mut(&set)
        .ok_or(GpuError::Invalid(
            "vkUpdateDescriptorSets: unknown VkDescriptorSet",
        ))?
        .texel_buffers
        .insert((binding, array_element), view);
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
    update_descriptor_image_element(dev, set, binding, 0, image, sampler)
}

pub fn update_descriptor_image_element(
    dev: &mut Device,
    set: VkDescriptorSet,
    binding: u32,
    array_element: u32,
    image: Option<VkImage>,
    sampler: Option<VkSampler>,
) -> Result<()> {
    validate_descriptor_element(dev, set, binding, array_element)?;
    let rec = dev.descriptor_sets.get_mut(&set).ok_or(GpuError::Invalid(
        "vkUpdateDescriptorSets: unknown VkDescriptorSet",
    ))?;
    let entry = rec.images.entry((binding, array_element)).or_default();
    if image.is_some() {
        entry.image = image;
    }
    if sampler.is_some() {
        entry.sampler = sampler;
    }
    Ok(())
}

fn validate_descriptor_element(
    dev: &Device,
    set: VkDescriptorSet,
    binding: u32,
    array_element: u32,
) -> Result<()> {
    let set = dev.descriptor_sets.get(&set).ok_or(GpuError::Invalid(
        "vkUpdateDescriptorSets: unknown VkDescriptorSet",
    ))?;
    let layout = dev.set_layouts.get(&set.layout).ok_or(GpuError::Invalid(
        "vkUpdateDescriptorSets: unknown descriptor set layout",
    ))?;
    let binding = layout
        .bindings
        .iter()
        .find(|candidate| candidate.binding == binding)
        .ok_or(GpuError::Invalid(
            "vkUpdateDescriptorSets: binding not declared by layout",
        ))?;
    if array_element >= binding.descriptor_count {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn copy_descriptors(
    dev: &mut Device,
    source: VkDescriptorSet,
    source_binding: u32,
    source_element: u32,
    destination: VkDescriptorSet,
    destination_binding: u32,
    destination_element: u32,
    count: u32,
) -> Result<()> {
    let source = dev
        .descriptor_sets
        .get(&source)
        .ok_or(GpuError::Invalid(
            "vkUpdateDescriptorSets: unknown source set",
        ))?
        .clone();
    let destination = dev
        .descriptor_sets
        .get_mut(&destination)
        .ok_or(GpuError::Invalid(
            "vkUpdateDescriptorSets: unknown destination set",
        ))?;
    for index in 0..count {
        let source_element = source_element
            .checked_add(index)
            .ok_or(GpuError::OutOfBounds)?;
        let destination_element = destination_element
            .checked_add(index)
            .ok_or(GpuError::OutOfBounds)?;
        let source_key = (source_binding, source_element);
        let destination_key = (destination_binding, destination_element);
        if let Some(value) = source.buffers.get(&source_key) {
            destination.buffers.insert(destination_key, *value);
        } else if let Some(value) = source.images.get(&source_key) {
            destination.images.insert(destination_key, *value);
        } else if let Some(value) = source.texel_buffers.get(&source_key) {
            destination.texel_buffers.insert(destination_key, *value);
        } else {
            return Err(GpuError::Invalid(
                "vkUpdateDescriptorSets: source descriptor is unbound",
            ));
        }
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
            update_descriptor_buffer_element(
                dev,
                set,
                e.dst_binding,
                e.dst_array_element
                    .checked_add(i as u32)
                    .ok_or(GpuError::OutOfBounds)?,
                bi.buffer,
                bi.offset,
                bi.range,
            )?;
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
