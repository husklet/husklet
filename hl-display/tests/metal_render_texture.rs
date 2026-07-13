#![cfg(target_os = "macos")]

use hl_display::metal::MetalCtx;
use hl_display::metal_backend::MetalBackend;
use hl_gpu::backend::GpuBackend;
use hl_gpu::id::{BindGroupId, BufferId, PipelineId, SamplerId, ShaderId, TextureId};
use hl_gpu::ir::{
    self, texture_usage, AddressMode, BindEntry, BindGroupDesc, BindResource, BufferDesc,
    ColorAttachment, ColorTargetState, CommandBuffer, Enc, Filter, LoadOp, RenderPipelineDesc,
    SamplerDesc, ShaderRef, TextureDesc, TextureDim, TextureFormat, Topology, VertexAttr,
    VertexLayout,
};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{
    MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};

const W: u32 = 8;
const H: u32 = 8;

const QUAD_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct VIn {
    float2 p [[attribute(0)]];
    float2 uv [[attribute(1)]];
};

struct VOut {
    float4 position [[position]];
    float2 uv [[user(v0)]];
};

vertex VOut vmain(VIn in [[stage_in]]) {
    VOut out;
    out.position = float4(in.p, 0.0, 1.0);
    out.uv = in.uv;
    return out;
}

fragment float4 paint_quadrants(VOut in [[stage_in]]) {
    if (in.uv.y < 0.5) {
        return in.uv.x < 0.5 ? float4(1.0, 0.0, 0.0, 1.0) : float4(0.0, 1.0, 0.0, 1.0);
    }
    return in.uv.x < 0.5 ? float4(0.0, 0.0, 1.0, 1.0) : float4(1.0, 1.0, 1.0, 1.0);
}

fragment float4 solid_red(VOut in [[stage_in]]) {
    return float4(1.0, 0.0, 0.0, 1.0);
}

fragment float4 sample_tex(
    VOut in [[stage_in]],
    texture2d<float> src [[texture(0)]],
    sampler src_sampler [[sampler(0)]]
) {
    return src.sample(src_sampler, in.uv);
}
"#;

const CHROME_SOLID_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Uniforms {
    float4 sk_RTAdjust;
};

struct VIn {
    float2 position [[attribute(0)]];
    float4 color [[attribute(1)]];
};

struct VOut {
    float4 position [[position]];
    float4 color [[user(v0)]];
};

vertex VOut vmain(VIn in [[stage_in]], constant Uniforms& u [[buffer(1)]]) {
    VOut out;
    out.position = float4(in.position, 0.0, 1.0);
    out.color = in.color;
    out.position = float4(
        out.position.xy * u.sk_RTAdjust.xz + out.position.ww * u.sk_RTAdjust.yw,
        0.0,
        out.position.w
    );
    return out;
}

fragment float4 fmain(VOut in [[stage_in]]) {
    return in.color;
}
"#;

fn pack_msl(src: &str) -> Vec<u32> {
    let mut words = vec![src.len() as u32];
    for chunk in src.as_bytes().chunks(4) {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        words.push(u32::from_le_bytes(word));
    }
    words
}

