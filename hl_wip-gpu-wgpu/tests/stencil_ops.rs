//! EXHAUSTIVE stencil-operation coverage: every one of the 8 protocol stencil ops (KEEP..DECREMENT_WRAP),
//! including the CLAMP-vs-WRAP boundary cases, driven through a real two-pass draw on the `WgpuExecutor` and
//! verified to leave EXACTLY the stencil value the shared oracle predicate `stencil_op::apply` computes.
//!
//! `pipeline::stencil_operation` has an arm per op code, but before this only REPLACE (implicitly, via the
//! differential stencil programs) was exercised. Method, in one command buffer over one depth+stencil
//! attachment:
//!   pass 1 (write): stencil compare ALWAYS + `pass_op = OP`, reference = `ref`, over a plane pre-cleared to
//!     stencil `stored`; so every covered texel's stencil becomes `apply(OP, stored, ref)`.
//!   pass 2 (verify): stencil compare EQUAL with reference = the oracle's expected value + `pass_op = KEEP`;
//!     it paints GREEN only where the stored stencil equals the expectation. A green center pixel proves the
//!     op wrote EXACTLY the oracle value; a negative control (EQUAL vs expected^0xff) must NOT paint.
//! Skips with no adapter.

use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, ColorTargetState, DepthAttachment, DepthState, RenderPipelineDesc, ShaderRef,
    StencilFaceState, TextureDesc,
};
use hl_gpu::protocol::model::enums::{
    compare, stencil_op, texture_usage, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
use hl_gpu::{
    Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind,
};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const W: u32 = 4;
const H: u32 = 4;

const VS: &str = r#"#version 460
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    gl_Position = vec4(p[gl_VertexIndex], 0.0, 1.0);
}
"#;

fn fs_const(rgba: [f32; 4]) -> String {
    format!(
        "#version 460\nlayout(location=0) out vec4 o;\nvoid main() {{ o = vec4({:?}, {:?}, {:?}, {:?}); }}\n",
        rgba[0], rgba[1], rgba[2], rgba[3]
    )
}

fn glsl(stage: u32, source: &str) -> Vec<u32> {
    GlslDescriptor { stage, entry: "main".to_string(), source: source.to_string() }.to_words()
}

fn ds_tex() -> TextureDesc {
    TextureDesc {
        width: W, height: H, depth: 1, mip_levels: 1, sample_count: 1,
        dim: TextureDim::D2, format: TextureFormat::Depth24PlusStencil8,
        usage: texture_usage::RENDER_TARGET, label: String::new(),
    }
}

fn color_tex() -> TextureDesc {
    TextureDesc {
        width: W, height: H, depth: 1, mip_levels: 1, sample_count: 1,
        dim: TextureDim::D2, format: TextureFormat::Rgba8Unorm,
        usage: texture_usage::RENDER_TARGET | texture_usage::COPY_SRC, label: String::new(),
    }
}

fn ds_state(compare_op: u32, pass_op: u32) -> DepthState {
    let face = StencilFaceState { compare: compare_op, fail_op: stencil_op::KEEP, depth_fail_op: stencil_op::KEEP, pass_op };
    DepthState {
        format: TextureFormat::Depth24PlusStencil8,
        depth_write: false,
        depth_compare: compare::ALWAYS,
        stencil_front: face,
        stencil_back: face,
        stencil_read_mask: 0xff,
        stencil_write_mask: 0xff,
    }
}

fn pipe(id: u32, fs_module: u32, state: DepthState) -> Cmd {
    Cmd::CreateRenderPipeline(
        id,
        RenderPipelineDesc {
            vertex: ShaderRef { module: 1, entry: "main".into() },
            fragment: Some(ShaderRef { module: fs_module, entry: "main".into() }),
            vertex_buffers: vec![],
            color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
            depth: Some(state),
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
        },
    )
}

fn session(exec: &WgpuExecutor) -> Session {
    let caps = exec.capabilities();
    let mut limits = Limits::from_capabilities(caps);
    limits.copy_alignment = 1;
    Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)))
}

