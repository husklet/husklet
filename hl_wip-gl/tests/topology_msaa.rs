//! Lowering tests for the primitive TOPOLOGY and MSAA sample-count fields threaded into the render
//! pipeline descriptor by [`hl_gl::service::frame`].
//!
//! Before this pass the frame builder distinguished only `GL_TRIANGLE_STRIP` and folded EVERY other GL
//! primitive mode onto `Topology::TriangleList` — so a `glDrawArrays(GL_LINES)` / `GL_POINTS` /
//! `GL_LINE_STRIP` silently rasterized as triangles — and it hardcoded `sample_count: 1`. Each GL mode
//! with a neutral equivalent now maps to it, and the sample count is sourced from one documented place
//! (`framebuffer_sample_count`), which for every framebuffer this model can currently represent (all
//! single-sampled — there is no `glRenderbufferStorageMultisample` entry point yet) is 1.
//!
//! Driven through the `record` + `swap` services against a `hl_gpu::RecordingSink`, mirroring
//! `tests/fixedfunc.rs`.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::descriptor::RenderPipelineDesc;
use hl_gpu::protocol::model::enums::{TextureFormat, Topology};
use hl_gpu::{Cmd, RecordingSink};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

// GL primitive modes without a `glconst` symbol in this shim (they have no neutral `Topology` equivalent,
// so they lower via documented fallbacks — see `gl_topology`). Named here for readable assertions.
const GL_LINE_LOOP: u32 = 0x0002;
const GL_TRIANGLE_FAN: u32 = 0x0006;

fn ctx_64() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 64, height: 64 };
    c
}

fn flat_program(c: &mut GlContext) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fsh = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fsh, FS);
    record::compile_shader(c, fsh);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fsh);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    prog
}

fn tri_vbo(c: &mut GlContext) {
    let vbo = record::gen_buffer(c);
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(c, GL_ARRAY_BUFFER, &vec![0u8; 24], 0x88E4);
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

/// Lower a single `glDrawArrays(mode)` into the default framebuffer and return the pipeline it produced.
fn lower_default_fbo_draw(mode: u32) -> RenderPipelineDesc {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c);
    tri_vbo(&mut c);
    record::draw_arrays(&mut c, mode, 0, 4);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    pipeline_desc(&sink.batches[0]).clone()
}

// ---------------------------------------------------------------------------------------------------
// GL primitive mode -> neutral Topology
// ---------------------------------------------------------------------------------------------------

#[test]
fn draw_points_lowers_to_point_list() {
    assert_eq!(
        lower_default_fbo_draw(GL_POINTS).topology,
        Topology::PointList,
        "glDrawArrays(GL_POINTS) must rasterize as points, not fold to TriangleList"
    );
}

#[test]
fn draw_lines_lowers_to_line_list() {
    assert_eq!(
        lower_default_fbo_draw(GL_LINES).topology,
        Topology::LineList,
        "glDrawArrays(GL_LINES) must rasterize as line segments, not triangles"
    );
}

#[test]
fn draw_line_strip_lowers_to_line_strip() {
    assert_eq!(lower_default_fbo_draw(GL_LINE_STRIP).topology, Topology::LineStrip);
}

#[test]
fn draw_triangles_lowers_to_triangle_list() {
    assert_eq!(lower_default_fbo_draw(GL_TRIANGLES).topology, Topology::TriangleList);
}

#[test]
fn draw_triangle_strip_lowers_to_triangle_strip() {
    assert_eq!(lower_default_fbo_draw(GL_TRIANGLE_STRIP).topology, Topology::TriangleStrip);
}

#[test]
fn line_loop_lowers_to_line_strip_documented_approximation() {
    // GL_LINE_LOOP has no neutral variant; the closest honored LINE topology is LineStrip (every segment
    // except the implicit closing edge). An honest approximation that stays a line primitive — NOT a
    // silent fold to triangles.
    assert_eq!(lower_default_fbo_draw(GL_LINE_LOOP).topology, Topology::LineStrip);
}

#[test]
fn triangle_fan_falls_back_to_triangle_list_documented_gap() {
    // GL_TRIANGLE_FAN has no neutral fan topology; it keeps the safe TRIANGLE fallback rather than a wrong
    // primitive class. Documented gap (a fan drawn as an independent-triangle list mis-connects its
    // shared-vertex fan), asserted here so the fallback is intentional and never silently changes.
    assert_eq!(lower_default_fbo_draw(GL_TRIANGLE_FAN).topology, Topology::TriangleList);
}

#[test]
fn unknown_mode_never_panics_and_defaults_to_triangle_list() {
    // A bogus GL primitive enum must not panic — safe TriangleList default.
    assert_eq!(lower_default_fbo_draw(0xBEEF).topology, Topology::TriangleList);
}

// ---------------------------------------------------------------------------------------------------
// MSAA sample count -> the lowered pipeline descriptor
// ---------------------------------------------------------------------------------------------------

#[test]
fn default_framebuffer_draw_is_single_sampled() {
    // The default window framebuffer is single-sampled: the pipeline declares sample_count 1 (no longer a
    // blind hardcode — sourced from `framebuffer_sample_count`).
    assert_eq!(
        lower_default_fbo_draw(GL_TRIANGLES).sample_count,
        1,
        "a plain (non-multisample) framebuffer lowers sample_count 1"
    );
}

#[test]
fn plain_offscreen_fbo_draw_is_single_sampled() {
    // A plain (non-multisample) offscreen FBO — a single-sampled color texture attachment — also lowers
    // sample_count 1. NOTE: this model has no multisample-attachment representation yet (no
    // `glRenderbufferStorageMultisample` / `glTexStorage2DMultisample` entry point, no per-resource
    // `samples`), so a sample_count > 1 case is not yet expressible; every representable FBO is
    // single-sampled and this locks that non-regression.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d_format(&mut c, 32, 32, &[], TextureFormat::Rgba8Unorm);
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);

    flat_program(&mut c);
    tri_vbo(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 4);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(pipeline_desc(&sink.batches[0]).sample_count, 1, "a plain offscreen FBO lowers sample_count 1");
}

#[test]
fn distinct_topologies_mint_distinct_pipelines() {
    // The pipeline residency key folds topology in, so the same program drawn as triangles then as lines
    // creates TWO pipelines (not one reused with the wrong primitive).
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c);
    tri_vbo(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 4);
    record::draw_arrays(&mut c, GL_LINES, 0, 4);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());

    let pipelines: Vec<Topology> = sink.batches[0]
        .iter()
        .filter_map(|c| match c {
            Cmd::CreateRenderPipeline(_, d) => Some(d.topology),
            _ => None,
        })
        .collect();
    assert!(
        pipelines.contains(&Topology::TriangleList) && pipelines.contains(&Topology::LineList),
        "topology is part of the pipeline key: got {pipelines:?}"
    );
}
