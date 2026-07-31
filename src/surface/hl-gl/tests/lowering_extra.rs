//! Extra lowering coverage for op paths the existing suites don't pin: indexed/base-vertex draws, the
//! index-buffer format + offset, blend-equation-separate / cull-winding / primitive-topology pipeline
//! lowering, `glClearBufferfv`, and the honest "unlinked program presents nothing" path. Drives the real
//! recording ops + swap against a `RecordingSink` and asserts the exact emitted `Cmd`/`Enc`.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::RenderPipelineDesc;
use hl_gpu::protocol::model::enums::{IndexFormat, Topology};
use hl_gpu::{Cmd, RecordingSink};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "void main(){ gl_FragColor = vec4(1.0); }\n";

fn ctx() -> GlContext {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: 256,
        height: 256,
    });
    c
}

/// Link the flat program (no textures / uniforms), returning its GL name, WITHOUT binding it.
fn flat_program(c: &mut GlContext) -> u32 {
    let v = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, v, VS);
    let f = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, f, FS);
    let p = record::create_program(c);
    record::attach_shader(c, p, v);
    record::attach_shader(c, p, f);
    assert!(record::link_program(c, p));
    p
}

/// A bound program + a VBO of 4 vec2 verts with attribute 0 enabled.
fn setup_geometry(c: &mut GlContext) {
    let p = flat_program(c);
    record::use_program(c, p);
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);
    record::viewport(c, [0, 0, 256, 256]);
}

fn submit_ops(batch: &[Cmd]) -> &[Enc] {
    for cmd in batch {
        if let Cmd::Submit(cb) = cmd {
            return &cb.encoder;
        }
    }
    panic!("no Submit in batch");
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

#[path = "lowering_extra/clear.rs"]
mod clear;
#[path = "lowering_extra/indexed.rs"]
mod indexed;
#[path = "lowering_extra/indirect.rs"]
mod indirect;
#[path = "lowering_extra/pipeline.rs"]
mod pipeline;
#[path = "lowering_extra/stencil.rs"]
mod stencil;
#[path = "lowering_extra/unlinked.rs"]
mod unlinked;
#[path = "lowering_extra/mrt.rs"]
mod mrt;
#[path = "lowering_extra/vformat.rs"]
mod vformat;
