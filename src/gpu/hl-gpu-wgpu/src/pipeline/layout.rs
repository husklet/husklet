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
            BindingKind::Sampler { comparison } => wgpu::BindingType::Sampler(if comparison {
                wgpu::SamplerBindingType::Comparison
            } else {
                wgpu::SamplerBindingType::Filtering
            }),
        }
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
/// 8=DST_ALPHA 9=1-DST_ALPHA 10=SRC_ALPHA_SATURATE 11=CONSTANT 12=1-CONSTANT. Every value the protocol can carry maps to a concrete
/// wgpu factor; an unmodeled code defaults to `One` (matching the GL driver's own fallback) rather than
/// silently dropping the blend.
pub(super) struct BlendState;
impl BlendState {
    pub(super) fn factor(code: u32) -> wgpu::BlendFactor {
        use wgpu::BlendFactor as F;
        match code {
            0 => F::Zero,
            1 => F::One,
            2 => F::Src,
            3 => F::OneMinusSrc,
            4 => F::SrcAlpha,
            5 => F::OneMinusSrcAlpha,
            6 => F::Dst,
            7 => F::OneMinusDst,
            8 => F::DstAlpha,
            9 => F::OneMinusDstAlpha,
            10 => F::SrcAlphaSaturated,
            11 => F::Constant,
            12 => F::OneMinusConstant,
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
