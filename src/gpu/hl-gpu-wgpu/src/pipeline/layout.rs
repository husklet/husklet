use super::*;

/// Reconcile the binding TYPE the same `(group, binding)` slot declares in the two graphics stages. Equal
/// types pass through; two storage buffers merge to the WIDER access (writable subsumes read-only). Any
/// other disagreement (buffer vs texture, uniform vs storage, texture-shape mismatch, …) is a genuine
/// shader bug across stages and yields `None` so the caller reports it rather than guessing.
pub(super) struct BindingLayout;
impl BindingLayout {
    pub(super) fn reconcile(a: BindingKind, b: BindingKind) -> Option<BindingKind> {
        match (a, b) {
            (
                BindingKind::StorageBuffer { read_only: r1 },
                BindingKind::StorageBuffer { read_only: r2 },
            ) => Some(BindingKind::StorageBuffer {
                read_only: r1 && r2,
            }),
            _ if a == b => Some(a),
            _ => None,
        }
    }

    /// Lower a neutral [`BindingKind`] to the `wgpu::BindingType` a `BindGroupLayoutEntry` carries. Buffers use
    /// `min_binding_size: None` (so a per-stage size disagreement never rejects the layout — the shader's own
    /// access is validated against the module, not the layout) and no dynamic offset (the shim bakes offsets
    /// into each `BindResource::Buffer.offset`).
    pub(super) fn binding_type(kind: BindingKind) -> wgpu::BindingType {
        match kind {
            BindingKind::UniformBuffer => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            BindingKind::StorageBuffer { read_only } => wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            BindingKind::Texture { dim, sample, multi } => wgpu::BindingType::Texture {
                sample_type: match sample {
                    TexSample::Float { filterable } => {
                        wgpu::TextureSampleType::Float { filterable }
                    }
                    TexSample::Sint => wgpu::TextureSampleType::Sint,
                    TexSample::Uint => wgpu::TextureSampleType::Uint,
                    TexSample::Depth => wgpu::TextureSampleType::Depth,
                },
                view_dimension: match dim {
                    TexDim::D1 => wgpu::TextureViewDimension::D1,
                    TexDim::D2 => wgpu::TextureViewDimension::D2,
                    TexDim::D2Array => wgpu::TextureViewDimension::D2Array,
                    TexDim::D3 => wgpu::TextureViewDimension::D3,
                    TexDim::Cube => wgpu::TextureViewDimension::Cube,
                    TexDim::CubeArray => wgpu::TextureViewDimension::CubeArray,
                },
                multisampled: multi,
            },
            BindingKind::StorageTexture {
                dim,
                format,
                access,
            } => wgpu::BindingType::StorageTexture {
                access: match access {
                    naga::StorageAccess::LOAD => wgpu::StorageTextureAccess::ReadOnly,
                    naga::StorageAccess::STORE => wgpu::StorageTextureAccess::WriteOnly,
                    access
                        if access.contains(naga::StorageAccess::LOAD)
                            && access.contains(naga::StorageAccess::STORE) =>
                    {
                        wgpu::StorageTextureAccess::ReadWrite
                    }
                    _ => wgpu::StorageTextureAccess::Atomic,
                },
                format: storage_format(format),
                view_dimension: match dim {
                    TexDim::D1 => wgpu::TextureViewDimension::D1,
                    TexDim::D2 => wgpu::TextureViewDimension::D2,
                    TexDim::D2Array => wgpu::TextureViewDimension::D2Array,
                    TexDim::D3 => wgpu::TextureViewDimension::D3,
                    TexDim::Cube => wgpu::TextureViewDimension::Cube,
                    TexDim::CubeArray => wgpu::TextureViewDimension::CubeArray,
                },
            },
            BindingKind::Sampler { comparison } => wgpu::BindingType::Sampler(if comparison {
                wgpu::SamplerBindingType::Comparison
            } else {
                wgpu::SamplerBindingType::Filtering
            }),
        }
    }
}

