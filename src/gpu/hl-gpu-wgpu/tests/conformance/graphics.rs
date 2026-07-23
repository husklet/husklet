use super::*;

// -------------------------------------------------------------------------------------------------
// graphics — a REAL SPIR-V vertex+fragment triangle rasterized on lavapipe (the CPU oracle cannot)
// -------------------------------------------------------------------------------------------------

/// Mint real SPIR-V (with both entry points) from a WGSL seed via naga (wgsl-in → spv-out) — the round
/// trip the guest's SPIR-V ABI relies on. The executor then translates it back (spv-in → wgsl-out) and
/// builds a real render pipeline, so the SPIR-V genuinely drives the rasterizer.
fn wgsl_to_spirv(src: &str) -> Vec<u32> {
    let module = naga::front::wgsl::parse_str(src).expect("seed wgsl parses");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("seed wgsl validates");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("emit spir-v")
}

#[test]
fn graphics_spirv_triangle_shades_pixels() {
    // A full-clip-space triangle (covers every pixel of the target) whose fragment shader outputs solid
    // green. Both entry points live in one SPIR-V module.
    let seed = r#"
        @vertex
        fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
            var p = array<vec2<f32>, 3>(
                vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
            return vec4<f32>(p[vi], 0.0, 1.0);
        }
        @fragment
        fn fs_main() -> @location(0) vec4<f32> {
            return vec4<f32>(0.0, 1.0, 0.0, 1.0);
        }
    "#;
    let spirv = wgsl_to_spirv(seed);

    let mut g = exec();
    let s = run_batch(
        &mut g,
        &[
            Cmd::CreateTexture(
                1,
                tex(
                    4,
                    4,
                    TextureFormat::Rgba8Unorm,
                    texture_usage::RENDER_TARGET | texture_usage::COPY_SRC,
                ),
            ),
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
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment {
                            texture: 1,
                            load: LoadOp::Clear,
                            clear: [1.0, 0.0, 0.0, 1.0], // red background — overwritten by the triangle
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
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    let green = [0u8, 255, 0, 255];
    for (i, texel) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            texel, green,
            "pixel {i} should be shaded green by the SPIR-V fragment shader"
        );
    }
}
