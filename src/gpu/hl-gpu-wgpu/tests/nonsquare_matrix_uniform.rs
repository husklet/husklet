//! Non-square matrix uniforms are laid out at the std140 column stride the driver writes.
//!
//! A GLES3 smoke run partitioned perfectly: every non-square matrix type failed, every square one passed.
//! The hypothesis worth testing in this layer is not whether the type can be EXPRESSED — it can — but
//! whether it is expressed at the right COLUMN STRIDE. std140 pads every matrix column to 16 bytes
//! regardless of its row count, so an implementation that sized columns by their natural width would read
//! `mat2x3` columns 12 bytes apart instead of 16 and return neighbouring data, silently.
//!
//! Every type is driven through ONE shader shape: all nine have at least two columns and two rows, so
//! `m[0][0]`, `m[1][0]`, `m[0][1]`, `m[1][1]` exist for each and land in four different output channels.
//! The uploaded bytes are written at the std140 positions the GL driver uses (column `c`, row `r` at
//! `16*c + 4*r`), so a reader using any other stride picks up a value this test can name.
//!
//! The square types are included deliberately as the CONTROL. If the reported partition originates here,
//! the square ones pass and the non-square ones fail; if all nine pass, the partition comes from somewhere
//! else and this test says so by staying green.

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

/// `(type, columns)` — every matrix form GLSL ES 3.0 defines.
const MATRICES: [(&str, u32); 9] = [
    ("mat2", 2),
    ("mat3", 3),
    ("mat4", 4),
    ("mat2x3", 2),
    ("mat2x4", 2),
    ("mat3x2", 3),
    ("mat3x4", 3),
    ("mat4x2", 4),
    ("mat4x3", 4),
];

/// The four elements read, and the value written to each. Distinct so a column read at the wrong stride,
/// or a row/column transposition, lands on a value that names which mistake it was.
const M00: f32 = 0.2;
const M10: f32 = 0.4;
const M01: f32 = 0.6;
const M11: f32 = 0.8;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

fn fs(ty: &str) -> String {
    format!(
        "#version 460\nlayout(std140, binding = 0) uniform HlUniforms {{ {ty} m; }};\n\
         layout(location = 0) out vec4 o;\n\
         void main() {{ o = vec4(m[0][0], m[1][0], m[0][1], m[1][1]); }}\n"
    )
}

/// The same read, but from element 1 of a two-element ARRAY of matrices.
fn fs_array(ty: &str) -> String {
    format!(
        "#version 460\nlayout(std140, binding = 0) uniform HlUniforms {{ {ty} m[2]; }};\n\
         layout(location = 0) out vec4 o;\n\
         void main() {{ o = vec4(m[1][0][0], m[1][1][0], m[1][0][1], m[1][1][1]); }}\n"
    )
}

/// std140 bytes for a TWO-element array of matrices: element `e`, column `c`, row `r` at
/// `16 * (e * columns + c) + 4 * r` — a matrix column occupies its own 16-byte slot whether or not the
/// matrix is in an array, so the array is just a longer run of the same slots.
///
/// Element 0 is filled with DECOYS. Reading the wrong element returns them, and they are far enough from
/// the expected values that the assertion names the mistake rather than tolerating it.
fn uniform_bytes_array(columns: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; (columns as usize) * 2 * 16];
    let mut put = |element: usize, column: usize, row: usize, value: f32| {
        let at = (element * columns as usize + column) * 16 + row * 4;
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    for (column, row) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        put(0, column, row, 1.0); // decoy: element 0 reads as full white
    }
    put(1, 0, 0, M00);
    put(1, 1, 0, M10);
    put(1, 0, 1, M01);
    put(1, 1, 1, M11);
    bytes
}

