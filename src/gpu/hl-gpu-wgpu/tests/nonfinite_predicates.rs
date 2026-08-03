//! `isnan` / `isinf` must return the RIGHT ANSWER — the property that was never under test.
//!
//! Both builtins had coverage before this file, and it asserted that the shader COMPILES and that no
//! `IsInf` instruction survives into the emitted WGSL (`src/wgsl/tests.rs`). Neither of those is the
//! property in question. A rewrite can satisfy both while returning constant `false` at runtime, and that
//! is exactly the failure mode these builtins have: on Metal the shader compiler defaults to fast math,
//! under which it may assume no operand is NaN or infinite and fold any such test away. Nothing in the
//! tree asserted a VALUE, on any backend, so the workaround has been green throughout on a question nobody
//! was asking.
//!
//! WGSL has no `isNan`/`isInf` builtins at all — gpuweb removed them (gpuweb#2311) precisely because
//! backends assuming fast math made them unreliable — so naga's `wgsl-out` emits neither, and every such
//! test has to survive as ordinary arithmetic this crate writes itself.
//!
//! METHOD: the values under test arrive through a UNIFORM BUFFER as raw bit patterns, so no compiler can
//! constant-fold the answer at translation time; it must actually test the value it loaded. Each output
//! channel carries one predicate, and the finite control channel must come back 0 — a rewrite that
//! answered "true" for everything is as wrong as one that answers "false", and this catches both.

mod gpu_harness;
use gpu_harness::{color_target, glsl, near, new_session, px, tex2d};

use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, RenderPipelineDesc,
    ShaderRef,
};
use hl_gpu::protocol::model::enums::{buffer_usage, texture_usage, LoadOp, Topology};
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, CommandBuffer, Enc, GpuExecutor, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 4;
const H: u32 = 4;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

/// R = `isinf(+INF)`, G = `isnan(NaN)`, B = the FINITE CONTROL (`isinf(1.0) || isnan(1.0)`), A =
/// `isinf(-INF)`. Expected `(1, 1, 0, 1)`.
const FS: &str = r#"#version 460
layout(std140, binding = 0) uniform HlUniforms { vec4 v; };
layout(location = 0) out vec4 o;
void main() {
    o = vec4(
        isinf(v.x) ? 1.0 : 0.0,
        isnan(v.y) ? 1.0 : 0.0,
        (isinf(v.z) || isnan(v.z)) ? 1.0 : 0.0,
        isinf(v.w) ? 1.0 : 0.0
    );
}
"#;

/// The four f32 bit patterns, uploaded raw so the values are genuinely runtime data: `+INF`, a quiet
/// `NaN`, finite `1.0`, `-INF`.
fn uniform_bytes() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(16);
    for bits in [0x7F80_0000u32, 0x7FC0_0000, 0x3F80_0000, 0xFF80_0000] {
        bytes.extend_from_slice(&bits.to_le_bytes());
    }
    bytes
}

/// Mint the SAME fragment shader as real SPIR-V (`glsl-in` → validate → `spv-out`), which is what a
/// precompiled-shader guest sends. `OpIsInf`/`OpIsNan` survive into the payload, so this reaches the
/// translator's own representation rather than the textual source rewrite — the one route a GLSL rewrite
/// can never cover.
fn fragment_spirv(source: &str) -> Vec<u32> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(
            &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
            source,
        )
        .expect("glsl-in accepts isinf/isnan — the gap is wgsl-out, not the front end");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("the module validates — the predicates are legal naga IR");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("spv-out emits OpIsInf/OpIsNan")
}

/// Which payload kind carries the fragment shader — the same source, reaching the translator two ways.
#[derive(Clone, Copy, Debug)]
enum Payload {
    Glsl,
    SpirV,
}

impl Payload {
    fn shader(self, source: &str) -> Cmd {
        match self {
            Self::Glsl => Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", source),
            },
            Self::SpirV => Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::SpirV,
                spirv: fragment_spirv(source),
            },
        }
    }

    /// The SPIR-V front end keeps naga's own entry-point name (`main`); the GLSL path is told `fmain`.
    fn entry(self) -> &'static str {
        match self {
            Self::Glsl => "fmain",
            Self::SpirV => "main",
        }
    }
}