/// Run the write-then-verify two-pass for `op` with `stored`/`reference`, verifying against `verify_ref`.
/// Returns the center pixel after the verify pass (green ⇒ stencil matched `verify_ref`).
fn run(exec: &mut WgpuExecutor, op: u32, stored: u8, reference: u32, verify_ref: u32) -> [u8; 4] {
    let mut s = session(exec);
    hl_gpu::runtime::submit(
        &mut s,
        exec,
        0,
        &[
            Cmd::CreateTexture(1, color_tex()),
            Cmd::CreateTexture(2, ds_tex()),
            Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, VS) },
            Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, &fs_const([1.0, 0.0, 0.0, 1.0])) }, // red (pass 1)
            Cmd::CreateShader { id: 3, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, &fs_const([0.0, 1.0, 0.0, 1.0])) }, // green (pass 2)
            pipe(1, 2, ds_state(compare::ALWAYS, op)),          // write pipeline: pass_op = OP under test
            pipe(2, 3, ds_state(compare::EQUAL, stencil_op::KEEP)), // verify pipeline: EQUAL, no write
            Cmd::Submit(CommandBuffer {
                encoder: vec![
                    // pass 1 — write the stencil via `op`
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: Some(DepthAttachment { texture: 2, load: LoadOp::Clear, clear_depth: 1.0, clear_stencil: stored as u32 }),
                    },
                    Enc::SetPipeline(1),
                    Enc::SetStencilReference { reference },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                    // pass 2 — verify the written stencil equals `verify_ref`
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Load, clear: [0.0; 4], store: true }],
                        depth: Some(DepthAttachment { texture: 2, load: LoadOp::Load, clear_depth: 0.0, clear_stencil: 0 }),
                    },
                    Enc::SetPipeline(2),
                    Enc::SetStencilReference { reference: verify_ref },
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            }),
        ],
    )
    .expect("the two-pass stencil write+verify must run cleanly");
    let px = exec.read_texture(&s.resources, 1).unwrap();
    let c = ((H / 2 * W + W / 2) * 4) as usize;
    [px[c], px[c + 1], px[c + 2], px[c + 3]]
}

fn is_green(p: [u8; 4]) -> bool {
    p[1] > 200 && p[0] < 50
}

#[test]
fn every_stencil_op_writes_exactly_the_oracle_value() {
    let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
        Ok(e) => e,
        Err(_) => return,
    };

    // (op, stored, reference) — covering all 8 ops plus the CLAMP/WRAP boundary cases that distinguish
    // INCREMENT_CLAMP/WRAP (at 0xFF) and DECREMENT_CLAMP/WRAP (at 0x00).
    let cases: &[(u32, u8, u32)] = &[
        (stencil_op::KEEP, 0x10, 0x03),
        (stencil_op::ZERO, 0x10, 0x03),
        (stencil_op::REPLACE, 0x10, 0x03),
        (stencil_op::INVERT, 0x10, 0x03),
        (stencil_op::INCREMENT_CLAMP, 0x10, 0x03),
        (stencil_op::INCREMENT_CLAMP, 0xFF, 0x03), // clamp boundary → stays 0xFF
        (stencil_op::INCREMENT_WRAP, 0x10, 0x03),
        (stencil_op::INCREMENT_WRAP, 0xFF, 0x03), // wrap boundary → 0x00
        (stencil_op::DECREMENT_CLAMP, 0x10, 0x03),
        (stencil_op::DECREMENT_CLAMP, 0x00, 0x03), // clamp boundary → stays 0x00
        (stencil_op::DECREMENT_WRAP, 0x10, 0x03),
        (stencil_op::DECREMENT_WRAP, 0x00, 0x03), // wrap boundary → 0xFF
    ];

    for &(op, stored, reference) in cases {
        let expected = stencil_op::apply(op, stored, reference as u8) as u32;
        // Positive: EQUAL against the oracle's expected value paints green.
        let got = run(&mut exec, op, stored, reference, expected);
        assert!(
            is_green(got),
            "stencil op {op} (stored={stored:#x}, ref={reference:#x}): stored value must equal oracle \
             apply()={expected:#x}, but the EQUAL-verify pass did not paint green (got {got:?})"
        );
        // Negative control: EQUAL against a DIFFERENT value must NOT paint green (stays red), proving the
        // stencil holds the specific expected value, not just "some value".
        let miss = run(&mut exec, op, stored, reference, expected ^ 0xff);
        assert!(
            !is_green(miss),
            "stencil op {op}: EQUAL against a wrong reference must not match (stored is exactly {expected:#x}), got {miss:?}"
        );
    }
}
