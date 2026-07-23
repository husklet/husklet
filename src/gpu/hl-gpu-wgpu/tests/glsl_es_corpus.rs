//! ANGLE-style GLSL-ES corpus battery — the EXECUTOR half of the Chrome shader wall.
//!
//! Chrome (via ANGLE) and GTK4 GskGpu forward *GLSL-ES* vertex/fragment shaders that naga-24's `glsl-in`
//! rejects wholesale. `crate::glsl_es::normalize` (ES->460, `gl_VertexID`->`gl_VertexIndex`, combined-sampler
//! split, matrix/array IO reshaping, switch lowering) makes them naga-acceptable before parse; this battery
//! proves it against a LARGE, diverse corpus of the constructs ANGLE actually emits.
//!
//! Each shader is pushed through the REAL executor path -- `Cmd::CreateShader { kind: Glsl }` ->
//! `glsl_to_wgsl_reflect` (ES-normalize -> naga glsl-in -> validate -> wgsl-out) -> `create_shader_module`
//! on the device. A shader that survives that reaches a VALID wgpu shader module (the DoD).
//!
//! Every corpus entry declares its `Expect`: `Pass` (must reach a module) or `NagaLimit` (a construct
//! naga-24 genuinely cannot model with any reasonable textual normalization -- asserted to STILL fail, with
//! the exact reason logged, so the limit is on the record and never faked into a green).

mod common;
use common::*;

use std::sync::{Mutex, MutexGuard, OnceLock};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
    RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
    ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

static EXEC: OnceLock<Option<Mutex<WgpuExecutor>>> = OnceLock::new();

fn exec() -> Option<MutexGuard<'static, WgpuExecutor>> {
    EXEC.get_or_init(|| {
        WgpuExecutor::new(DeviceConfig::default())
            .ok()
            .map(Mutex::new)
    })
    .as_ref()
    .map(|m| m.lock().unwrap_or_else(|e| e.into_inner()))
}

fn new_sess(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    )
}

/// Compile ONE GLSL-ES stage through the full executor shader path. `Ok` => it reached a valid wgpu shader
/// module. `Err(msg)` carries the exact glsl-in/validate/wgsl-out diagnostic.
fn compile(exec: &mut WgpuExecutor, stage: u32, entry: &str, src: &str) -> Result<(), String> {
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(stage, entry, src),
        }],
    )
    .map(|_| ())
    .map_err(|e| format!("{e:?}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Pass,
    NagaLimit(&'static str),
}
use Expect::*;

struct Case {
    name: &'static str,
    stage: u32,
    entry: &'static str,
    src: &'static str,
    expect: Expect,
}

const fn vs(name: &'static str, src: &'static str, expect: Expect) -> Case {
    Case {
        name,
        stage: glsl_stage::VERTEX,
        entry: "vmain",
        src,
        expect,
    }
}
const fn fs(name: &'static str, src: &'static str, expect: Expect) -> Case {
    Case {
        name,
        stage: glsl_stage::FRAGMENT,
        entry: "fmain",
        src,
        expect,
    }
}

// ===================================================================================================
// Real renders: a subset of the corpus is not just compiled but DRAWN, and an EXACT pixel is read back
// off the device -- proving the ES-normalized shaders execute, not merely translate. Each uses an ES
// `gl_VertexID` fullscreen-triangle vertex shader (itself exercising the ES vertex-index builtin path).
// ===================================================================================================

const W: u32 = 8;
const H: u32 = 8;

// A fullscreen triangle from `gl_VertexID` (no attributes) -- ANGLE's standard clear/blit vertex shape and
// an exercise of the ES vertex-index lowering.
const DRAW_VS: &str = r#"#version 300 es
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexID], 0.0, 1.0);
}
"#;

fn rt(w: u32, h: u32) -> TextureDesc {
    TextureDesc {
        width: w,
        height: h,
        depth: 1,
        mip_levels: 1,
        sample_count: 1,
        dim: TextureDim::D2,
        format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
        label: String::new(),
    }
}
fn ct() -> ColorTargetState {
    ColorTargetState {
        format: TextureFormat::Rgba8Unorm,
        blend: None,
        write_mask: 0xF,
    }
}

/// Draw `DRAW_VS` + `fs` over an 8x8 target (no bindings) and return the readback.
fn draw_plain(exec: &mut WgpuExecutor, fs_src: &str) -> Vec<u8> {
    let mut s = new_sess(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, rt(W, H)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", DRAW_VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs_src),
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
                    color_targets: vec![ct()],
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
    .expect("plain ES draw must run cleanly");
    exec.read_texture(&s.resources, 1).unwrap()
}

fn approx(a: [u8; 4], b: [u8; 4], tol: i16) -> bool {
    (0..4).all(|k| (a[k] as i16 - b[k] as i16).abs() <= tol)
}

#[path = "common/glsl_es_corpus_data.rs"]
mod corpus;
#[path = "glsl_es_corpus/compile.rs"]
mod corpus_test;
#[path = "glsl_es_corpus/mat2.rs"]
mod mat2;
#[path = "glsl_es_corpus/plain.rs"]
mod plain;
#[path = "glsl_es_corpus/sampler.rs"]
mod sampler;
