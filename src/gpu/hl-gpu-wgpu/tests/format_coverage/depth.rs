use super::*;

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// VS emitting a fullscreen triangle at a baked constant clip-space `z` (w=1, so `gl_Position.z` is the
/// per-draw depth). FS emits a baked constant color. Distinct `z`/color per draw lets one pass prove occlusion.
fn depth_vs(z: f32) -> Vec<u32> {
    let src = format!(
        "#version 460\nvoid main() {{\n  vec2 p[3] = vec2[3](vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0));\n  gl_Position = vec4(p[gl_VertexIndex], {z:?}, 1.0);\n}}\n"
    );
    glsl(glsl_stage::VERTEX, "vmain", &src)
}
fn depth_fs(c: [u8; 4]) -> Vec<u32> {
    let src = format!(
        "#version 460\nlayout(location=0) out vec4 o;\nvoid main() {{ o = vec4({:?}, {:?}, {:?}, 1.0); }}\n",
        c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0
    );
    glsl(glsl_stage::FRAGMENT, "fmain", &src)
}

/// Run three fullscreen draws (green z=0.5, blue z=0.2, red z=0.8, in that order) through `ds_fmt` as the
/// depth attachment with the given depth `cmp`, and return the single readback pixel of the 1×1 color target.
fn depth_run(exec: &mut WgpuExecutor, ds_fmt: TextureFormat, cmp: u32) -> [u8; 4] {
    let mut s = new_session(exec);
    let ds = DepthState::depth_only(ds_fmt, /*depth_write*/ true, cmp);
    let pipe = |module_vs: u32, module_fs: u32| RenderPipelineDesc {
        vertex: ShaderRef {
            module: module_vs,
            entry: "vmain".into(),
        },
        fragment: Some(ShaderRef {
            module: module_fs,
            entry: "fmain".into(),
        }),
        vertex_buffers: vec![],
        color_targets: vec![ct(TextureFormat::Rgba8Unorm)],
        depth: Some(ds.clone()),
        topology: Topology::TriangleList,
        cull: 0,
        front_face: 0,
        sample_count: 1,
        label: String::new(),
    };
    let draw = |pipe: u32| {
        vec![
            Enc::SetPipeline(pipe),
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
        ]
    };
    let mut enc = vec![Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: 1,
            load: LoadOp::Clear,
            clear: [0.0, 0.0, 0.0, 1.0],
            store: true,
        }],
        depth: Some(DepthAttachment {
            texture: 2,
            load: LoadOp::Clear,
            clear_depth: 1.0,
            clear_stencil: 0,
        }),
    }];
    enc.extend(draw(1)); // green z=0.5
    enc.extend(draw(2)); // blue  z=0.2 (nearest)
    enc.extend(draw(3)); // red   z=0.8 (farthest, drawn last)
    enc.push(Enc::EndRenderPass);

    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    1,
                    1,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
            Cmd::CreateTexture(2, tex(1, 1, ds_fmt, texture_usage::RENDER_TARGET)),
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_vs(0.5),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_fs(GREEN),
            },
            Cmd::CreateShader {
                id: 3,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_vs(0.2),
            },
            Cmd::CreateShader {
                id: 4,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_fs(BLUE),
            },
            Cmd::CreateShader {
                id: 5,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_vs(0.8),
            },
            Cmd::CreateShader {
                id: 6,
                kind: ShaderPayloadKind::Glsl,
                spirv: depth_fs(RED),
            },
            Cmd::CreateRenderPipeline(1, pipe(1, 2)),
            Cmd::CreateRenderPipeline(2, pipe(3, 4)),
            Cmd::CreateRenderPipeline(3, pipe(5, 6)),
            Cmd::Submit(CommandBuffer {
                encoder: enc,
                signal: None,
            }),
        ],
    )
    .unwrap_or_else(|e| panic!("depth {ds_fmt:?} cmp={cmp}: submit must run cleanly, got {e:?}"));
    let img = exec.read_texture(&s.resources, 1).unwrap();
    [img[0], img[1], img[2], img[3]]
}

#[test]
fn depth_formats_nearest_occludes_regardless_of_draw_order() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    for &ds_fmt in &[
        TextureFormat::Depth32Float,
        TextureFormat::Depth24PlusStencil8,
    ] {
        // LESS test: nearest (blue, z=0.2) survives; red (z=0.8, drawn LAST) is rejected as farther.
        let with_test = depth_run(&mut exec, ds_fmt, compare::LESS);
        assert!(near_tol(with_test, BLUE, 2),
            "{ds_fmt:?} as depth attachment (LESS): nearest fragment (blue z=0.2) must occlude the farther \
             ones regardless of draw order, got {with_test:?}");

        // Control: force the test ALWAYS → the LAST-drawn fragment (red, the FARTHEST) wins, proving the
        // depth test — not draw order — produced the blue result above.
        let no_test = depth_run(&mut exec, ds_fmt, compare::ALWAYS);
        assert!(near_tol(no_test, RED, 2),
            "{ds_fmt:?} (ALWAYS): with the depth test disabled the last-drawn fragment (red) must win, got {no_test:?}");

        assert_ne!(with_test, no_test,
            "{ds_fmt:?}: the LESS result must differ from the ALWAYS result — proof the format's depth test gated the draw");
        eprintln!("depth {ds_fmt:?}: LESS→nearest(blue) occludes; ALWAYS→last(red) — depth attachment works");
    }
}

// ==========================================================================================================
// The executor must advertise EXACTLY the formats these tests prove it handles.
// ==========================================================================================================
