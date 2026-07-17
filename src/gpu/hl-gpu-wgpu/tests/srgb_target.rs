//! DEMO — sRGB color-space handling on the wgpu executor, proven with exact bytes.
//!
//! The differential fuzzer (commit 98a5fd70) EXCLUDED sRGB targets as untested; this closes that gap. An
//! sRGB render target (`TextureFormat::Rgba8Srgb` → wgpu `Rgba8UnormSrgb`) means the GPU applies the
//! linear→sRGB (gamma) ENCODE when a fragment writes its output, and the sRGB→linear DECODE when a shader
//! samples an sRGB texture; a plain `Rgba8Unorm` target does neither. If the executor advertised/mapped
//! sRGB as if it were plain `Unorm`, colors would be stored too dark (128 instead of 188 for linear 0.5).
//!
//! TRANSFER FUNCTION (IEC 61966-2-1 sRGB OETF, the encode wgpu applies on write to an sRGB target):
//!     V = 12.92 · L                       for L ≤ 0.0031308
//!     V = 1.055 · L^(1/2.4) − 0.055       for L  > 0.0031308
//! For LINEAR L = 0.5:
//!     V = 1.055 · 0.5^(1/2.4) − 0.055 = 1.055 · 0.749154 − 0.055 = 0.735357
//!     round(0.735357 · 255) = round(187.5) = 188      ← the EXACT expected sRGB8 byte
//! A naive backend that treated the sRGB target as plain `Rgba8Unorm` would store round(0.5·255) = 128.
//! So: gamma-on-write is PROVEN iff an sRGB target stores ~188 while an identical draw into an `Rgba8Unorm`
//! target stores ~128. The inverse (decode-on-sample) is proven by sampling a stored-188 sRGB texel and
//! landing ~0.5 linear (128 in a linear target), whereas the same 188 in a plain `Unorm` texture samples
//! back as 188 (no decode).
//!
//! Both readbacks are RAW stored bytes (`copy_texture_to_buffer` copies the encoded texels verbatim — it
//! does NOT decode), so the asserted byte IS the on-device stored value.

mod common;
use common::*;

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 64;
const H: u32 = 64;

/// The exact sRGB8 encoding of linear 0.5 (see the module header): round(187.5) = 188.
const SRGB_OF_LINEAR_HALF: u8 = 188;
/// The value a naive "sRGB == Unorm" backend would store for linear 0.5: round(0.5·255) = 128.
const NAIVE_LINEAR_HALF: u8 = 128;

// A fullscreen triangle that writes a CONSTANT linear 0.5 gray. On an sRGB target the GPU gamma-encodes
// this on write (→188); on a plain Unorm target it is stored verbatim (→128).
const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;
const FS_CONST: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(0.5, 0.5, 0.5, 1.0); }
"#;

// Sample a texture at its center and write the (decoded, for sRGB) linear result straight into a LINEAR
// Rgba8Unorm target — so the stored byte is exactly the sampled linear value.
const FS_SAMPLE: &str = r#"#version 460
layout(set = 0, binding = 0) uniform texture2D t;
layout(set = 0, binding = 1) uniform sampler   s;
layout(location = 0) out vec4 o;
void main() { o = texture(sampler2D(t, s), vec2(0.5, 0.5)); }
"#;

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

/// Draw the constant-linear-0.5 fullscreen triangle into a fresh `fmt` target and return its raw readback.
fn draw_const(exec: &mut WgpuExecutor, fmt: TextureFormat) -> Vec<u8> {
    let mut s = new_session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    W,
                    H,
                    fmt,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS_CONST),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![ct(fmt)],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the constant-color sRGB/Unorm draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

