use super::*;

#[test]
fn glsl_and_legacy_graphics_shaders_are_accepted_opaquely_by_the_executor() {
    // The fixed-function CPU oracle rasterizes from the pipeline + vertex data, not the shader source, so a
    // forwarded GLSL / MSL graphics module is an accepted opaque handle at the executor boundary.
    let gd = GlslDescriptor {
        stage: glsl_stage::VERTEX,
        entry: "vmain".into(),
        source: "#version 460\nvoid main(){}\n".into(),
    };
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    exec.execute(
        &mut res,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: gd.to_words(),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Msl,
                spirv: vec![0x4141_4141],
            },
        ],
    )
    .expect("opaque graphics shader modules are accepted by the CPU executor");
    assert_eq!(res.shaders.len(), 2);
}

// ---------------------------------------------------------------------------------------------------
// runtime-pipeline validation (validate.rs) rejects an unsupported shader payload before execution
// ---------------------------------------------------------------------------------------------------

#[test]
fn runtime_rejects_a_shader_payload_the_backend_never_advertised() {
    use hl_gpu::CommandSink;
    // The CpuExecutor advertises only the KERNEL shader payload, so a GLSL CreateShader routed through the
    // full runtime pipeline is rejected at VALIDATE (a typed ResourceLimit), before the executor is touched.
    let gd = GlslDescriptor {
        stage: glsl_stage::VERTEX,
        entry: "v".into(),
        source: "x".into(),
    };
    let mut sink = InProcessCommandSink::new(CpuExecutor::new());
    let err = sink
        .submit(&[Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: gd.to_words(),
        }])
        .unwrap_err();
    assert_eq!(err, GpuError::ResourceLimit("shader payload"));
}
