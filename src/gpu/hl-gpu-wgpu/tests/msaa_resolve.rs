//! Exact-pixel MSAA-resolve demo — the end-to-end proof that a 4× multisampled render target really
//! ANTIALIASES a triangle edge and that `Enc::ResolveTexture` averages its samples into a single-sample
//! texture.
//!
//! ONE triangle, TWO renders of identical geometry:
//!
//!   * `sample_count = 4` (MSAA): draw the triangle into a 4× multisampled color target, then
//!     `ResolveTexture` it into a single-sample texture and read that back. Along the diagonal edge the
//!     resolved pixels carry INTERMEDIATE coverage (a partially-covered pixel averages fg+bg to a gray the
//!     hard rasterizer never produces), while the interior is exact fg and the exterior exact bg.
//!   * `sample_count = 1` (the aliased control): draw the SAME triangle into a plain single-sample target
//!     and read it back. Every pixel is EXACTLY fg or bg — a hard, stair-stepped edge with ZERO
//!     intermediate pixels.
//!
//! The asserts pin this exactly: the 1× render has *zero* intermediate pixels; the MSAA render has *many*,
//! ALL lying on the diagonal edge; and there is at least one pixel the MSAA smooths (a gray) that the 1×
//! render leaves pure — the smooth gradient the aliased render lacks. Both frames are written to
//! `/tmp/hl-demo/` (`msaa_resolve.png` = smooth, `msaa_resolve_noaa.png` = hard) for a visual confrontation.
//!
//! REGRESSION PROOF: `Enc::ResolveTexture` returned `GpuError::Unsupported("wgpu: ResolveTexture
//! (multisample) unimplemented")` and was UN-advertised before this change — the resolve below could not
//! run at all. The demo asserts the op is now advertised, that the resolve runs (a rejected op would make
//! `runtime::submit` return `Err` and fail the `.expect`), and — as a live negative control — that a
//! resolve of a NON-multisampled source is still a clean typed error (the op is validated, not a blind pass).
//!
//! If NO adapter is reachable (no lavapipe/Vulkan ICD) the test skips, mirroring the rest of the suite.

mod common;

use common::{new_session, write_png, OUT_DIR};

use hl_gpu::protocol::model::command::etag;
use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, ColorTargetState, Extent3d, Origin3d, RenderPipelineDesc, ShaderRef,
    TextureDesc, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureDim, TextureFormat, Topology};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 48;
const H: u32 = 48;

// fg = opaque white, bg = opaque black — the widest possible fg/bg contrast, so a partially-covered edge
// pixel resolves to an unmistakable mid-gray (neither ~255 nor ~0).
const FG: [u8; 4] = [255, 255, 255, 255];
const BG: [u8; 4] = [0, 0, 0, 255];

// A right triangle whose vertices land on framebuffer corners: bottom-left, bottom-right, top-left. Its
// hypotenuse runs from the framebuffer's top-left (0,0) to its bottom-right (W,H) — the MAIN diagonal
// `fy == fx` — so the covered side is `fy > fx` (the lower-left half) and the aliasing lives on that
// diagonal.
const TRI_VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

const WHITE_FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(1.0, 1.0, 1.0, 1.0); }
"#;

fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
    GlslDescriptor {
        stage,
        entry: entry.to_string(),
        source: source.to_string(),
    }
    .to_words()
}

fn tex(w: u32, h: u32, sample_count: u32, usage: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage,
        label: String::new(),
    }
}

fn pipeline(sample_count: u32) -> RenderPipelineDesc {
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
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Rgba8Unorm,
            blend: None,
            write_mask: 0xF,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count,
        label: String::new(),
    }
}

/// Draw the triangle into a `sample_count`× color target; when `sample_count > 1`, resolve into a
/// single-sample texture. Returns the single-sample tight RGBA readback plane (`W*H*4`).
fn render(exec: &mut WgpuExecutor, sample_count: u32) -> Vec<u8> {
    let mut s = new_session(exec);
    let shaders = vec![
        Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::VERTEX, "vmain", TRI_VS),
        },
        Cmd::CreateShader {
            id: 2,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, "fmain", WHITE_FS),
        },
        Cmd::CreateRenderPipeline(1, pipeline(sample_count)),
    ];

    // The color target the pipeline draws into MUST share its sample count.
    let read_id = if sample_count > 1 {
        // 1 = the 4× MSAA color target (RENDER_TARGET only — a multisampled texture is never copied,
        // only resolved); 2 = the single-sample resolve destination we read back.
        let mut cmds = vec![
            Cmd::CreateTexture(1, tex(W, H, sample_count, texture_usage::RENDER_TARGET)),
            Cmd::CreateTexture(
                2,
                tex(
                    W,
                    H,
                    1,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
        ];
        cmds.extend(shaders);
        cmds.push(Cmd::Submit(CommandBuffer {
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
                // Average the 4 samples of each texel into the single-sample destination.
                Enc::ResolveTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: W,
                        height: H,
                        depth: 1,
                    },
                },
            ],
            signal: None,
        }));
        hl_gpu::runtime::submit(&mut s, exec, 0, &cmds)
            .expect("the MSAA draw + ResolveTexture must run (a rejected resolve would Err here)");
        2
    } else {
        // A plain single-sample color target drawn + read back directly — the aliased control.
        let mut cmds = vec![Cmd::CreateTexture(
            1,
            tex(
                W,
                H,
                1,
                texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
            ),
        )];
        cmds.extend(shaders);
        cmds.push(Cmd::Submit(CommandBuffer {
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
        }));
        hl_gpu::runtime::submit(&mut s, exec, 0, &cmds).expect("the single-sample draw must run");
        1
    };
    exec.read_texture(&s.resources, read_id).unwrap()
}

