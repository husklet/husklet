//! Native samplers. A thin map of the protocol [`SamplerDesc`] filter/address modes onto `wgpu::Sampler`.
//! Not exercised by the current conformance suite, but wired so `CreateSampler` routes to a real object.

use hl_gpu::protocol::model::descriptor::SamplerDesc;
use hl_gpu::protocol::model::enums::{AddressMode, Filter};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::WgpuExecutor;

/// Downcast a live sampler id to its native handle.
pub fn native<'a>(res: &'a SessionResources, id: u32) -> Result<&'a wgpu::Sampler> {
    res.samplers
        .get(id)?
        .downcast_ref::<wgpu::Sampler>()
        .ok_or(GpuError::Invalid("wgpu: sampler native type mismatch"))
}

fn filter(f: Filter) -> wgpu::FilterMode {
    match f {
        Filter::Nearest => wgpu::FilterMode::Nearest,
        Filter::Linear => wgpu::FilterMode::Linear,
    }
}

fn address(a: AddressMode) -> wgpu::AddressMode {
    match a {
        AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        AddressMode::Repeat => wgpu::AddressMode::Repeat,
        AddressMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
    }
}

impl WgpuExecutor {
    pub(crate) fn create_sampler(
        &self,
        res: &mut SessionResources,
        id: u32,
        d: &SamplerDesc,
    ) -> Result<()> {
        let s = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hl-sampler"),
            address_mode_u: address(d.address_u),
            address_mode_v: address(d.address_v),
            address_mode_w: address(d.address_w),
            mag_filter: filter(d.mag_filter),
            min_filter: filter(d.min_filter),
            mipmap_filter: filter(d.mip_filter),
            ..Default::default()
        });
        res.samplers.insert(id, Box::new(s))
    }
}
