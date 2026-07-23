//! Lowering tests: drive the GL recording ops + the swap service against a `hl_gpu::RecordingSink` and
//! assert the exact protocol `Cmd`/`Enc` sequence a frame lowers to (plus the GLSL→shader-IR adapter).
//!
//! This is the acceptance gate for the GL→IR lowering layer: no socket, no GPU — just the recorded
//! command stream emitted at `eglSwapBuffers`. GL is deferred-lowering, so `gl*` recording submits
//! nothing; only `swap_buffers` touches the sink.

use hl_gl::adapter::glsl;
use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::BindResource;
use hl_gpu::protocol::model::enums::{buffer_usage, LoadOp};
use hl_gpu::{Cmd, RecordingSink, ShaderPayloadKind};

const VS: &str = "attribute vec2 aPos;\nvarying vec2 vUV;\nvoid main(){ vUV = aPos; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vUV); }\n";

fn ctx_640x480() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: 640,
        height: 480,
    };
    c
}

/// Find the single `Cmd::Submit` in a batch and return its encoder ops.
fn submit_ops(batch: &[Cmd]) -> &[Enc] {
    for cmd in batch {
        if let Cmd::Submit(cb) = cmd {
            return &cb.encoder;
        }
    }
    panic!("no Submit in batch: {batch:?}");
}

/// How many `CreateShader` commands a batch carries (the host naga-compiles one per command).
fn count_shaders(batch: &[Cmd]) -> usize {
    batch
        .iter()
        .filter(|c| matches!(c, Cmd::CreateShader { .. }))
        .count()
}

/// How many `CreateRenderPipeline` commands a batch carries.
fn count_pipelines(batch: &[Cmd]) -> usize {
    batch
        .iter()
        .filter(|c| matches!(c, Cmd::CreateRenderPipeline(_, _)))
        .count()
}

// ---------------------------------------------------------------------------------------------------
// recording layer (deferred — submits nothing)
// ---------------------------------------------------------------------------------------------------

/// Record a complete textured-quad frame: a VBO upload, a shader/program link, a 2x2 texture upload,
/// and one `glDrawArrays`, then swap.
fn record_textured_quad(c: &mut GlContext) {
    // vertex buffer
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    let verts: Vec<u8> = (0..48).map(|i| i as u8).collect(); // 6 verts * vec2 f32
    record::buffer_data(c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);

    // program
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    record::uniform_sampler(c, 0, 0); // uTex -> texture unit 0

    // texture
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(c, 2, 2, &[0xABu8; 16]); // 2x2 RGBA8
    record::tex_parameter(c, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    record::tex_parameter(c, GL_TEXTURE_MAG_FILTER, GL_LINEAR);

    record::viewport(c, [0, 0, 640, 480]);
    record::draw_arrays(c, GL_TRIANGLES, 0, 6);
}

#[path = "lowering/frame.rs"]
mod frame;
#[path = "lowering/program.rs"]
mod program;
#[path = "lowering/resource.rs"]
mod resource;
#[path = "lowering/state.rs"]
mod state;
#[path = "lowering/texture.rs"]
mod texture;
#[path = "lowering/glsl.rs"]
mod translation;
#[path = "lowering/vertex.rs"]
mod vertex;