fn render(exec: &mut WgpuExecutor, payload: Payload, source: &str) -> hl_gpu::Result<Vec<u8>> {
    let mut session = new_session(exec);
    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex2d(W, H, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC),
            ),
            Cmd::CreateBuffer(
                1,
                BufferDesc {
                    size: 16,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data: uniform_bytes(),
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            payload.shader(source),
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vmain".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 2,
                        entry: payload.entry().into(),
                    }),
                    vertex_buffers: vec![],
                    color_targets: vec![color_target()],
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
                    entries: vec![BindEntry {
                        binding: 0,
                        resource: BindResource::Buffer {
                            id: 1,
                            offset: 0,
                            size: 16,
                        },
                    }],
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
    )?;
    exec.read_texture(&session.resources, 1)
}

/// The DoD: both predicates answer correctly for runtime values, and the finite control answers `false`.
///
/// UNCONFIRMED ON METAL. This runs green on Vulkan/lavapipe whether or not the rewrite is bit-pattern
/// based, because Vulkan preserves NaN and Inf and does not enable fast math — which is the whole reason
/// the previous coverage never caught anything. The failure this guards is Metal-specific and needs a host
/// run; see the crate's report for the exact command.
#[test]
fn isnan_and_isinf_answer_correctly_for_runtime_values() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    for payload in [Payload::Glsl, Payload::SpirV] {
        let pixels = render(&mut exec, payload, FS).unwrap_or_else(|e| {
            panic!("{payload:?}: a shader using isnan/isinf must compile and draw: {e}")
        });
        let expected = [255, 255, 0, 255];
        for y in 0..H {
            for x in 0..W {
                let got = px(&pixels, W, x, y);
                assert!(
                    near(got, expected),
                    "{payload:?} pixel ({x},{y}) is {got:?}, expected {expected:?} \
                     (R=isinf(+INF), G=isnan(NaN), B=finite control, A=isinf(-INF)) — a channel that came \
                     back 0 means the predicate was folded away; a B of 255 means it answers true for everything"
                );
            }
        }
    }
}

/// The predicates used from inside CONTROL FLOW and a HELPER FUNCTION, so the arena rebuild has to carry
/// more than a straight-line expression list.
///
/// This exists because of what the rebuild is: replacing a predicate means rebuilding a function's whole
/// expression arena and remapping every handle that pointed into it. An exhaustive `match` makes the
/// compiler catch a MISSING variant, but it cannot catch a variant remapped WRONGLY, and the simple case
/// above only walks a handful. This one routes the predicates through a call, a loop, nested branches and
/// a dynamically-indexed vector, so `Call`/`CallResult`, `If`, `Loop`, `Return` and `Access` all pass
/// through the remap with an answer that is checked, not merely compiled.
const FS_CONTROL_FLOW: &str = r#"#version 460
layout(std140, binding = 0) uniform HlUniforms { vec4 v; };
layout(location = 0) out vec4 o;

float classify(float x) {
    if (isnan(x)) { return 0.25; }
    if (isinf(x)) { return 0.5; }
    return 1.0;
}

void main() {
    float acc = 0.0;
    for (int i = 0; i < 2; ++i) {
        acc += classify(v[i]);
    }
    o = vec4(classify(v.z), acc, classify(v.w), 1.0);
}
"#;

#[test]
fn predicates_survive_the_arena_rebuild_through_calls_and_control_flow() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    // v = [+INF, NaN, 1.0, -INF]: R = classify(1.0) = 1.0; G = classify(+INF) + classify(NaN) = 0.75;
    // B = classify(-INF) = 0.5; A = 1.0. Every channel is a DIFFERENT value, so a remap that crossed two
    // handles shows up as a wrong colour rather than a wrong shape.
    let expected = [255, 191, 128, 255];
    for payload in [Payload::Glsl, Payload::SpirV] {
        let pixels = render(&mut exec, payload, FS_CONTROL_FLOW).unwrap_or_else(|e| {
            panic!("{payload:?}: the control-flow shader must compile and draw: {e}")
        });
        for y in 0..H {
            for x in 0..W {
                let got = px(&pixels, W, x, y);
                assert!(
                    near(got, expected),
                    "{payload:?} pixel ({x},{y}) is {got:?}, expected {expected:?} — a handle remapped \
                     to the wrong expression during the arena rebuild changes the value, not the shape"
                );
            }
        }
    }
}
