//! Bind groups. The protocol creates a bind group independently of any pipeline, but wgpu binds resources
//! against a concrete `BindGroupLayout` that (with auto layouts) belongs to a pipeline. So the native
//! handle here is just the *descriptor*; the real `wgpu::BindGroup` is built at draw/dispatch time from
//! the currently-bound pipeline's own layout (`build`), keyed to the bindings the WGSL declares.

use std::num::NonZeroU64;

use hl_gpu::protocol::model::descriptor::{BindGroupDesc, BindResource};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::{buffer, texture, WgpuExecutor};

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
                BindResource::Texture { id } => {
                    texture::WgpuTexture::get(res, *id)?;
                }
                BindResource::Sampler { id } => {
                    res.samplers.get(*id)?;
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
    ) -> Result<wgpu::BindGroup> {
        let keep = |e: &hl_gpu::protocol::model::descriptor::BindEntry| {
            filter.map_or(true, |f| f.contains(&(d.set, e.binding)))
        };
        // The wgpu entries borrow the resolved views/samplers; collect those first so they outlive the
        // `BindGroupEntry` slice. Only resources for KEPT entries are resolved + bound, in order.
        let mut views = Vec::new();
        let mut samplers = Vec::new();
        for e in d.entries.iter().filter(|e| keep(e)) {
            match &e.resource {
                BindResource::Texture { id } => {
                    views.push(texture::WgpuTexture::get(res, *id)?.view.clone())
                }
                BindResource::Sampler { id } => samplers.push(self.sampler(res, *id)?.clone()),
                BindResource::Buffer { .. } => {}
            }
        }
        let mut vi = 0usize;
        let mut si = 0usize;
        let mut entries = Vec::with_capacity(d.entries.len());
        for e in d.entries.iter().filter(|e| keep(e)) {
            let resource = match &e.resource {
                BindResource::Buffer { id, offset, size } => {
                    let b = buffer::WgpuBuffer::get(res, *id)?;
                    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &b.buffer,
                        offset: *offset,
                        size: NonZeroU64::new(*size),
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
            };
            entries.push(wgpu::BindGroupEntry {
                binding: e.binding,
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
