use super::compute::atomic_counter_program;
use super::*;

// (7) ADVERSARIAL — malformed / unsupported inputs must return a clean Err, never panic
// =================================================================================================

fn create_shader_only(kind: ShaderPayloadKind, words: Vec<u32>) -> Vec<Cmd> {
    vec![Cmd::CreateShader {
        id: 1,
        kind,
        spirv: words,
    }]
}

#[test]
fn malformed_spirv_bad_magic_errs() {
    let mut g = exec();
    assert!(try_batch(
        &mut g,
        &create_shader_only(ShaderPayloadKind::SpirV, vec![0xDEAD_BEEF, 0, 0])
    )
    .is_err());
}

#[test]
fn malformed_spirv_valid_magic_garbage_errs() {
    // A valid SPIR-V header (magic, version 1.0, gen 0, bound 2, schema 0) followed by an instruction word
    // claiming a 10-word length with no following words — a truncated stream naga's spv-in must reject
    // (rather than panic). ([magic,0,0,0,0] would be a *valid empty* module, so it is not used here.)
    let words = vec![SPIRV_MAGIC, 0x0001_0000, 0, 2, 0, 0x000A_0001];
    let mut g = exec();
    let r = try_batch(&mut g, &create_shader_only(ShaderPayloadKind::SpirV, words));
    assert!(
        r.is_err(),
        "truncated SPIR-V instruction stream must be a clean Err"
    );
}

#[test]
fn empty_spirv_words_errs() {
    let mut g = exec();
    assert!(try_batch(
        &mut g,
        &create_shader_only(ShaderPayloadKind::SpirV, vec![])
    )
    .is_err());
}

#[test]
fn malformed_glsl_errs() {
    let mut g = exec();
    let words = glsl_words(glsl_stage::VERTEX, "vmain", "this is not glsl @@@ ;;;");
    assert!(try_batch(&mut g, &create_shader_only(ShaderPayloadKind::Glsl, words)).is_err());
}

#[test]
fn legacy_msl_payload_rejected() {
    // MSL is not advertised → the runtime rejects it at validate; either way it never silently succeeds.
    let mut g = exec();
    assert!(try_batch(
        &mut g,
        &create_shader_only(ShaderPayloadKind::Msl, vec![0x1234_5678, 1, 2])
    )
    .is_err());
}

#[test]
fn demo_builtin_payload_rejected() {
    // DemoBuiltin passes the (bit==0) validate gate and must be rejected honestly by the executor.
    let mut g = exec();
    assert!(try_batch(
        &mut g,
        &create_shader_only(ShaderPayloadKind::DemoBuiltin, vec![1, 2, 3])
    )
    .is_err());
}

#[test]
fn compute_pipeline_from_graphics_shader_errs() {
    // A SPIR-V *compute* module is now accepted (see tests/spirv_compute.rs), but a graphics-ONLY module
    // (here only the vertex/fragment entries of SEED_VINDEX_GREEN) used for compute must still fail —
    // `vs_main` is a vertex entry, not a compute entry. The executor's error scope must turn wgpu's
    // validation error into a clean typed Err, not a panic.
    let spirv = wgsl_to_spirv(SEED_VINDEX_GREEN);
    let mut g = exec();
    let r = try_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "vs_main".into(),
                    },
                    label: String::new(),
                },
            ),
        ],
    );
    assert!(
        r.is_err(),
        "a graphics-only module (no compute entry point) must not build a compute pipeline"
    );
}

#[test]
fn render_pipeline_from_kernel_shader_errs() {
    let mut g = exec();
    g.define_kernel(1, atomic_counter_program());
    let r = try_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::PtxKernel,
                spirv: kernel_words(),
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "counter".into(),
                    },
                    fragment: None,
                    vertex_buffers: vec![],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
        ],
    );
    assert!(
        r.is_err(),
        "a render pipeline vertex stage needs a graphics shader, not a kernel"
    );
}

#[test]
fn unsupported_vertex_format_errs() {
    // Unorm8x1 has no WebGPU vertex format → the pipeline lowering must reject it, not silently widen.
    let spirv = wgsl_to_spirv(SEED_POS2_GREEN);
    let layout = VertexLayout {
        stride: 4,
        step_mode: 0,
        attrs: vec![VertexAttr {
            location: 0,
            format: vfmt(1, 1, false),
            offset: 0,
        }], // u8x1
    };
    let mut g = exec();
    let r = try_batch(
        &mut g,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv,
            },
            Cmd::CreateRenderPipeline(
                1,
                RenderPipelineDesc {
                    vertex: ShaderRef {
                        module: 1,
                        entry: "vs_main".into(),
                    },
                    fragment: Some(ShaderRef {
                        module: 1,
                        entry: "fs_main".into(),
                    }),
                    vertex_buffers: vec![layout],
                    color_targets: vec![ColorTargetState {
                        format: TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: 0xF,
                    }],
                    depth: None,
                    topology: Topology::TriangleList,
                    cull: 0,
                    front_face: 0,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
        ],
    );
    assert!(
        r.is_err(),
        "unsupported vertex attribute format must be a clean Err"
    );
}

#[test]
fn out_of_bounds_buffer_read_errs() {
    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[Cmd::CreateBuffer(
            1,
            buf(8, buffer_usage::COPY_DST | buffer_usage::COPY_SRC),
        )],
    );
    assert!(
        g.read_buffer(&s.resources, BufferId(1), 4, 8).is_err(),
        "read past end must be OutOfBounds"
    );
}

// =================================================================================================
