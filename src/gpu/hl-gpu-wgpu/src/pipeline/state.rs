use super::*;

/// Constructs compute storage-buffer layout entries.
pub(super) struct ComputeLayout;
impl ComputeLayout {
    pub(super) fn storage(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
        wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }
    }
}

pub(super) struct PrimitiveTopology(pub(super) Topology);

impl PrimitiveTopology {
    pub(super) fn native(self) -> wgpu::PrimitiveTopology {
        match self.0 {
            Topology::PointList => wgpu::PrimitiveTopology::PointList,
            Topology::LineList => wgpu::PrimitiveTopology::LineList,
            Topology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
            Topology::TriangleList => wgpu::PrimitiveTopology::TriangleList,
            Topology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
        }
    }
}

/// Map the protocol's `cull` code (`RenderPipelineDesc::cull`: 0 = none, 1 = front, 2 = back — the GL
/// driver's `glCullFace`/`GL_CULL_FACE` state) to wgpu's optional culled face. `0` (the neutral wire
/// default, and the only value the frozen suite used) is `None` — byte-for-byte the previous `..default()`
/// behavior; a real `glCullFace(GL_BACK)` guest now actually culls instead of the state silently vanishing.
pub(super) struct CullMode(pub(super) u32);

impl CullMode {
    /// An UNRECOGNISED code is REFUSED rather than absorbed into `None`. Absorbing it made a malformed
    /// value and a deliberate "no culling" the same observation, so a guest whose cull state failed to
    /// encode would render un-culled geometry and report success — the sibling `compare` code has been
    /// range-checked all along (`sampler.rs`, and again in the runtime's `validate`), and the difference
    /// between the two was an oversight rather than a policy.
    pub(super) fn native(self) -> Result<Option<wgpu::Face>> {
        match self.0 {
            0 => Ok(None),
            1 => Ok(Some(wgpu::Face::Front)),
            2 => Ok(Some(wgpu::Face::Back)),
            _ => Err(GpuError::Invalid("wgpu: unsupported cull mode")),
        }
    }
}

/// Map the protocol's `front_face` code (`RenderPipelineDesc::front_face`: 0 = CCW, 1 = CW — the GL
/// driver's `glFrontFace`) to a `wgpu::FrontFace`. `0` (the neutral default) is `Ccw`, identical to the
/// previous hardcoded default; it only changes an observable result together with a non-zero `cull`.
pub(super) struct FrontFace(pub(super) u32);

impl FrontFace {
    /// Refused on an unrecognised code, for the reason given on [`CullMode::native`]: silently answering
    /// `Ccw` made a malformed winding indistinguishable from the default one.
    pub(super) fn native(self) -> Result<wgpu::FrontFace> {
        match self.0 {
            0 => Ok(wgpu::FrontFace::Ccw),
            1 => Ok(wgpu::FrontFace::Cw),
            _ => Err(GpuError::Invalid("wgpu: unsupported front face winding")),
        }
    }
}

/// Map the protocol's RGBA `write_mask` (`ColorTargetState::write_mask`, low 4 bits `R<<0|G<<1|B<<2|A<<3` —
/// the GL driver's `glColorMask`) to `wgpu::ColorWrites`. `0xF` (the neutral default) is `ALL`, identical to
/// the previous hardcoded value; a guest that masks a channel (e.g. `glColorMask(1,1,1,0)` to preserve the
/// destination alpha) now actually leaves that channel untouched instead of the mask silently vanishing.
pub(super) struct ColorMask(pub(super) u32);

impl ColorMask {
    pub(super) fn native(self) -> wgpu::ColorWrites {
        let mut w = wgpu::ColorWrites::empty();
        if self.0 & 1 != 0 {
            w |= wgpu::ColorWrites::RED;
        }
        if self.0 & 2 != 0 {
            w |= wgpu::ColorWrites::GREEN;
        }
        if self.0 & 4 != 0 {
            w |= wgpu::ColorWrites::BLUE;
        }
        if self.0 & 8 != 0 {
            w |= wgpu::ColorWrites::ALPHA;
        }
        w
    }
}