fn quad_vertices() -> Vec<u8> {
    let verts: [[f32; 4]; 6] = [
        [-1.0, 1.0, 0.0, 0.0],
        [-1.0, -1.0, 0.0, 1.0],
        [1.0, 1.0, 1.0, 0.0],
        [1.0, 1.0, 1.0, 0.0],
        [-1.0, -1.0, 0.0, 1.0],
        [1.0, -1.0, 1.0, 1.0],
    ];
    let mut out = Vec::with_capacity(verts.len() * 4 * std::mem::size_of::<f32>());
    for v in verts {
        for f in v {
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
    out
}

fn pipeline(fragment_entry: &str) -> RenderPipelineDesc {
    RenderPipelineDesc {
        vertex: ShaderRef {
            module: 20,
            entry: "vmain".into(),
        },
        fragment: Some(ShaderRef {
            module: 20,
            entry: fragment_entry.into(),
        }),
        vertex_buffers: vec![VertexLayout {
            stride: 16,
            step_mode: 0,
            attrs: vec![
                VertexAttr {
                    location: 0,
                    format: 2,
                    offset: 0,
                },
                VertexAttr {
                    location: 1,
                    format: 2,
                    offset: 8,
                },
            ],
        }],
        color_targets: vec![ColorTargetState {
            format: TextureFormat::Bgra8Unorm,
            blend: None,
            write_mask: 0xf,
        }],
        depth: None,
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        label: String::new(),
    }
}

fn create_common_pipeline_state(be: &mut MetalBackend) {
    let vertices = quad_vertices();
    be.create_buffer(
        BufferId(10),
        &BufferDesc {
            size: vertices.len() as u64,
            usage: ir::buffer_usage::VERTEX,
            label: "quad".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(10), 0, &vertices).unwrap();
    be.create_shader(ShaderId(20), ir::ShaderPayloadKind::LegacyMsl, &pack_msl(QUAD_MSL)).unwrap();
    be.create_render_pipeline(PipelineId(30), &pipeline("paint_quadrants"))
        .unwrap();
    be.create_render_pipeline(PipelineId(31), &pipeline("sample_tex"))
        .unwrap();
    be.create_render_pipeline(PipelineId(32), &pipeline("solid_red"))
        .unwrap();
    be.create_sampler(
        SamplerId(3),
        &SamplerDesc {
            min_filter: Filter::Nearest,
            mag_filter: Filter::Nearest,
            mip_filter: Filter::Nearest,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            address_w: AddressMode::ClampToEdge,
        },
    )
    .unwrap();
}

fn create_sample_bind_group(be: &mut MetalBackend, texture_id: u32) {
    be.create_bind_group(
        BindGroupId(40),
        &BindGroupDesc {
            set: 0,
            entries: vec![
                BindEntry {
                    binding: 0,
                    resource: BindResource::Texture { id: texture_id },
                },
                BindEntry {
                    binding: 0,
                    resource: BindResource::Sampler { id: 3 },
                },
            ],
        },
    )
    .unwrap();
}

fn read_bgra(
    ctx: &MetalCtx,
    tex: &objc2::runtime::ProtocolObject<dyn objc2_metal::MTLTexture>,
) -> Vec<u8> {
    ctx.readback_bgra(tex, W, H)
}

fn new_rgba_texture(ctx: &MetalCtx, w: u32, h: u32) -> Retained<ProtocolObject<dyn MTLTexture>> {
    let desc = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::RGBA8Unorm,
            w as usize,
            h as usize,
            false,
        )
    };
    desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
    desc.setStorageMode(MTLStorageMode::Shared);
    ctx.device
        .newTextureWithDescriptor(&desc)
        .expect("newTextureWithDescriptor RGBA failed")
}

fn read_rgba(tex: &ProtocolObject<dyn MTLTexture>, w: u32, h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let region = objc2_metal::MTLRegion {
        origin: objc2_metal::MTLOrigin { x: 0, y: 0, z: 0 },
        size: objc2_metal::MTLSize {
            width: w as usize,
            height: h as usize,
            depth: 1,
        },
    };
    unsafe {
        tex.getBytes_bytesPerRow_fromRegion_mipmapLevel(
            std::ptr::NonNull::new(out.as_mut_ptr() as *mut _).unwrap(),
            (w * 4) as usize,
            region,
            0,
        );
    }
    out
}

fn chrome_solid_vertices() -> Vec<u8> {
    let color = [233u8, 238, 247, 255];
    let verts: [([f32; 2], [u8; 4]); 4] = [
        ([0.0, 0.0], color),
        ([0.0, 256.0], color),
        ([480.0, 0.0], color),
        ([480.0, 256.0], color),
    ];
    let mut out = Vec::with_capacity(verts.len() * 12);
    for (pos, color) in verts {
        out.extend_from_slice(&pos[0].to_le_bytes());
        out.extend_from_slice(&pos[1].to_le_bytes());
        out.extend_from_slice(&color);
    }
    out
}

fn chrome_rt_adjust() -> Vec<u8> {
    let values = [1.0f32 / 256.0, -1.0, 1.0 / 128.0, -1.0];
    let mut out = Vec::with_capacity(16);
    for value in values {
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

fn px(bgra: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [bgra[i], bgra[i + 1], bgra[i + 2], bgra[i + 3]]
}

fn assert_px_near(bgra: &[u8], x: u32, y: u32, want_bgra: [u8; 4]) {
    let got = px(bgra, x, y);
    for (g, w) in got.into_iter().zip(want_bgra) {
        assert!(
            (g as i16 - w as i16).abs() <= 3,
            "pixel ({x},{y}) got BGRA={got:?}, want approximately {want_bgra:?}"
        );
    }
}

fn assert_rgba_near(rgba: &[u8], w: u32, x: u32, y: u32, want: [u8; 4]) {
    let i = ((y * w + x) * 4) as usize;
    let got = [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]];
    for (g, w) in got.into_iter().zip(want) {
        assert!(
            (g as i16 - w as i16).abs() <= 3,
            "pixel ({x},{y}) got RGBA={got:?}, want approximately {want:?}"
        );
    }
}

#[test]
fn render_to_offscreen_texture_then_sample_into_second_target() {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("skipping Metal render-to-texture test: no Metal device");
        return;
    };
    let src = ctx.new_bgra_texture(W, H);
    let dst = ctx.new_bgra_texture(W, H);
    let mut be = MetalBackend::new(&ctx);
    be.set_render_target(1, src.clone());
    be.set_render_target(2, dst.clone());
    create_common_pipeline_state(&mut be);
    create_sample_bind_group(&mut be, 1);

    be.submit(&CommandBuffer {
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
            Enc::SetPipeline(30),
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 10,
                offset: 0,
            },
            Enc::Draw {
                vertex_count: 6,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 2,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(31),
            Enc::SetBindGroup {
                index: 0,
                group: 40,
            },
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 10,
                offset: 0,
            },
            Enc::Draw {
                vertex_count: 6,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    })
    .unwrap();

    let out = read_bgra(&ctx, &dst);
    assert_px_near(&out, 1, 1, [0, 0, 255, 255]);
    assert_px_near(&out, 6, 1, [0, 255, 0, 255]);
    assert_px_near(&out, 1, 6, [255, 0, 0, 255]);
    assert_px_near(&out, 6, 6, [255, 255, 255, 255]);
}

#[test]
fn load_pass_preserves_prior_offscreen_contents_before_blit_readback() {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("skipping Metal load/store test: no Metal device");
        return;
    };
    let tex = ctx.new_bgra_texture(W, H);
    let copy = ctx.new_bgra_texture(W, H);
    let mut be = MetalBackend::new(&ctx);
    be.set_render_target(1, tex.clone());
    create_common_pipeline_state(&mut be);

    be.submit(&CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 1.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 1,
                    load: LoadOp::Load,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(32),
            Enc::SetScissor {
                x: 2,
                y: 2,
                w: 4,
                h: 4,
            },
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 10,
                offset: 0,
            },
            Enc::Draw {
                vertex_count: 6,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    })
    .unwrap();

    ctx.blit(&tex, &copy);
    let out = read_bgra(&ctx, &copy);
    assert_px_near(&out, 0, 0, [255, 0, 0, 255]);
    assert_px_near(&out, 7, 7, [255, 0, 0, 255]);
    assert_px_near(&out, 3, 3, [0, 0, 255, 255]);
    assert_px_near(&out, 4, 4, [0, 0, 255, 255]);
}