fn storage_format(format: naga::StorageFormat) -> wgpu::TextureFormat {
    use naga::StorageFormat as S;
    use wgpu::TextureFormat as T;
    match format {
        S::R8Unorm => T::R8Unorm,
        S::R8Snorm => T::R8Snorm,
        S::R8Uint => T::R8Uint,
        S::R8Sint => T::R8Sint,
        S::R16Uint => T::R16Uint,
        S::R16Sint => T::R16Sint,
        S::R16Float => T::R16Float,
        S::Rg8Unorm => T::Rg8Unorm,
        S::Rg8Snorm => T::Rg8Snorm,
        S::Rg8Uint => T::Rg8Uint,
        S::Rg8Sint => T::Rg8Sint,
        S::R32Uint => T::R32Uint,
        S::R32Sint => T::R32Sint,
        S::R32Float => T::R32Float,
        S::Rg16Uint => T::Rg16Uint,
        S::Rg16Sint => T::Rg16Sint,
        S::Rg16Float => T::Rg16Float,
        S::Rgba8Unorm => T::Rgba8Unorm,
        S::Rgba8Snorm => T::Rgba8Snorm,
        S::Rgba8Uint => T::Rgba8Uint,
        S::Rgba8Sint => T::Rgba8Sint,
        S::Bgra8Unorm => T::Bgra8Unorm,
        S::Rgb10a2Uint => T::Rgb10a2Uint,
        S::Rgb10a2Unorm => T::Rgb10a2Unorm,
        S::Rg11b10Ufloat => T::Rg11b10Ufloat,
        S::R64Uint => T::R64Uint,
        S::Rg32Uint => T::Rg32Uint,
        S::Rg32Sint => T::Rg32Sint,
        S::Rg32Float => T::Rg32Float,
        S::Rgba16Uint => T::Rgba16Uint,
        S::Rgba16Sint => T::Rgba16Sint,
        S::Rgba16Float => T::Rgba16Float,
        S::Rgba32Uint => T::Rgba32Uint,
        S::Rgba32Sint => T::Rgba32Sint,
        S::Rgba32Float => T::Rgba32Float,
        S::R16Unorm => T::R16Unorm,
        S::R16Snorm => T::R16Snorm,
        S::Rg16Unorm => T::Rg16Unorm,
        S::Rg16Snorm => T::Rg16Snorm,
        S::Rgba16Unorm => T::Rgba16Unorm,
        S::Rgba16Snorm => T::Rgba16Snorm,
    }
}

/// Map the protocol's opaque WebGPU depth-compare code (carried through the neutral [`compare`] constants,
/// Vulkan `VkCompareOp` ordering) to a `wgpu::CompareFunction`. An unrecognized code is treated as
/// `Always` — matching the CPU oracle's `compare::passes`, which never hard-fails a draw on a code it does
/// not model.
pub(super) struct CompareFunction(pub(super) u32);

impl CompareFunction {
    pub(super) fn native(self) -> wgpu::CompareFunction {
        use wgpu::CompareFunction as C;
        match self.0 {
            compare::NEVER => C::Never,
            compare::LESS => C::Less,
            compare::EQUAL => C::Equal,
            compare::LESS_EQUAL => C::LessEqual,
            compare::GREATER => C::Greater,
            compare::NOT_EQUAL => C::NotEqual,
            compare::GREATER_EQUAL => C::GreaterEqual,
            _ => C::Always, // compare::ALWAYS and any unmodeled code
        }
    }
}

/// Map the protocol's opaque stencil-operation code (the neutral [`stencil_op`] numbering, Vulkan
/// `VkStencilOp` ordering) to a `wgpu::StencilOperation`. An unrecognized code is treated as `Keep`,
/// mirroring `compare_function`'s `Always` fallback — an honest bring-up never hard-fails on a code it does
/// not model, it just leaves the stencil untouched.
pub(super) struct StencilState;
impl StencilState {
    pub(super) fn operation(code: u32) -> wgpu::StencilOperation {
        use wgpu::StencilOperation as S;
        match code {
            stencil_op::ZERO => S::Zero,
            stencil_op::REPLACE => S::Replace,
            stencil_op::INCREMENT_CLAMP => S::IncrementClamp,
            stencil_op::DECREMENT_CLAMP => S::DecrementClamp,
            stencil_op::INVERT => S::Invert,
            stencil_op::INCREMENT_WRAP => S::IncrementWrap,
            stencil_op::DECREMENT_WRAP => S::DecrementWrap,
            _ => S::Keep, // stencil_op::KEEP and any unmodeled code
        }
    }

