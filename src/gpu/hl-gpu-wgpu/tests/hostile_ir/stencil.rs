use super::*;

// =================================================================================================
// (7) STENCIL on a non-stencil target — SetStencilReference is a harmless no-op (clamp), not a crash
// =================================================================================================

#[test]
fn set_stencil_reference_without_stencil_is_harmless_noop() {
    let Some(mut g) = exec() else { return };
    // A plain color pipeline (no depth/stencil) drawn in a color-only pass, with a SetStencilReference in
    // the stream: the reference has no stencil to test against, so it is a defined no-op — the draw still
    // runs and paints the target white. Proves a stray stencil-state op neither errors spuriously nor panics.
    let mut s = session(&g);
    let mut cmds = vec![Cmd::CreateTexture(
        1,
        tex(2, 2, TextureFormat::Rgba8Unorm, RT),
    )];
    cmds.extend(white_triangle_pipeline(1, 1, 2));
    // A fullscreen triangle so the 2x2 target is fully covered.
    let vs = "#version 460\nvoid main(){ vec2 p[3] = vec2[3](vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0)); gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0); }\n";
    cmds[1] = Cmd::CreateShader {
        id: 1,
        kind: ShaderPayloadKind::Glsl,
        spirv: glsl(glsl_stage::VERTEX, "vmain", vs),
    };
    cmds.push(Cmd::Submit(CommandBuffer {
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
            Enc::SetStencilReference { reference: 0x7f }, // no stencil aspect -> harmless
            Enc::Draw {
                vertex_count: 3,
                instance_count: 1,
                first_vertex: 0,
                first_instance: 0,
            },
            Enc::EndRenderPass,
        ],
        signal: None,
    }));
    hl_gpu::runtime::submit(&mut s, &mut *g, 0, &cmds).expect(
        "SetStencilReference on a non-stencil target must be a harmless no-op, not an error/panic",
    );
    let px = g.read_texture(&s.resources, 1).unwrap();
    for (i, out) in px.chunks_exact(4).enumerate() {
        assert_eq!(
            out,
            [255, 255, 255, 255],
            "pixel {i}: the draw must still paint white despite the stray stencil-ref"
        );
    }
    drop(s);
    assert_survives(&mut g, "stray_stencil_ref");
}
