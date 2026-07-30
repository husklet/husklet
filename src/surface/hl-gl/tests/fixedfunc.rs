//! Lowering tests for the fixed-function state + data uniforms this pass made real: `glBlendFunc` /
//! `glDepthFunc` / `glCullFace` / `glFrontFace` (+ their enables) reflected in the lowered pipeline
//! descriptor, and `glUniform*` values recorded into the program's uniform block and shipped at draw.
//!
//! Driven through the `record` + `swap` services against a `hl_gpu::RecordingSink` (no socket, no GPU:
//! just the emitted `Cmd`/`Enc` stream), complementing `tests/lowering.rs` + `tests/render.rs`.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{es3, record, swap};

use hl_gpu::protocol::model::descriptor::{RenderPipelineDesc, SamplerDesc};
use hl_gpu::protocol::model::enums::{buffer_usage, AddressMode, Filter};
use hl_gpu::{Cmd, RecordingSink};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str =
    "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";
// A program with one data uniform (a vec4 color) — exercises the uniform-block write path.
const FS_U: &str =
    "precision mediump float;\nuniform vec4 uColor;\nvoid main(){ gl_FragColor = uColor; }\n";

fn ctx_64() -> GlContext {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: 64,
        height: 64,
    });
    c
}

fn flat_program(c: &mut GlContext, fs: &str) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fsh = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fsh, fs);
    record::compile_shader(c, fsh);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fsh);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    prog
}

fn tri_vbo(c: &mut GlContext) {
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(c, GL_ARRAY_BUFFER, &[0u8; 24], 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);
}

fn pipeline_desc(batch: &[Cmd]) -> &RenderPipelineDesc {
    batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(_, d) => Some(d),
            _ => None,
        })
        .expect("a CreateRenderPipeline")
}

// ---------------------------------------------------------------------------------------------------
// blend / depth / cull → the lowered pipeline descriptor
// ---------------------------------------------------------------------------------------------------

#[test]
fn blend_func_lowers_to_pipeline_blend_state() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    record::enable(&mut c, GL_BLEND);
    record::blend_func(&mut c, GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let d = pipeline_desc(&sink.batches[0]);
    let blend = d.color_targets[0]
        .blend
        .as_ref()
        .expect("blend enabled → Some(BlendState)");
    // GL_SRC_ALPHA -> wire 4, GL_ONE_MINUS_SRC_ALPHA -> wire 5, GL_FUNC_ADD -> op 0.
    assert_eq!(
        (blend.src_color, blend.dst_color, blend.op_color),
        (4, 5, 0)
    );
    assert_eq!(
        (blend.src_alpha, blend.dst_alpha, blend.op_alpha),
        (4, 5, 0)
    );
}

#[test]
fn blend_disabled_lowers_to_no_blend() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(pipeline_desc(&sink.batches[0]).color_targets[0]
        .blend
        .is_none());
}

#[test]
fn depth_test_lowers_to_pipeline_depth_state() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_func(&mut c, GL_LEQUAL);
    record::depth_mask(&mut c, false);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let depth = pipeline_desc(&sink.batches[0])
        .depth
        .as_ref()
        .expect("depth-test → Some(DepthState)");
    // The neutral protocol compare code the wgpu executor decodes (`hl_gpu` `enums::compare`, Vulkan
    // VkCompareOp ordering): LESS_EQUAL = 3. (Previously this asserted 4, the WebGPU 1-based numbering,
    // which the executor decoded as GREATER — silently mis-testing every depth-tested draw.)
    assert_eq!(
        depth.depth_compare, 3,
        "GL_LEQUAL -> neutral compare::LESS_EQUAL (3)"
    );
    assert!(!depth.depth_write, "glDepthMask(false) disables writes");
}

#[test]
fn cull_face_lowers_to_pipeline_cull_and_winding() {
    let mut c = ctx_64();
    // Exercise the direct fixed-function mapping. A presented window target deliberately reflects clip Y
    // and reverses this winding once more to preserve the same visible GL face.
    c.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    record::enable(&mut c, GL_CULL_FACE);
    record::cull_face(&mut c, GL_FRONT);
    record::front_face(&mut c, GL_CW);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let d = pipeline_desc(&sink.batches[0]);
    assert_eq!(d.cull, 1, "GL_FRONT -> cull front (1)");
    assert_eq!(d.front_face, 1, "GL_CW -> front_face 1");
}

#[test]
fn no_cull_by_default() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(
        pipeline_desc(&sink.batches[0]).cull,
        0,
        "cull disabled by default"
    );
}

// ---------------------------------------------------------------------------------------------------
// glColorMask -> the lowered color-target write mask (was a faked no-op; the mask is now honored)
// ---------------------------------------------------------------------------------------------------

#[test]
fn color_mask_lowers_to_pipeline_write_mask() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    // Disable green + alpha writes: R=1, G=0, B=1, A=0 -> packed R<<0|B<<2 = 0b0101 = 0x5.
    record::color_mask(&mut c, true, false, true, false);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(
        pipeline_desc(&sink.batches[0]).color_targets[0].write_mask,
        0x5,
        "glColorMask(1,0,1,0) lowers to write_mask R|B (0x5), not the hardcoded 0xf"
    );
}

