//! Lowering tests for multi-pass, offscreen, and client-memory rendering.
//!
//! The tests drive the `record` and `swap` services against a [`RecordingSink`], inspecting the emitted
//! command stream without a socket, GPU, or guest library.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureFormat};
use hl_gpu::{Cmd, RecordingSink};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str =
    "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

fn ctx_64() -> GlContext {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: 64,
        height: 64,
    });
    c.set_present_frame(
        Some(hl_gpu::protocol::model::descriptor::SurfaceToken::new(7).unwrap()),
        Some(hl_gpu::protocol::model::descriptor::FrameSerial::new(1).unwrap()),
    );
    c
}

/// Link + bind a flat-color program (no uniforms/samplers) and return its GL name.
fn flat_program(c: &mut GlContext) -> u32 {
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
    prog
}

/// Upload a 3-vertex position VBO and point attribute 0 at it (`stride` bytes/vertex).
fn tri_vbo(c: &mut GlContext, stride: i32) -> u32 {
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    let verts = vec![0u8; 3 * stride as usize];
    record::buffer_data(c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, stride, 0);
    record::enable_vertex_attrib(c, 0);
    vbo
}

/// The single `Cmd::Submit` encoder ops in a batch.
fn submit_ops(batch: &[Cmd]) -> &[Enc] {
    for cmd in batch {
        if let Cmd::Submit(cb) = cmd {
            return &cb.encoder;
        }
    }
    panic!("no Submit in batch: {batch:?}");
}

/// The render-target `CreateTexture` carrying `RENDER_TARGET | PRESENT` usage.
fn render_target_desc(batch: &[Cmd]) -> (u32, &hl_gpu::protocol::model::descriptor::TextureDesc) {
    batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d)
                if d.usage & (texture_usage::RENDER_TARGET | texture_usage::PRESENT)
                    == (texture_usage::RENDER_TARGET | texture_usage::PRESENT) =>
            {
                Some((*id, d))
            }
            _ => None,
        })
        .expect("a render-target CreateTexture")
}

#[path = "render/client.rs"]
mod client;
#[path = "render/graph.rs"]
mod graph;
#[path = "render/order.rs"]
mod order;
#[path = "render/pass.rs"]
mod pass;
#[path = "render/range.rs"]
mod range;
#[path = "render/target.rs"]
mod target;
#[path = "render/texture.rs"]
mod texture;
#[path = "render/transaction.rs"]
mod transaction;
