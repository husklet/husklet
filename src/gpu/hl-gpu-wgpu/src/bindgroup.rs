//! Bind groups. The protocol creates a bind group independently of any pipeline, but wgpu binds resources
//! against a concrete `BindGroupLayout` that (with auto layouts) belongs to a pipeline. So the native
//! handle here is just the *descriptor*; the real `wgpu::BindGroup` is built at draw/dispatch time from
//! the currently-bound pipeline's own layout (`build`), keyed to the bindings the WGSL declares.

use std::num::NonZeroU64;

use hl_gpu::protocol::model::descriptor::{BindGroupDesc, BindResource};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::{buffer, texture, WgpuExecutor};
use wgpu::util::DeviceExt;

const SAMPLER_METADATA_WORDS: usize = 8;

fn encode_sampler_desc(desc: &hl_gpu::protocol::model::descriptor::SamplerDesc) -> [u32; SAMPLER_METADATA_WORDS] {
    use hl_gpu::protocol::model::enums::{AddressMode, Filter};
    let address = |mode| match mode {
        AddressMode::ClampToEdge => 0,
        AddressMode::Repeat => 1,
        AddressMode::MirrorRepeat => 2,
        AddressMode::MirrorClampToEdge => 3,
    };
    let filter = |mode| match mode {
        Filter::Nearest => 0,
        Filter::Linear => 1,
        Filter::Cubic => 2,
    };
    [
        filter(desc.min_filter), filter(desc.mag_filter), filter(desc.mip_filter),
        address(desc.address_u), address(desc.address_v), address(desc.address_w),
        desc.lod_min_clamp.to_bits(), desc.lod_max_clamp.to_bits(),
    ]
}

fn sampler_metadata_words(
    exec: &WgpuExecutor,
    res: &SessionResources,
    d: &BindGroupDesc,
    layout: &crate::reflect::SamplerMetadataLayout,
    remap_group_zero: bool,
) -> Result<Vec<u32>> {
    let mut words = vec![0; layout.samplers.iter().map(|slot| slot.base_ordinal + slot.count).max().unwrap_or(0) as usize * SAMPLER_METADATA_WORDS];
    for slot in &layout.samplers {
        let guest_binding = if remap_group_zero && d.set == 0 {
            slot.binding.checked_sub(crate::wgsl::viewport::GUEST_OFFSET).ok_or(GpuError::Invalid("wgpu: invalid sampler metadata binding shift"))?
        } else { slot.binding };
        let entry = d.entries.iter().find(|entry| entry.binding == guest_binding)
            .ok_or(GpuError::Invalid("wgpu: sampler metadata binding is absent"))?;
        let ids: Vec<u32> = match &entry.resource {
            BindResource::Sampler { id } => vec![*id],
            BindResource::SamplerArray { ids } => ids.clone(),
            _ => return Err(GpuError::Invalid("wgpu: sampler metadata slot is not a sampler")),
        };
        if ids.len() != slot.count as usize {
            return Err(GpuError::Invalid("wgpu: sampler metadata array count mismatch"));
        }
        for (index, id) in ids.into_iter().enumerate() {
            let desc = exec.sampler_desc(res, id)?;
            let base = (slot.base_ordinal as usize + index) * SAMPLER_METADATA_WORDS;
            words[base..base + SAMPLER_METADATA_WORDS].copy_from_slice(&encode_sampler_desc(desc));
        }
    }
    Ok(words)
}

#[cfg(test)]
mod sampler_metadata_tests {
    use super::*;
    use hl_gpu::protocol::model::descriptor::SamplerDesc;
    use hl_gpu::protocol::model::enums::{AddressMode, Filter};

    #[test]
    fn encoding_preserves_cubic_address_and_lod_state() {
        let desc = SamplerDesc {
            min_filter: Filter::Cubic,
            mag_filter: Filter::Linear,
            mip_filter: Filter::Nearest,
            address_u: AddressMode::Repeat,
            address_v: AddressMode::MirrorRepeat,
            address_w: AddressMode::ClampToEdge,
            lod_min_clamp: 1.25,
            lod_max_clamp: 7.5,
            ..SamplerDesc::default()
        };
        assert_eq!(encode_sampler_desc(&desc), [2, 1, 0, 1, 2, 0, 1.25f32.to_bits(), 7.5f32.to_bits()]);
    }
}

