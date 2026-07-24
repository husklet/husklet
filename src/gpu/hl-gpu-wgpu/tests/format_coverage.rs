//! DEMO — exhaustive `TextureFormat` coverage on the wgpu executor, proven with EXACT stored bytes.
//!
//! The srgb_target demo proved the two sRGB formats round-trip; the differential fuzzer and the rest of
//! the suite lean almost entirely on `Rgba8Unorm`. This binary closes the gap by sweeping EVERY format the
//! executor advertises in its `Capabilities::texture_formats` bitset and proving each one materializes and
//! reads back the exact bytes its layout demands — channel order (RGBA vs BGRA), gamma encoding (sRGB), and
//! float bit-patterns (f16/f32) all confronted, plus the two depth formats driven as real depth attachments.
//!
//! The formats the protocol advertises (`hl_gpu::protocol::model::capability`):
//!   COLOR_FORMATS: Rgba8Unorm, Bgra8Unorm, Rgba8Srgb, Bgra8Srgb, R8Unorm, Rg8Unorm, Rgba16Float,
//!                  Rgba32Float, R32Float
//!   DEPTH:         Depth32Float (DEPTH_FORMATS) + Depth24PlusStencil8 (this executor lowers stencil, so it
//!                  additionally advertises the combined depth+stencil format — see `capabilities_for`).
//!
//! METHOD (color): a fullscreen triangle whose fragment shader outputs a CONSTANT linear color
//! `C = (0.75, 0.5, 0.25, 1.0)` is drawn into a fresh target of each format, then the target is read back
//! RAW (`copy_texture_to_buffer` copies encoded texels verbatim — no decode) and the tight bytes are
//! asserted against the format's exact layout. This is the same proven gamma-on-write path srgb_target uses;
//! the ONLY variable is the target format, so any mishandling in `convert::texture_format` /
//! `convert::texel_bytes` / `texture::make_texture` / readback surfaces as wrong bytes.
//!
//! sRGB TRANSFER FUNCTION (IEC 61966-2-1 sRGB OETF — the encode wgpu/lavapipe applies on write to an sRGB
//! target; alpha is NEVER encoded):
//!     V = 12.92 · L                       for L ≤ 0.0031308
//!     V = 1.055 · L^(1/2.4) − 0.055       for L  > 0.0031308
//! For the swept color: sRGB8(0.75)=225, sRGB8(0.5)=188, sRGB8(0.25)=137 (each computed live below, ±2 for
//! lavapipe rounding). A naive "sRGB == Unorm" backend would instead store 191/128/64 — the wide gap is the
//! proof the encode actually happened.
//!
//! METHOD (sample/swizzle): a raw texel is uploaded into a 1×1 sampled texture of a BGRA / sRGB format and
//! sampled (nearest) into a LINEAR `Rgba8Unorm` target — proving BGR channels swizzle to RGB and sRGB texels
//! decode to linear on the sample, where a plain Unorm texel passes through untouched.
//!
//! METHOD (depth): each depth format backs a real depth attachment; three fullscreen draws at different
//! depths with a `LESS` test prove the NEAREST fragment occludes the farther ones regardless of draw order
//! (the control re-runs with the test forced `ALWAYS`, where the LAST-drawn — farthest — fragment wins).

mod gpu_harness;
use gpu_harness::*;

use hl_gpu::protocol::model::capability::{COLOR_FORMATS, DEPTH_FORMATS};
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    DepthAttachment, DepthState, RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, compare, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat,
    Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// The constant LINEAR color every color-format draw emits. All three RGB components are exactly
/// representable in f16 AND f32 (0.75 = 3/4, 0.5, 0.25), so the float formats round-trip with zero error;
/// they are distinct so a channel-order bug (RGBA↔BGRA) cannot hide, and none is 0/1 so a dropped-channel
/// or clamp bug shows.
const C: [f32; 4] = [0.75, 0.5, 0.25, 1.0];

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS_CONST: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(0.75, 0.5, 0.25, 1.0); }
"#;
// Sample a 1×1 texture at its center into a LINEAR target (used to prove BGR swizzle + sRGB decode).
const FS_SAMPLE: &str = r#"#version 460
layout(set = 0, binding = 0) uniform texture2D t;
layout(set = 0, binding = 1) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler2D(t, s), vec2(0.5, 0.5)); }
"#;

// ---- transfer-function / packing references (independent of the executor under test) -------------------

/// Unorm8 encode with round-half-up — the WebGPU/Vulkan unorm store.
fn unorm8(l: f32) -> u8 {
    (l.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}
/// IEC 61966-2-1 sRGB OETF, then unorm8. The exact linear→sRGB8 encode a `*Srgb` target applies on write.
fn srgb8(l: f32) -> u8 {
    let l = l.clamp(0.0, 1.0);
    let v = if l <= 0.0031308 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0 + 0.5) as u8
}
/// Decode one IEEE-754 binary16 (half) to f32 — to confront the raw bytes an `Rgba16Float` target stores.
fn half_to_f32(h: u16) -> f32 {
    let sign = (h >> 15) & 1;
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x3ff) as f32;
    let mag = if exp == 0 {
        mant * 2f32.powi(-24) // subnormal
    } else if exp == 0x1f {
        f32::INFINITY
    } else {
        (1.0 + mant / 1024.0) * 2f32.powi(exp - 15)
    };
    if sign == 1 {
        -mag
    } else {
        mag
    }
}
fn le_u16(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn le_f32_at(b: &[u8]) -> f32 {
    f32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

fn tex(w: u32, h: u32, fmt: TextureFormat, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: fmt,
        usage,
        label: String::new(),
    }
}
fn ct(fmt: TextureFormat) -> ColorTargetState {
    ColorTargetState {
        format: fmt,
        blend: None,
        write_mask: 0xF,
    }
}

#[path = "format_coverage/capability.rs"]
mod capability;
#[path = "format_coverage/color.rs"]
mod color;
#[path = "format_coverage/depth.rs"]
mod depth;
#[path = "format_coverage/sample.rs"]
mod sample;