fn at(plane: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [plane[i], plane[i + 1], plane[i + 2], plane[i + 3]]
}

/// A pixel is fg/bg only if EVERY rgb channel is at the extreme (within 1 ULP); anything else — a channel
/// strictly between — is an antialiased "intermediate" (partial-coverage) pixel.
fn is_fg(p: [u8; 4]) -> bool {
    (0..3).all(|k| p[k] >= 254)
}
fn is_bg(p: [u8; 4]) -> bool {
    (0..3).all(|k| p[k] <= 1)
}
fn is_intermediate(p: [u8; 4]) -> bool {
    !is_fg(p) && !is_bg(p)
}

/// Count intermediate pixels and the max distance of any intermediate pixel from the diagonal edge
/// (`fy == fx`). A correct antialiased render puts EVERY intermediate pixel right on the edge.
fn edge_stats(plane: &[u8]) -> (u32, i64) {
    let mut count = 0u32;
    let mut max_off_diag = 0i64;
    for y in 0..H {
        for x in 0..W {
            if is_intermediate(at(plane, x, y)) {
                count += 1;
                max_off_diag = max_off_diag.max((x as i64 - y as i64).abs());
            }
        }
    }
    (count, max_off_diag)
}

#[test]
fn msaa_resolve_antialiases_the_edge() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        // No adapter (no lavapipe/Vulkan ICD reachable) — skip, mirroring the rest of the suite.
        Err(_) => return,
    };

    // FAIL-BEFORE proof: the op is advertised now (it was NOT before this change — the frozen coverage test
    // `blit_and_resolve_are_advertised_and_run` used to assert `!supports_command(RESOLVE_TEXTURE)`).
    assert!(
        exec.capabilities().supports_command(etag::RESOLVE_TEXTURE),
        "ResolveTexture must be advertised now (it was un-advertised + rejected before this change)"
    );

    // Deep-interior (covered: fy >> fx) and deep-exterior (uncovered: fx >> fy) sample points, well clear of
    // the diagonal so BOTH renders must agree on them exactly.
    let interior = (3u32, H - 4); // bottom-left region → inside the triangle
    let exterior = (W - 4, 3u32); // top-right region → outside the triangle

    // ---- MSAA (4×) render + resolve ----
    let msaa = render(&mut exec, 4);
    write_png("msaa_resolve", W, H, &msaa);
    let (msaa_edge, msaa_off_diag) = edge_stats(&msaa);

    assert_eq!(
        at(&msaa, interior.0, interior.1),
        FG,
        "MSAA interior must be exact fg (white)"
    );
    assert_eq!(
        at(&msaa, exterior.0, exterior.1),
        BG,
        "MSAA exterior must be exact bg (black)"
    );
    assert!(
        msaa_edge >= W / 2,
        "MSAA must antialias the diagonal — expected many intermediate (partial-coverage) pixels, got {msaa_edge}"
    );
    assert!(
        msaa_off_diag <= 2,
        "every MSAA intermediate pixel must lie ON the diagonal edge (|x-y| <= 2), max off-diagonal was {msaa_off_diag}"
    );

    // ---- single-sample (1×) render — the aliased control ----
    let noaa = render(&mut exec, 1);
    write_png("msaa_resolve_noaa", W, H, &noaa);
    let (noaa_edge, _) = edge_stats(&noaa);

    assert_eq!(
        at(&noaa, interior.0, interior.1),
        FG,
        "1× interior must be exact fg (white)"
    );
    assert_eq!(
        at(&noaa, exterior.0, exterior.1),
        BG,
        "1× exterior must be exact bg (black)"
    );
    assert_eq!(
        noaa_edge, 0,
        "the 1× render is HARD-aliased: EVERY pixel must be pure fg or pure bg, but {noaa_edge} were intermediate"
    );

    // The decisive contrast: at least one pixel the MSAA render smooths to a gray, the 1× render leaves
    // pure — the antialiased gradient the aliased render fundamentally lacks.
    let smoothed = (0..H * W).any(|i| {
        let (x, y) = (i % W, i / W);
        is_intermediate(at(&msaa, x, y)) && !is_intermediate(at(&noaa, x, y))
    });
    assert!(
        smoothed,
        "there must be an edge pixel MSAA resolves to an intermediate coverage value that the 1× render leaves pure"
    );

    // Negative control: the resolve op is genuinely validated — resolving a SINGLE-sampled source errors.
    let mut s = new_session(&exec);
    let bad = hl_gpu::runtime::submit(
        &mut s,
        &mut exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    W,
                    H,
                    1,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateTexture(
                2,
                tex(
                    W,
                    H,
                    1,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::Submit(CommandBuffer {
                encoder: vec![Enc::ResolveTexture {
                    src: 1,
                    src_sub: TextureSubresource::base(),
                    src_origin: Origin3d::default(),
                    dst: 2,
                    dst_sub: TextureSubresource::base(),
                    dst_origin: Origin3d::default(),
                    extent: Extent3d {
                        width: W,
                        height: H,
                        depth: 1,
                    },
                }],
                signal: None,
            }),
        ],
    );
    assert!(
        bad.is_err(),
        "resolving a non-multisampled source must be a clean typed error, not a silent pass"
    );

    eprintln!(
        "demo `msaa_resolve`: MSAA edge pixels={msaa_edge} (all on-diagonal, off<={msaa_off_diag}) vs 1× edge pixels=0 \
         — PNGs at {OUT_DIR}/msaa_resolve.png (smooth) + {OUT_DIR}/msaa_resolve_noaa.png (hard)"
    );
}