#[test]
fn chrome_like_rgba_triangle_strip_rtadjust_fills_offscreen_quad() {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("skipping Chrome-like offscreen test: no Metal device");
        return;
    };
    let width = 512;
    let height = 256;
    let target = new_rgba_texture(&ctx, width, height);
    let vertices = chrome_solid_vertices();
    let uniforms = chrome_rt_adjust();
    let mut be = MetalBackend::new(&ctx);
    be.set_render_target(13, target.clone());

    be.create_buffer(
        BufferId(10),
        &BufferDesc {
            size: vertices.len() as u64,
            usage: ir::buffer_usage::VERTEX,
            label: "chrome-solid-vbo".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(10), 0, &vertices).unwrap();
    be.create_buffer(
        BufferId(11),
        &BufferDesc {
            size: uniforms.len() as u64,
            usage: ir::buffer_usage::UNIFORM,
            label: "chrome-rtadjust".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(11), 0, &uniforms).unwrap();
    be.create_shader(ShaderId(20), ir::ShaderPayloadKind::LegacyMsl, &pack_msl(CHROME_SOLID_MSL))
        .unwrap();
    be.create_bind_group(
        BindGroupId(40),
        &BindGroupDesc {
            set: 0,
            entries: vec![BindEntry {
                binding: 1,
                resource: BindResource::Buffer {
                    id: 11,
                    offset: 0,
                    size: uniforms.len() as u64,
                },
            }],
        },
    )
    .unwrap();
    be.create_render_pipeline(
        PipelineId(30),
        &RenderPipelineDesc {
            vertex: ShaderRef {
                module: 20,
                entry: "vmain".into(),
            },
            fragment: Some(ShaderRef {
                module: 20,
                entry: "fmain".into(),
            }),
            vertex_buffers: vec![VertexLayout {
                stride: 12,
                step_mode: 0,
                attrs: vec![
                    VertexAttr {
                        location: 0,
                        format: 2,
                        offset: 0,
                    },
                    VertexAttr {
                        location: 1,
                        format: 0x10104,
                        offset: 8,
                    },
                ],
            }],
            color_targets: vec![ColorTargetState {
                format: TextureFormat::Rgba8Unorm,
                blend: None,
                write_mask: 0xf,
            }],
            depth: None,
            topology: Topology::TriangleStrip,
            cull: 0,
            front_face: 0,
            label: "chrome-solid-rtadjust".into(),
        },
    )
    .unwrap();

    be.submit(&CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: 13,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(30),
            Enc::SetBindGroup {
                index: 0,
                group: 40,
            },
            Enc::SetViewport {
                x: 0.0,
                y: 0.0,
                w: width as f32,
                h: height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            },
            Enc::SetScissor {
                x: 0,
                y: 0,
                w: width,
                h: height,
            },
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 10,
                offset: 0,
            },
            Enc::Draw {
                vertex_count: 4,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    })
    .unwrap();

    let rgba = read_rgba(&target, width, height);
    let want = [233, 238, 247, 255];
    assert_rgba_near(&rgba, width, 4, 4, want);
    assert_rgba_near(&rgba, width, 240, 128, want);
    assert_rgba_near(&rgba, width, 479, 255, want);
    assert_rgba_near(&rgba, width, 500, 240, [0, 0, 0, 255]);
}

