//! Native samplers. A thin map of the protocol [`SamplerDesc`] filter/address modes onto `wgpu::Sampler`.
//! Not exercised by the current conformance suite, but wired so `CreateSampler` routes to a real object.

use hl_gpu::protocol::model::descriptor::SamplerDesc;
use hl_gpu::protocol::model::enums::{compare, AddressMode, Filter};
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
        if d.lod_min_clamp < 0.0 || d.lod_max_clamp < 0.0 {
            return Err(GpuError::Invalid(
                "wgpu: sampler LOD clamps must be non-negative",
            ));
        }
        if d.lod_min_clamp > d.lod_max_clamp {
            return Err(GpuError::Invalid(
                "wgpu: sampler minimum LOD exceeds maximum LOD",
            ));
        }
        let compare = d.compare.map(|function| match function {
            compare::NEVER => wgpu::CompareFunction::Never,
            compare::LESS => wgpu::CompareFunction::Less,
            compare::EQUAL => wgpu::CompareFunction::Equal,
            compare::LESS_EQUAL => wgpu::CompareFunction::LessEqual,
            compare::GREATER => wgpu::CompareFunction::Greater,
            compare::NOT_EQUAL => wgpu::CompareFunction::NotEqual,
            compare::GREATER_EQUAL => wgpu::CompareFunction::GreaterEqual,
            compare::ALWAYS => wgpu::CompareFunction::Always,
            _ => wgpu::CompareFunction::Always,
        });
        if d.compare.is_some_and(|function| function > compare::ALWAYS) {
            return Err(GpuError::Invalid("wgpu: unsupported sampler comparison"));
        }
        if [d.mag_filter, d.min_filter, d.mip_filter].contains(&Filter::Cubic) {
            return Err(GpuError::Unsupported(
                "wgpu: cubic sampler filtering is not implemented",
            ));
        }
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
                Filter::Cubic => unreachable!("cubic rejected above"),
            },
            min_filter: match d.min_filter {
                Filter::Nearest => wgpu::FilterMode::Nearest,
                Filter::Linear => wgpu::FilterMode::Linear,
                Filter::Cubic => unreachable!("cubic rejected above"),
            },
            mipmap_filter: match d.mip_filter {
                Filter::Nearest => wgpu::FilterMode::Nearest,
                Filter::Linear => wgpu::FilterMode::Linear,
                Filter::Cubic => unreachable!("cubic rejected above"),
            },
            lod_min_clamp: d.lod_min_clamp,
            lod_max_clamp: d.lod_max_clamp,
            compare,
            ..Default::default()
        });
        res.samplers.insert(id, Box::new(s))
    }
}
