use super::*;

/// Per-slot vertex step mode: `step_mode == 0` steps per-vertex, non-zero steps per-instance (the encoding
/// the GL driver emits from `glVertexAttribDivisor`).
pub(super) struct VertexState;
impl VertexState {
    pub(super) fn step_mode(vl: &VertexLayout) -> wgpu::VertexStepMode {
        if vl.step_mode == 0 {
            wgpu::VertexStepMode::Vertex
        } else {
            wgpu::VertexStepMode::Instance
        }
    }

    /// Decode a protocol vertex-attribute format into a `wgpu::VertexFormat`. The wire packs
    /// `comps | (kind<<8) | (normalized<<16) | (integer<<17)` (the GL driver's `vertex_format_wire`): `comps`
    /// in 1..=4, `kind` 0=f32 1=u8 2=i8 3=u16 4=i16 5=u32 6=i32 7=f16. WebGPU has no 1-/3-component 8-/16-bit
    /// formats, so those combinations are rejected honestly rather than silently widened.
    pub(crate) fn format(packed: u32) -> Result<wgpu::VertexFormat> {
        use wgpu::VertexFormat as F;
        let comps = packed & 0xff;
        let kind = (packed >> 8) & 0xff;
        let normalized = (packed >> 16) & 1 != 0;
        let bad = || GpuError::Unsupported("wgpu: unsupported vertex attribute format");
        Ok(match (kind, comps) {
            // 32-bit float
            (0, 1) => F::Float32,
            (0, 2) => F::Float32x2,
            (0, 3) => F::Float32x3,
            (0, 4) => F::Float32x4,
            // 32-bit unsigned / signed integer
            (5, 1) => F::Uint32,
            (5, 2) => F::Uint32x2,
            (5, 3) => F::Uint32x3,
            (5, 4) => F::Uint32x4,
            (6, 1) => F::Sint32,
            (6, 2) => F::Sint32x2,
            (6, 3) => F::Sint32x3,
            (6, 4) => F::Sint32x4,
            // 16-bit float (x2 / x4 only)
            (7, 2) => F::Float16x2,
            (7, 4) => F::Float16x4,
            (8, 4) if normalized => F::Unorm10_10_10_2,
            // 8-bit (x2 / x4 only), normalized → Unorm/Snorm else Uint/Sint
            (1, 2) => {
                if normalized {
                    F::Unorm8x2
                } else {
                    F::Uint8x2
                }
            }
            (1, 4) => {
                if normalized {
                    F::Unorm8x4
                } else {
                    F::Uint8x4
                }
            }
            (2, 2) => {
                if normalized {
                    F::Snorm8x2
                } else {
                    F::Sint8x2
                }
            }
            (2, 4) => {
                if normalized {
                    F::Snorm8x4
                } else {
                    F::Sint8x4
                }
            }
            // 16-bit integer (x2 / x4 only), normalized → Unorm/Snorm else Uint/Sint
            (3, 2) => {
                if normalized {
                    F::Unorm16x2
                } else {
                    F::Uint16x2
                }
            }
            (3, 4) => {
                if normalized {
                    F::Unorm16x4
                } else {
                    F::Uint16x4
                }
            }
            (4, 2) => {
                if normalized {
                    F::Snorm16x2
                } else {
                    F::Sint16x2
                }
            }
            (4, 4) => {
                if normalized {
                    F::Snorm16x4
                } else {
                    F::Sint16x4
                }
            }
            _ => return Err(bad()),
        })
    }
}
