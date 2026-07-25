//! Native samplers. A thin map of the protocol [`SamplerDesc`] filter/address modes onto `wgpu::Sampler`.
//! Not exercised by the current conformance suite, but wired so `CreateSampler` routes to a real object.

use hl_gpu::protocol::model::descriptor::SamplerDesc;
use hl_gpu::protocol::model::enums::{AddressMode, Filter};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::WgpuExecutor;

impl WgpuExecutor {
    /// Downcast a live sampler id to its native handle.
    pub(crate) fn sampler<'a>(
        &self,
        resources: &'a SessionResources,
        id: u32,
    ) -> Result<&'a wgpu::Sampler> {
        resources
            .samplers
            .get(id)?
            .downcast_ref::<wgpu::Sampler>()
            .ok_or(GpuError::Invalid("wgpu: sampler native type mismatch"))
    }

    pub(crate) fn create_sampler(
        &self,
        res: &mut SessionResources,
        id: u32,
        d: &SamplerDesc,
    ) -> Result<()> {
        let s = self.gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("hl-sampler"),
            address_mode_u: match d.address_u {
                AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
                AddressMode::Repeat => wgpu::AddressMode::Repeat,
                AddressMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
            },
            address_mode_v: match d.address_v {
                AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
                AddressMode::Repeat => wgpu::AddressMode::Repeat,
                AddressMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
            },
            address_mode_w: match d.address_w {
                AddressMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
                AddressMode::Repeat => wgpu::AddressMode::Repeat,
                AddressMode::MirrorRepeat => wgpu::AddressMode::MirrorRepeat,
            },
            mag_filter: match d.mag_filter {
                Filter::Nearest => wgpu::FilterMode::Nearest,
                Filter::Linear => wgpu::FilterMode::Linear,
            },
            min_filter: match d.min_filter {
                Filter::Nearest => wgpu::FilterMode::Nearest,
                Filter::Linear => wgpu::FilterMode::Linear,
            },
            mipmap_filter: match d.mip_filter {
                Filter::Nearest => wgpu::FilterMode::Nearest,
                Filter::Linear => wgpu::FilterMode::Linear,
            },
            ..Default::default()
        });
        res.samplers.insert(id, Box::new(s))
    }
}