#[test]
fn default_color_mask_writes_all_channels() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(
        pipeline_desc(&sink.batches[0]).color_targets[0].write_mask,
        0xf,
        "default (no glColorMask) writes all RGBA channels"
    );
}

#[test]
fn color_mask_all_false_lowers_to_zero_write_mask() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS);
    tri_vbo(&mut c);
    // A color-disabled pass (e.g. a depth/stencil-only prepass): no channel is written.
    record::color_mask(&mut c, false, false, false, false);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(
        pipeline_desc(&sink.batches[0]).color_targets[0].write_mask,
        0x0
    );
}

// ---------------------------------------------------------------------------------------------------
// glBindSampler: a bound sampler OBJECT overrides the texture's own filter/wrap in the lowered
// SamplerDesc (was faked — glBindSampler/glSamplerParameteri stored state the frame builder ignored).
// ---------------------------------------------------------------------------------------------------

const VS_T: &str =
    "attribute vec2 aPos;\nvarying vec2 vUV;\nvoid main(){ vUV = aPos; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS_T: &str = "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vUV); }\n";

/// Link + use a textured program (`uTex` -> unit 0) and bind a 2x2 texture with NEAREST/REPEAT params.
fn textured_setup(c: &mut GlContext) {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS_T);
    record::compile_shader(c, vs);
    let fsh = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fsh, FS_T);
    record::compile_shader(c, fsh);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fsh);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    record::uniform_sampler(c, 0, 0); // uTex -> texture unit 0
    tri_vbo(c);
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(c, 2, 2, &[0xABu8; 16]);
    record::tex_parameter(c, GL_TEXTURE_MIN_FILTER, GL_NEAREST);
    record::tex_parameter(c, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    record::tex_parameter(c, GL_TEXTURE_WRAP_S, GL_REPEAT);
    record::tex_parameter(c, GL_TEXTURE_WRAP_T, GL_REPEAT);
}

fn first_sampler_desc(batch: &[Cmd]) -> &SamplerDesc {
    batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateSampler(_, d) => Some(d),
            _ => None,
        })
        .expect("a CreateSampler for the sampled texture")
}

#[test]
fn bound_sampler_object_overrides_texture_filter_and_wrap() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    textured_setup(&mut c);
    // A sampler object with LINEAR filtering + CLAMP_TO_EDGE — the opposite of the texture's NEAREST/REPEAT.
    let samp = c.samplers.gen();
    es3::sampler_parameter(&mut c, samp, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32, 0.0);
    es3::sampler_parameter(&mut c, samp, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32, 0.0);
    es3::sampler_parameter(
        &mut c,
        samp,
        GL_TEXTURE_WRAP_S,
        GL_CLAMP_TO_EDGE as i32,
        0.0,
    );
    es3::sampler_parameter(
        &mut c,
        samp,
        GL_TEXTURE_WRAP_T,
        GL_CLAMP_TO_EDGE as i32,
        0.0,
    );
    es3::bind_sampler(&mut c, 0, samp);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let sd = first_sampler_desc(&sink.batches[0]);
    assert_eq!(
        sd.min_filter,
        Filter::Linear,
        "bound sampler object's LINEAR min-filter overrides texture NEAREST"
    );
    assert_eq!(
        sd.mag_filter,
        Filter::Linear,
        "object LINEAR mag-filter overrides texture NEAREST"
    );
    assert_eq!(
        sd.address_u,
        AddressMode::ClampToEdge,
        "object CLAMP_TO_EDGE overrides texture REPEAT"
    );
    assert_eq!(sd.address_v, AddressMode::ClampToEdge);
}

#[test]
fn no_sampler_object_leaves_texture_params_authoritative() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    textured_setup(&mut c);
    // No glBindSampler: the texture's own NEAREST/REPEAT must still drive the lowered sampler.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let sd = first_sampler_desc(&sink.batches[0]);
    assert_eq!(
        sd.min_filter,
        Filter::Nearest,
        "texture NEAREST wins when no sampler object is bound"
    );
    assert_eq!(
        sd.address_u,
        AddressMode::Repeat,
        "texture REPEAT wins when no sampler object is bound"
    );
}

// ---------------------------------------------------------------------------------------------------
// data uniform → the shipped uniform-block bytes
// ---------------------------------------------------------------------------------------------------

#[test]
fn uniform_value_is_written_into_the_shipped_uniform_block() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c, FS_U);
    tri_vbo(&mut c);

    // uColor is the program's single data uniform → declaration index 0.
    let color = [0.25f32, 0.5, 0.75, 1.0];
    let mut bytes = Vec::new();
    for v in color {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    record::uniform_at(&mut c, 0, &bytes);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The uniform buffer upload: CreateBuffer(UNIFORM) immediately followed by its WriteBuffer.
    let pos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::UNIFORM))
        .expect("a UNIFORM CreateBuffer");
    let id = match &batch[pos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    match &batch[pos + 1] {
        Cmd::WriteBuffer {
            id: wid,
            offset: 0,
            data,
        } => {
            assert_eq!(*wid, id);
            assert_eq!(
                data, &bytes,
                "the glUniform4fv value reaches the shipped uniform block"
            );
        }
        other => panic!("expected WriteBuffer after the UNIFORM CreateBuffer, got {other:?}"),
    }
}