// A quad covering the TOP half of clip space (NDC y in [0, +1]) with solid_red. Rendering it into a
// framebuffer reveals that framebuffer's Y convention: Metal (top-left) puts red in the top rows; GL
// (bottom-left) puts red in the bottom rows.
fn top_half_quad_vertices() -> Vec<u8> {
    let verts: [[f32; 4]; 6] = [
        [-1.0, 1.0, 0.0, 0.0],
        [-1.0, 0.0, 0.0, 1.0],
        [1.0, 1.0, 1.0, 0.0],
        [1.0, 1.0, 1.0, 0.0],
        [-1.0, 0.0, 0.0, 1.0],
        [1.0, 0.0, 1.0, 1.0],
    ];
    let mut out = Vec::with_capacity(verts.len() * 16);
    for v in verts {
        for f in v {
            out.extend_from_slice(&f.to_le_bytes());
        }
    }
    out
}

// Regression for the web-content upside-down bug: Chrome GPU-rasterizes page body text into offscreen
// content tiles (e.g. textures 512/514, created by the guest via CreateTexture and used as render-pass
// targets) and then composites them with GL-convention texcoords. GL's framebuffer origin is bottom-left
// but Metal's is top-left, so without compensation the tile is stored Y-mirrored and the page renders
// upside-down while the directly-drawn UI stays upright. The backend Y-flips passes that target an
// OFFSCREEN texture (not the presented surface registered via set_render_target) so the tile is stored
// bottom-left like GL. This test draws the SAME top-half quad into (a) the presented surface and (b) an
// offscreen tile, and asserts the tile is the vertical MIRROR of the surface — i.e. the offscreen flip is
// active for the tile and NOT for the surface.
#[test]
fn offscreen_render_target_is_stored_bottom_left_like_gl() {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("skipping offscreen Y-flip test: no Metal device");
        return;
    };
    let surface = ctx.new_bgra_texture(W, H);
    let mut be = MetalBackend::new(&ctx);
    // Presented surface: registered via set_render_target → NOT flipped (scanned out top-left).
    be.set_render_target(1, surface.clone());
    // Content tile: created by the guest as a sampled render target → an OFFSCREEN target → flipped.
    be.create_texture(
        TextureId(50),
        &TextureDesc {
            width: W,
            height: H,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Bgra8Unorm,
            usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET | texture_usage::COPY_DST,
            label: "content-tile".into(),
        },
    )
    .unwrap();

    let vertices = top_half_quad_vertices();
    be.create_buffer(
        BufferId(10),
        &BufferDesc {
            size: vertices.len() as u64,
            usage: ir::buffer_usage::VERTEX,
            label: "top-half-quad".into(),
        },
    )
    .unwrap();
    be.write_buffer(BufferId(10), 0, &vertices).unwrap();
    be.create_shader(ShaderId(20), ir::ShaderPayloadKind::LegacyMsl, &pack_msl(QUAD_MSL)).unwrap();
    // solid_red, Bgra8Unorm color target (matches both surface and tile).
    be.create_render_pipeline(PipelineId(32), &pipeline("solid_red"))
        .unwrap();

    let draw_pass = |target: u32| CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: target,
                    load: LoadOp::Clear,
                    clear: [0.0, 0.0, 0.0, 1.0],
                    store: true,
                }],
                depth: None,
            },
            Enc::SetPipeline(32),
            Enc::SetViewport {
                x: 0.0,
                y: 0.0,
                w: W as f32,
                h: H as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            },
            Enc::SetVertexBuffer {
                slot: 0,
                buffer: 10,
                offset: 0,
            },
            Enc::Draw {
                vertex_count: 6,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    };

    be.submit(&draw_pass(1)).unwrap();
    let surf = read_bgra(&ctx, &surface);
    be.submit(&draw_pass(50)).unwrap();
    let tile = read_bgra(&ctx, be.texture(50).expect("content tile materialized"));

    let red = [0u8, 0, 255, 255]; // solid_red = RGBA(1,0,0,1) → BGRA
    let black = [0u8, 0, 0, 255];
    // Presented surface keeps Metal's top-left convention: top half red, bottom half black.
    assert_px_near(&surf, 4, 1, red);
    assert_px_near(&surf, 4, 6, black);
    // Offscreen tile is flipped to GL's bottom-left convention: the vertical mirror.
    assert_px_near(&tile, 4, 1, black);
    assert_px_near(&tile, 4, 6, red);
}