/// Upload one raw texel `stored` into a 1×1 `src_fmt` texture, sample it (nearest, clamp) into a LINEAR
/// `Rgba8Unorm` target, and return the single readback pixel. If `src_fmt` is sRGB the GPU decodes on the
/// sample; if it is plain `Unorm` it does not.
fn sample_stored(exec: &mut WgpuExecutor, src_fmt: TextureFormat, stored: [u8; 4]) -> [u8; 4] {
    let mut s = new_session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            // 1×1 target (linear) + 1×1 source in `src_fmt`.
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateTexture(
                2,
                tex(
                    1,
                    1,
                    src_fmt,
                    texture_usage::SAMPLED | texture_usage::COPY_DST,
                ),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 4,
                    usage: buffer_usage::COPY_SRC,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: stored.to_vec(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS_SAMPLE),
            },
            Cmd::CreateSampler(
                1,
                SamplerDesc {
                    min_filter: Filter::Nearest,
                    mag_filter: Filter::Nearest,
                    mip_filter: Filter::Nearest,
                    address_u: AddressMode::ClampToEdge,
                    address_v: AddressMode::ClampToEdge,
                    address_w: AddressMode::ClampToEdge,
                },
            ),
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: "fmain".into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![ct(TextureFormat::Rgba8Unorm)],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
            Cmd::CreateBindGroup(
                1,
                BindGroupDesc {
                    set: 0,
                    entries: vec![
                        BindEntry {
                            binding: 0,
                            resource: BindResource::Texture { id: 2 },
                        },
                        BindEntry {
                            binding: 1,
                            resource: BindResource::Sampler { id: 1 },
                        },
                    ],
                },
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture {
                        src: 1,
                        src_offset: 0,
                        bytes_per_row: 4,
                        dst: 2,
                        mip: 0,
                        width: 1,
                        height: 1,
                    },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [0.0, 0.0, 0.0, 1.0],
                            store: true,
                        }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 },
                    Enc::Draw {
                        vertex_count: 3,
                        instance_count: 1,
                        first_vertex: 0,
                        first_instance: 0,
                    },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the sample-and-store draw must run cleanly");
    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn srgb_target_gamma_encodes_on_write_and_decodes_on_sample() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return, // no adapter — skip like the rest of the suite
    };

    // ================= CASE 1: gamma-on-WRITE (linear 0.5 → sRGB8 188, not 128) =================
    let srgb = draw_const(&mut exec, TextureFormat::Rgba8Srgb);
    let unorm = draw_const(&mut exec, TextureFormat::Rgba8Unorm);

    write_png("srgb_target", W, H, &srgb); // the correct, gamma-encoded (lighter) image
    write_png("srgb_target_linear_ref", W, H, &unorm); // the plain-Unorm reference (naive 128, darker)

    // Every sRGB texel is the gamma-ENCODED byte 188 (±1 unorm rounding), alpha untouched (encode is RGB-only).
    for (i, p) in srgb.chunks_exact(4).enumerate() {
        assert!(
            near_tol([p[0], p[1], p[2], p[3]], [SRGB_OF_LINEAR_HALF, SRGB_OF_LINEAR_HALF, SRGB_OF_LINEAR_HALF, 255], 2),
            "sRGB target px {i}: {:?} must be the gamma-encoded linear-0.5 value ~{SRGB_OF_LINEAR_HALF} (proves \
             linear→sRGB on write), not the naive {NAIVE_LINEAR_HALF}",
            &p[..4]
        );
    }
    // The SAME draw into a plain Unorm target stores the raw linear 0.5 → 128 (no encode).
    for (i, p) in unorm.chunks_exact(4).enumerate() {
        assert!(
            near_tol(
                [p[0], p[1], p[2], p[3]],
                [NAIVE_LINEAR_HALF, NAIVE_LINEAR_HALF, NAIVE_LINEAR_HALF, 255],
                2
            ),
            "Unorm target px {i}: {:?} must store linear 0.5 verbatim (~{NAIVE_LINEAR_HALF})",
            &p[..4]
        );
    }
    // The two MUST differ by a wide margin — this is the whole point: sRGB was NOT treated as plain Unorm.
    let (r_srgb, r_unorm) = (srgb[0] as i16, unorm[0] as i16);
    assert!(
        r_srgb - r_unorm >= 40,
        "sRGB store ({r_srgb}) must be far LIGHTER than the Unorm store ({r_unorm}) — a >=40 gap proves gamma \
         encoding actually happened; if they were equal the executor treated sRGB as plain Unorm"
    );

    // ================= CASE 2: sRGB→linear DECODE on sample (188 → ~0.5 linear = 128) =================
    // A stored-188 sRGB texel, sampled, lands ~0.503 linear → 128 in a linear target.
    let from_srgb = sample_stored(
        &mut exec,
        TextureFormat::Rgba8Srgb,
        [
            SRGB_OF_LINEAR_HALF,
            SRGB_OF_LINEAR_HALF,
            SRGB_OF_LINEAR_HALF,
            255,
        ],
    );
    // The SAME 188 in a plain Unorm texture is NOT decoded → samples back as 188.
    let from_unorm = sample_stored(
        &mut exec,
        TextureFormat::Rgba8Unorm,
        [
            SRGB_OF_LINEAR_HALF,
            SRGB_OF_LINEAR_HALF,
            SRGB_OF_LINEAR_HALF,
            255,
        ],
    );

    assert!(
        near_tol(from_srgb, [NAIVE_LINEAR_HALF, NAIVE_LINEAR_HALF, NAIVE_LINEAR_HALF, 255], 2),
        "sampling a stored-188 sRGB texel must DECODE to ~0.5 linear (~{NAIVE_LINEAR_HALF} in a linear target), \
         got {from_srgb:?}"
    );
    assert!(
        near_tol(from_unorm, [SRGB_OF_LINEAR_HALF, SRGB_OF_LINEAR_HALF, SRGB_OF_LINEAR_HALF, 255], 2),
        "sampling the same 188 from a plain Unorm texture must NOT decode (stays ~188), got {from_unorm:?}"
    );
    assert!(
        from_unorm[0] as i16 - from_srgb[0] as i16 >= 40,
        "the sRGB texel decodes ({}) but the identical Unorm texel does not ({}) — proving sRGB sampling is a \
         real linear decode, not a passthrough",
        from_srgb[0], from_unorm[0]
    );

    eprintln!(
        "demo `srgb_target`: WRITE linear 0.5 → sRGB8 {} (Unorm {}); SAMPLE stored-188 sRGB → linear {} \
         (Unorm passthrough {}). Transfer fn: IEC 61966-2-1 sRGB OETF. PNGs at {}/srgb_target.png \
         (lighter, correct) vs {}/srgb_target_linear_ref.png (naive 128, darker).",
        srgb[0], unorm[0], from_srgb[0], from_unorm[0], OUT_DIR, OUT_DIR
    );
}