impl WgpuExecutor {
    /// Downcast a live bind-group id to its stored descriptor.
    pub(crate) fn bind_group<'a>(
        &self,
        resources: &'a SessionResources,
        id: u32,
    ) -> Result<&'a BindGroupDesc> {
        resources
            .bind_groups
            .get(id)?
            .downcast_ref::<BindGroupDesc>()
            .ok_or(GpuError::Invalid("wgpu: bind-group native type mismatch"))
    }

    /// Record a bind group: store its descriptor after checking every referenced resource is live.
    pub(crate) fn create_bind_group(
        &self,
        res: &mut SessionResources,
        id: u32,
        d: &BindGroupDesc,
    ) -> Result<()> {
        for e in &d.entries {
            match &e.resource {
                BindResource::Buffer { id, .. } => {
                    buffer::WgpuBuffer::get(res, *id)?;
                }
                BindResource::TexelBuffer { id, .. } => {
                    buffer::WgpuBuffer::get(res, *id)?;
                }
                BindResource::Texture { id } => {
                    texture::WgpuTexture::get(res, *id)?;
                }
                BindResource::Sampler { id } => {
                    res.samplers.get(*id)?;
                }
                BindResource::BufferArray { elements } => {
                    for element in elements {
                        buffer::WgpuBuffer::get(res, element.id)?;
                    }
                }
                BindResource::TextureArray { ids } => {
                    for id in ids {
                        texture::WgpuTexture::get(res, *id)?;
                    }
                }
                BindResource::SamplerArray { ids } => {
                    for id in ids {
                        res.samplers.get(*id)?;
                    }
                }
            }
        }
        res.bind_groups.insert(id, Box::new(d.clone()))
    }

    /// Build a concrete `wgpu::BindGroup` for `d` against `layout` (the bound pipeline's group-0 layout).
    ///
    /// `filter` — when `Some` — restricts the emitted entries to the `(group, binding)` slots the bound
    /// pipeline's shaders actually READ (its auto layout's exact bindings; see
    /// `PipelineNative::Render.used_bindings`). The GL driver emits a bind-group entry per *bound* resource,
    /// which for a GskGpu program routinely includes textures/samplers the compiled shader never samples;
    /// without the filter the entry count (e.g. 5) would not match the auto layout (e.g. 3) and wgpu NACKs.
    /// Dropping an unsampled binding is semantically free — the shader never reads it. `None` keeps every
    /// entry (the compute path, whose bind groups already match their layout).
    pub(crate) fn build_bind_group(
        &self,
        res: &SessionResources,
        layout: &wgpu::BindGroupLayout,
        d: &BindGroupDesc,
        filter: Option<&[(u32, u32)]>,
        remap_group_zero: bool,
        texel_buffers: Option<&[crate::texel_buffer::View]>,
        internal: Option<&wgpu::Buffer>,
        sampler_metadata: Option<&crate::reflect::SamplerMetadataLayout>,
    ) -> Result<wgpu::BindGroup> {
        let binding = |binding: u32| -> Result<u32> {
            if remap_group_zero && d.set == 0 {
                binding
                    .checked_add(crate::wgsl::viewport::GUEST_OFFSET)
                    .ok_or(GpuError::OutOfBounds)
            } else {
                Ok(binding)
            }
        };
        let mut kept = Vec::new();
        for entry in &d.entries {
            let native = binding(entry.binding)?;
            if filter.is_none_or(|slots| slots.contains(&(d.set, native))) {
                kept.push((entry, native));
            }
        }
        // The wgpu entries borrow the resolved views/samplers; collect those first so they outlive the
        // `BindGroupEntry` slice. Only resources for KEPT entries are resolved + bound, in order.
        let mut views = Vec::new();
        let mut samplers = Vec::new();
        let mut view_arrays = Vec::new();
        let mut sampler_arrays = Vec::new();
        let mut buffer_arrays = Vec::new();
        for (e, _) in &kept {
            match &e.resource {
                BindResource::Texture { id } => {
                    views.push(texture::WgpuTexture::get(res, *id)?.view.clone())
                }
                BindResource::Sampler { id } => samplers.push(self.sampler(res, *id)?.clone()),
                BindResource::Buffer { .. } | BindResource::TexelBuffer { .. } => {}
                BindResource::TextureArray { ids } => view_arrays.push(
                    ids.iter()
                        .map(|id| Ok(texture::WgpuTexture::get(res, *id)?.view.clone()))
                        .collect::<Result<Vec<_>>>()?,
                ),
                BindResource::SamplerArray { ids } => sampler_arrays.push(
                    ids.iter()
                        .map(|id| Ok(self.sampler(res, *id)?.clone()))
                        .collect::<Result<Vec<_>>>()?,
                ),
                BindResource::BufferArray { elements } => buffer_arrays.push(
                    elements
                        .iter()
                        .map(|element| {
                            let buffer = buffer::WgpuBuffer::get(res, element.id)?;
                            Ok(wgpu::BufferBinding {
                                buffer: &buffer.buffer,
                                offset: element.offset,
                                size: NonZeroU64::new(element.size),
                            })
                        })
                        .collect::<Result<Vec<_>>>()?,
                ),
            }
        }
        let view_array_refs = view_arrays
            .iter()
            .map(|array| array.iter().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let sampler_array_refs = sampler_arrays
            .iter()
            .map(|array| array.iter().collect::<Vec<_>>())
            .collect::<Vec<_>>();
        let mut vi = 0usize;
        let mut si = 0usize;
        let mut vai = 0usize;
        let mut sai = 0usize;
        let mut bai = 0usize;
        let metadata_buffer = sampler_metadata.map(|metadata| -> Result<wgpu::Buffer> {
            let words = sampler_metadata_words(self, res, d, metadata, remap_group_zero)?;
            Ok(self.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("hl-sampler-metadata"),
                contents: bytemuck::cast_slice(&words),
                usage: wgpu::BufferUsages::STORAGE,
            }))
        }).transpose()?;
        let mut entries = Vec::with_capacity(d.entries.len() + usize::from(internal.is_some()) + usize::from(metadata_buffer.is_some()));
        if let Some(buffer) = internal {
            entries.push(wgpu::BindGroupEntry {
                binding: crate::wgsl::viewport::BINDING,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer,
                    offset: 0,
                    size: NonZeroU64::new(16),
                }),
            });
        }
        if let (Some(metadata), Some(buffer)) = (sampler_metadata, metadata_buffer.as_ref()) {
            entries.push(wgpu::BindGroupEntry {
                binding: metadata.binding,
                resource: buffer.as_entire_binding(),
            });
        }
        for (e, native) in kept {
            let resource = match &e.resource {
                BindResource::Buffer { id, offset, size } => {
                    let b = buffer::WgpuBuffer::get(res, *id)?;
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &b.buffer,
                        offset: *offset,
                        size: NonZeroU64::new(*size),
                    })
                }
                BindResource::TexelBuffer { .. } => {
                    let view = texel_buffers
                        .and_then(|views| {
                            views.iter().find(|view| view.binding == e.binding)
                        })
                        .ok_or(GpuError::Unsupported(
                            "wgpu: texel-buffer view was not prepared for this pass",
                        ))?;
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &view.buffer,
                        offset: view.offset,
                        size: NonZeroU64::new(view.size),
                    })
                }
                BindResource::Texture { .. } => {
                    let v = &views[vi];
                    vi += 1;
                    wgpu::BindingResource::TextureView(v)
                }
                BindResource::Sampler { .. } => {
                    let s = &samplers[si];
                    si += 1;
                    wgpu::BindingResource::Sampler(s)
                }
                BindResource::TextureArray { .. } => {
                    let array = &view_array_refs[vai];
                    vai += 1;
                    wgpu::BindingResource::TextureViewArray(array)
                }
                BindResource::SamplerArray { .. } => {
                    let array = &sampler_array_refs[sai];
                    sai += 1;
                    wgpu::BindingResource::SamplerArray(array)
                }
                BindResource::BufferArray { .. } => {
                    let array = &buffer_arrays[bai];
                    bai += 1;
                    wgpu::BindingResource::BufferArray(array)
                }
            };
            entries.push(wgpu::BindGroupEntry {
                binding: native,
                resource,
            });
        }
        Ok(self
            .gpu
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("hl-bindgroup"),
                layout,
                entries: &entries,
            }))
    }
}