/// std140 bytes for a matrix with `columns` columns: column `c`, row `r` sits at `16*c + 4*r`. This is what
/// the GL driver writes — it lays matrix columns out at an explicit `column * 16`.
fn uniform_bytes(columns: u32) -> Vec<u8> {
    let mut bytes = vec![0u8; (columns as usize) * 16];
    let mut put = |column: usize, row: usize, value: f32| {
        let at = column * 16 + row * 4;
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    put(0, 0, M00);
    put(1, 0, M10);
    put(0, 1, M01);
    put(1, 1, M11);
    bytes
}

fn render(exec: &mut WgpuExecutor, ty: &str, columns: u32, array: bool) -> hl_gpu::Result<Vec<u8>> {
    let mut session = new_session(exec);
    let elements = if array { 2 } else { 1 };
    let size = u64::from(columns) * 16 * elements;
    let (source, data) = if array {
        (fs_array(ty), uniform_bytes_array(columns))
    } else {
        (fs(ty), uniform_bytes(columns))
    };
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
                    size,
                    usage: buffer_usage::UNIFORM,
                    label: String::new(),
                },
            ),
            Cmd::WriteBuffer {
                id: 1,
                offset: 0,
                data,
            },
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", &source),
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
                            size,
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

#[test]
fn every_matrix_shape_reads_its_own_std140_elements() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let expected = [
        (M00 * 255.0).round() as u8,
        (M10 * 255.0).round() as u8,
        (M01 * 255.0).round() as u8,
        (M11 * 255.0).round() as u8,
    ];
    let mut failures = Vec::new();
    for (ty, columns) in MATRICES {
        match render(&mut exec, ty, columns, false) {
            Err(e) => failures.push(format!("{ty}: refused: {e}")),
            Ok(pixels) => {
                let got = px(&pixels, W, 0, 0);
                if !near(got, expected) {
                    failures.push(format!("{ty}: got {got:?}, want {expected:?}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "matrix uniforms must read their own std140 elements (column c, row r at 16c+4r):\n  {}",
        failures.join("\n  ")
    );
}

/// The same nine shapes, in a two-element ARRAY.
///
/// Arrays of TWO-ROW matrices (`mat2`, `mat3x2`, `mat4x2`) were refused outright until now: the column
/// split that makes a two-row matrix expressible at all — naga rejects `matNx2` in `std140` — declined any
/// member with a subscript, so three of the nine shapes could not appear in a uniform array. That set
/// contains a square type, which is why it was not the non-square partition; it was its own gap, and an
/// array of matrices in a uniform block is an ordinary thing to write.
///
/// The flattening is byte-identical to what the driver uploads, and this asserts that rather than assuming
/// it: element 0 holds decoys, so reading the wrong element or striding the wrong way returns white.
#[test]
fn every_matrix_shape_reads_its_own_elements_from_an_array() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let expected = [
        (M00 * 255.0).round() as u8,
        (M10 * 255.0).round() as u8,
        (M01 * 255.0).round() as u8,
        (M11 * 255.0).round() as u8,
    ];
    let mut failures = Vec::new();
    for (ty, columns) in MATRICES {
        match render(&mut exec, ty, columns, true) {
            Err(e) => failures.push(format!("{ty}[2]: refused: {e}")),
            Ok(pixels) => {
                let got = px(&pixels, W, 0, 0);
                if !near(got, expected) {
                    failures.push(format!("{ty}[2]: got {got:?}, want {expected:?}"));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "an array of matrices must read element 1's own std140 elements \
         (element e, column c, row r at 16*(e*C+c)+4*r):\n  {}",
        failures.join("\n  ")
    );
}

/// A matrix VARYING carries its values across the stage boundary — on both dialect routes.
///
/// WGSL has no matrix shader inputs or outputs, so every matrix varying must be split into per-location
/// vector slots with a private global bridging them inside `main`. That pass existed, and ran only on the
/// ES route; the GL driver rewrites its shaders to desktop form before they arrive, so the driver's own
/// output was refused while an ES guest's was accepted. It is the same dialect gate that once hid the
/// two-row-matrix rewrite, in a second pass — which is why this test drives BOTH spellings of the same
/// shader rather than trusting one.
///
/// The vertex stage writes a matrix whose four read-back elements are distinct, so a column delivered to
/// the wrong location, or a row/column transposition in the split, changes the colour rather than the shape.
#[test]
fn a_matrix_varying_survives_both_dialect_routes() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    let expected = [
        (M00 * 255.0).round() as u8,
        (M10 * 255.0).round() as u8,
        (M01 * 255.0).round() as u8,
        (M11 * 255.0).round() as u8,
    ];
    let mut failures = Vec::new();
    for (ty, _) in MATRICES {
        for (dialect, header) in [
            ("es", "#version 300 es\nprecision highp float;\n"),
            ("desktop", "#version 460\n"),
        ] {
            // Column c row r = the element values above where they exist, 0 elsewhere.
            let vs = format!(
                "{header}layout(location = 0) out {ty} v;\n\
                 void main() {{\n\
                   v = {ty}(0.0);\n\
                   v[0][0] = {M00}; v[1][0] = {M10}; v[0][1] = {M01}; v[1][1] = {M11};\n\
                   vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));\n\
                   gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);\n\
                 }}\n"
            );
            let fs = format!(
                "{header}layout(location = 0) in {ty} v;\n\
                 layout(location = 0) out vec4 o;\n\
                 void main() {{ o = vec4(v[0][0], v[1][0], v[0][1], v[1][1]); }}\n"
            );
            match render_varying(&mut exec, &vs, &fs) {
                Err(e) => failures.push(format!("{ty} ({dialect}): refused: {e}")),
                Ok(pixels) => {
                    let got = px(&pixels, W, 0, 0);
                    if !near(got, expected) {
                        failures.push(format!("{ty} ({dialect}): got {got:?}, want {expected:?}"));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "a matrix varying must carry its values across the stage boundary on both routes:\n  {}",
        failures.join("\n  ")
    );
}

/// Render with the given vertex + fragment sources and read back the target.
fn render_varying(exec: &mut WgpuExecutor, vs: &str, fs: &str) -> hl_gpu::Result<Vec<u8>> {
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
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
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
                    color_targets: vec![color_target()],
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
    )?;
    exec.read_texture(&session.resources, 1)
}

/// RECORDED LIMIT — a two-row matrix inside a NESTED STRUCT in a std140 block is refused.
///
/// The column split that makes `matNx2` expressible at all rewrites members declared directly in the
/// block. A matrix reached through a struct (`struct S { mat3x2 m; }; uniform Blk { S s; };`) is not one of
/// those, so it arrives at naga intact and is rejected. The failing set is the two-row shapes — `mat2`,
/// `mat3x2`, `mat4x2` — and every other shape works, nested or not.
///
/// Not fixed here, and the reason is worth stating rather than leaving as an omission: the struct is
/// declared outside the block and may also be used where `matNx2` is perfectly legal, so rewriting its
/// member would change types the shader relies on elsewhere. Doing it properly means rewriting uses
/// (`s.m`) rather than the declaration, which needs the field-access tracking this textual pass does not
/// have. A refusal is the honest state until then.
///
/// This asserts the limit so it cannot be mistaken for working, and so it fails the moment someone makes
/// it pass — at which point the assertion below should become a value check like the ones above.
#[test]
fn two_row_matrices_nested_in_a_struct_are_a_recorded_limit() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    for (ty, _) in MATRICES {
        let two_row = matches!(ty, "mat2" | "mat3x2" | "mat4x2");
        let fs = format!(
            "#version 460\nstruct S {{ {ty} m; }};\n\
             layout(std140, binding = 0) uniform Blk {{ S s; }};\n\
             layout(location = 0) out vec4 o;\n\
             void main() {{ o = vec4(s.m[0][0], s.m[1][0], s.m[0][1], s.m[1][1]); }}\n"
        );
        let outcome = render_source(&mut exec, &fs);
        if two_row {
            assert!(
                outcome.is_err(),
                "{ty} nested in a struct is a KNOWN LIMIT. If it now compiles, the split reaches struct \
                 members — replace this with a value assertion"
            );
        } else {
            outcome.unwrap_or_else(|e| panic!("{ty} nested in a struct must still compile: {e}"));
        }
    }
}

/// Compile a fragment source through the executor's real shader path; the pipeline is not built, so this
/// isolates translation.
fn render_source(exec: &mut WgpuExecutor, fs: &str) -> hl_gpu::Result<()> {
    let mut session = new_session(exec);
    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, "fmain", fs),
        }],
    )
    .map(|_| ())
}