    /// Lower one protocol [`StencilFaceState`] (opaque compare + the three stencil ops) into a
    /// `wgpu::StencilFaceState`. Front+back both `DISABLED` collapse to `wgpu::StencilFaceState::IGNORE`.
    pub(super) fn face(f: &StencilFaceState) -> wgpu::StencilFaceState {
        wgpu::StencilFaceState {
            compare: CompareFunction(f.compare).native(),
            fail_op: Self::operation(f.fail_op),
            depth_fail_op: Self::operation(f.depth_fail_op),
            pass_op: Self::operation(f.pass_op),
        }
    }
}

/// Decode a protocol blend-factor wire value into a `wgpu::BlendFactor`. The wire numbering is the neutral
/// one the GL driver emits from `glBlendFunc`/`glBlendFuncSeparate` (`hl-gl` `blend_factor_wire`):
/// 0=ZERO 1=ONE 2=SRC_COLOR 3=1-SRC_COLOR 4=SRC_ALPHA 5=1-SRC_ALPHA 6=DST_COLOR 7=1-DST_COLOR
/// 8=DST_ALPHA 9=1-DST_ALPHA 10=SRC_ALPHA_SATURATE 11=CONSTANT 12=1-CONSTANT
/// 13=SRC1_COLOR 14=1-SRC1_COLOR 15=SRC1_ALPHA 16=1-SRC1_ALPHA. Every value the protocol can carry maps
/// to a concrete wgpu factor; an unmodeled code defaults to `One` (matching the GL driver's own fallback)
/// rather than silently dropping the blend.
pub(super) struct BlendState;
impl BlendState {
    pub(super) fn factor(code: u32) -> wgpu::BlendFactor {
        use hl_gpu::protocol::model::enums::blend_factor;
        use wgpu::BlendFactor as F;
        match code {
            blend_factor::ZERO => F::Zero,
            blend_factor::ONE => F::One,
            blend_factor::SRC_COLOR => F::Src,
            blend_factor::ONE_MINUS_SRC_COLOR => F::OneMinusSrc,
            blend_factor::SRC_ALPHA => F::SrcAlpha,
            blend_factor::ONE_MINUS_SRC_ALPHA => F::OneMinusSrcAlpha,
            blend_factor::DST_COLOR => F::Dst,
            blend_factor::ONE_MINUS_DST_COLOR => F::OneMinusDst,
            blend_factor::DST_ALPHA => F::DstAlpha,
            blend_factor::ONE_MINUS_DST_ALPHA => F::OneMinusDstAlpha,
            blend_factor::SRC_ALPHA_SATURATE => F::SrcAlphaSaturated,
            blend_factor::CONSTANT => F::Constant,
            blend_factor::ONE_MINUS_CONSTANT => F::OneMinusConstant,
            blend_factor::SRC1_COLOR => F::Src1,
            blend_factor::ONE_MINUS_SRC1_COLOR => F::OneMinusSrc1,
            blend_factor::SRC1_ALPHA => F::Src1Alpha,
            blend_factor::ONE_MINUS_SRC1_ALPHA => F::OneMinusSrc1Alpha,
            _ => F::One,
        }
    }

    /// Decode a protocol blend-op wire value into a `wgpu::BlendOperation`. The wire numbering is the neutral
    /// one the GL driver emits from `glBlendEquation` (`hl-gl` `blend_op_wire`): 0=ADD 1=SUBTRACT
    /// 2=REVERSE_SUBTRACT 3=MIN 4=MAX. An unmodeled code defaults to `Add`.
    pub(super) fn operation(code: u32) -> wgpu::BlendOperation {
        use wgpu::BlendOperation as O;
        match code {
            1 => O::Subtract,
            2 => O::ReverseSubtract,
            3 => O::Min,
            4 => O::Max,
            _ => O::Add,
        }
    }

    /// Lower a protocol [`BlendState`] into a `wgpu::BlendState`, translating the separate color/alpha
    /// src+dst factors and equations. A target whose protocol blend is `None` is an opaque replace, which
    /// wgpu represents as `blend: None` on the color target.
    pub(super) fn lower(b: &hl_gpu::protocol::model::descriptor::BlendState) -> wgpu::BlendState {
        wgpu::BlendState {
            color: wgpu::BlendComponent {
                src_factor: Self::factor(b.src_color),
                dst_factor: Self::factor(b.dst_color),
                operation: Self::operation(b.op_color),
            },
            alpha: wgpu::BlendComponent {
                src_factor: Self::factor(b.src_alpha),
                dst_factor: Self::factor(b.dst_alpha),
                operation: Self::operation(b.op_alpha),
            },
        }
    }
}
