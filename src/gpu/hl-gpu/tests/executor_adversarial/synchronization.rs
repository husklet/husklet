use super::*;

#[test]
fn wait_on_an_unsignalled_fence_value_is_rejected() {
    let (mut exec, mut res) = primed(&[Cmd::CreateFence(1)]);
    let err = exec
        .execute(&mut res, &[Cmd::WaitFence { id: 1, value: 5 }])
        .unwrap_err();
    assert_eq!(
        err,
        GpuError::Invalid("wait on a fence value that was never signalled")
    );
}

#[test]
fn present_size_mismatch_is_rejected() {
    let (mut exec, mut res) = primed(&[
        Cmd::CreateTexture(
            1,
            tex(4, 4, TextureFormat::Bgra8Unorm, texture_usage::PRESENT),
        ),
        Cmd::CreateSurface(
            1,
            SurfaceDesc {
                width: 8,
                height: 8,
                format: TextureFormat::Bgra8Unorm,
                token: hl_gpu::SurfaceToken::new(1).unwrap(),
            },
        ),
    ]);
    let err = exec
        .execute(
            &mut res,
            &[Cmd::Present {
                surface: 1,
                texture: 1,
                serial: hl_gpu::FrameSerial::new(1).unwrap(),
            }],
        )
        .unwrap_err();
    assert_eq!(
        err,
        GpuError::Invalid("present texture size does not match surface")
    );
}

// ---------------------------------------------------------------------------------------------------
// huge dims must not hang or panic; opaque graphics shaders are accepted
// ---------------------------------------------------------------------------------------------------

#[test]
fn huge_dispatch_grid_over_a_spirv_pipeline_short_circuits() {
    // A dispatch with a maximal grid over a SPIR-V (non-kernel) compute pipeline must return promptly: the
    // CPU oracle cannot run SPIR-V, so it records the dispatch and returns Ok without iterating u32::MAX^3
    // threads. This proves an adversarial grid neither hangs nor panics.
    let mut exec = CpuExecutor::new();
    let mut res = SessionResources::new();
    exec.execute(
        &mut res,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::SpirV,
                spirv: vec![0x0723_0203],
            },
            Cmd::CreateComputePipeline(
                1,
                ComputePipelineDesc {
                    compute: ShaderRef {
                        module: 1,
                        entry: "c".into(),
                    },
                    label: String::new(),
                },
            ),
            Cmd::CreateBuffer(1, buf(16, buffer_usage::STORAGE)),
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
        ],
    )
    .unwrap();
    exec.execute(
        &mut res,
        &[submit(vec![
            Enc::BeginComputePass,
            Enc::SetPipeline(1),
            Enc::SetBindGroup { index: 0, group: 1 },
            Enc::Dispatch {
                x: u32::MAX,
                y: u32::MAX,
                z: u32::MAX,
            },
            Enc::EndComputePass,
        ])],
    )
    .expect("huge grid over a SPIR-V pipeline returns cleanly (no kernel to run)");
}
