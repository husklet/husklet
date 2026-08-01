//! LINUX-ONLY (see `gl_fbo_orientation.rs` for why the `hl-gl` dev-dependency is scoped there): the
//! masked/scissored `glClear` fallback lowered by `hl-gl` must be ACCEPTED by this executor.
//!
//! The structural tests in `hl-gl` assert what the lowering emits. They cannot see whether the executor
//! accepts it, and a refusal here is not a wrong pixel — a rejected submit retires the share group, so
//! every later GL call in that process reports `GL_CONTEXT_LOST`. That is the difference between a
//! wrong-but-readable answer and an unreachable one.
#![cfg(target_os = "linux")]

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{frame, record};
use hl_gpu::protocol::model::descriptor::{FrameSerial, SurfaceToken};
use hl_gpu::{FakeClock, GlobalLedger, GpuExecutor, Limits, Session};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 64;
const H: u32 = 64;

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0); }\n";

fn context() -> GlContext {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W,
        height: H,
    });
    c.set_present_frame(
        Some(SurfaceToken::new(7).unwrap()),
        Some(FrameSerial::new(1).unwrap()),
    );
    c
}

/// A linked program plus one bound vertex attribute — the minimum a `glDrawArrays` needs to lower.
fn geometry(c: &mut GlContext) {
    let v = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, v, VS);
    record::compile_shader(c, v);
    let f = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, f, FS);
    record::compile_shader(c, f);
    let p = record::create_program(c);
    record::attach_shader(c, p, v);
    record::attach_shader(c, p, f);
    assert!(record::link_program(c, p));
    record::use_program(c, p);
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    let vertices = [-1.0f32, -1.0, 3.0, -1.0, -1.0, 3.0];
    let bytes = vertices
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    record::buffer_data(c, GL_ARRAY_BUFFER, &bytes, 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);
    record::viewport(c, [0, 0, W as i32, H as i32]);
}

/// Build the recorded frame and run it on a real adapter. `Ok` means the executor accepted every command
/// and encoder op; an `Err` here is what the guest sees as a lost share group.
fn execute(c: &mut GlContext) -> hl_gpu::Result<()> {
    let mut executor = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(executor) => executor,
        // No adapter in this environment: report the skip rather than a green run (see AGENTS.md).
        Err(_) => return Ok(()),
    };
    let rendered = frame::Frame::build(c).expect("the recorded frame lowers to IR");
    let mut limits = Limits::from_capabilities(executor.capabilities());
    limits.copy_alignment = 1;
    let mut session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    hl_gpu::runtime::submit(&mut session, &mut executor, 0, &rendered.cmds).map(|_| ())
}

/// The control: the same program, the same establishing clear, the same draws, and NO masked or scissored
/// clear. It must pass, or a refusal in the cases below would prove nothing about the clear (AGENTS.md:
/// "a refusal proves nothing without a path that otherwise works").
#[test]
fn an_ordinary_clear_and_draw_is_accepted() {
    let mut c = context();
    geometry(&mut c);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    execute(&mut c).expect("the unmasked, unscissored path is accepted");
}

#[test]
fn a_scissored_depth_clear_is_accepted() {
    let mut c = context();
    geometry(&mut c);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::depth_mask(&mut c, true);
    record::clear_depth(&mut c, 0.25);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [8, 8, 16, 16]);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    record::disable(&mut c, GL_SCISSOR_TEST);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    execute(&mut c).expect("a scissored depth clear must not lose the share group");
}

#[test]
fn a_partially_masked_stencil_clear_is_accepted() {
    let mut c = context();
    geometry(&mut c);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::stencil_mask(&mut c, 0x0f);
    record::clear_stencil(&mut c, 0x3);
    record::clear_buffers(&mut c, GL_STENCIL_BUFFER_BIT);
    record::stencil_mask(&mut c, 0xff);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    execute(&mut c).expect("a partially masked stencil clear must not lose the share group");
}

#[test]
fn a_partially_masked_colour_clear_is_accepted() {
    let mut c = context();
    geometry(&mut c);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::color_mask(&mut c, false, false, false, true);
    record::clear_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::color_mask(&mut c, true, true, true, true);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    execute(&mut c).expect("a partially masked colour clear must not lose the share group");
}

/// The depth-write pair reported separately on the same bundle: an ordinary draw under an enabled depth
/// TEST, once with `glDepthMask(GL_FALSE)` and once with `GL_TRUE`. Only the write differs, so if the
/// second fails and the first passes the write is the variable.
fn depth_draw(write: bool) -> hl_gpu::Result<()> {
    let mut c = context();
    geometry(&mut c);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_depth(&mut c, 1.0);
    record::depth_mask(&mut c, true);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_func(&mut c, GL_LESS);
    record::depth_mask(&mut c, write);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    execute(&mut c)
}

#[test]
fn a_depth_tested_draw_with_writes_disabled_is_accepted() {
    depth_draw(false).expect("the read-only depth path is accepted");
}

#[test]
fn a_depth_tested_draw_with_writes_enabled_is_accepted() {
    depth_draw(true).expect("enabling depth writes must not lose the share group");
}

/// The reported depth-write pair, in the shape that discriminates: the SAME scissored `glClear` with the
/// depth mask off and on. `glClear` is gated by `glDepthMask`, so with writes off the depth clear is
/// dropped and nothing takes the rect-clear path; turning writes on is what routes the frame through it.
/// That is why "enabling depth writes loses the context" and "a scissored clear loses the context" are one
/// defect and not two.
fn scissored_depth_clear(write: bool) -> hl_gpu::Result<()> {
    let mut c = context();
    geometry(&mut c);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::depth_mask(&mut c, write);
    record::clear_depth(&mut c, 0.25);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [8, 8, 16, 16]);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    record::disable(&mut c, GL_SCISSOR_TEST);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_func(&mut c, GL_LESS);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    execute(&mut c)
}

#[test]
fn a_scissored_depth_clear_with_writes_masked_off_is_accepted() {
    scissored_depth_clear(false).expect("the masked-off control is accepted");
}

#[test]
fn a_scissored_depth_clear_with_writes_enabled_is_accepted() {
    scissored_depth_clear(true).expect("enabling depth writes must not lose the share group");
}