// Regression for "Metal Render Target Texture Id Can Alias Guest Texture Id 1" (gpu-display-sentry.md):
// the executor reserves texture id 1 for the presented surface (set_render_target). A guest that also
// creates texture id 1 must get its OWN distinct texture, never an alias of the present RT — otherwise its
// geometry silently corrupts the presented frame. We clear id 1 (present RT) green, then after the guest
// creates texture id 1, clear id 1 red: the present surface must stay green (untouched) while the guest's
// distinct texture goes red. Under the old aliasing (create_texture(1) was a no-op) the red clear landed
// back on the present surface and this test would see red there.
#[test]
fn guest_texture_id_1_does_not_alias_present_render_target() {
    let Some(ctx) = MetalCtx::new() else {
        eprintln!("skipping present-alias test: no Metal device");
        return;
    };
    let present = ctx.new_bgra_texture(W, H);
    let mut be = MetalBackend::new(&ctx);
    be.set_render_target(1, present.clone());

    let clear_pass = |target: u32, color: [f32; 4]| CommandBuffer {
        encoder: vec![
            Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: target,
                    load: LoadOp::Clear,
                    clear: color,
                    store: true,
                }],
                depth: None,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    };

    // id 1 → present RT (no guest texture yet): clear it green and confirm it landed on the surface.
    be.submit(&clear_pass(1, [0.0, 1.0, 0.0, 1.0])).unwrap();
    assert_px_near(&read_bgra(&ctx, &present), 4, 4, [0, 255, 0, 255]);

    // The guest creates its own texture at the reserved id 1 (a protocol violation we must not honor by
    // aliasing). This must materialize a DISTINCT texture, not silently no-op onto the present RT.
    be.create_texture(
        TextureId(1),
        &TextureDesc {
            width: W,
            height: H,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Bgra8Unorm,
            usage: texture_usage::SAMPLED | texture_usage::RENDER_TARGET,
            label: "guest-tex-1".into(),
        },
    )
    .unwrap();

    // id 1 now resolves to the guest's own texture: clear it red.
    be.submit(&clear_pass(1, [1.0, 0.0, 0.0, 1.0])).unwrap();
    // The presented surface must be UNTOUCHED — still green. This is the anti-aliasing guarantee.
    assert_px_near(&read_bgra(&ctx, &present), 4, 4, [0, 255, 0, 255]);
    // The guest's distinct texture received the red clear.
    assert_px_near(
        &read_bgra(&ctx, be.texture(1).expect("guest texture 1 materialized")),
        4,
        4,
        [0, 0, 255, 255],
    );
}
