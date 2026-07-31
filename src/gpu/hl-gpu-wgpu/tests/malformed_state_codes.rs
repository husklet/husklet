//! Malformed fixed-function state codes are REFUSED, not absorbed into a valid state.
//!
//! `RenderPipelineDesc::cull` and `front_face` are raw `u32` codes rather than validated wire enums, and
//! both used to fold every unrecognised value into a legal answer — an unknown cull became "no culling",
//! an unknown winding became counter-clockwise. That made a malformed value and a deliberate default the
//! SAME observation, which is the defect family this session has been chasing: a conversion that cannot
//! answer produces a plausible one, and it flows on as data. A guest whose cull state failed to encode
//! would draw un-culled geometry and be told everything worked.
//!
//! The sibling `compare` code was range-checked all along, in two places. Two paths handling the same
//! class of mistake differently is what marks the lenient one as an oversight rather than a policy.
//!
//! NOTE the POSITIVE CONTROLS below, and why they are not decoration: a battery that only asserts
//! refusals passes just as happily when EVERYTHING is refused, so the bad input and the good one become
//! indistinguishable again — one layer up. Each refusal here is paired with the nearest legal value, which
//! must still build.

mod gpu_harness;
use gpu_harness::{color_target, glsl, new_session};

use hl_gpu::protocol::model::descriptor::{RenderPipelineDesc, ShaderRef};
use hl_gpu::protocol::model::enums::Topology;
use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

const VS: &str = r#"#version 460
void main() { gl_Position = vec4(0.0, 0.0, 0.0, 1.0); }
"#;
const FS: &str = r#"#version 460
layout(location = 0) out vec4 o;
void main() { o = vec4(1.0); }
"#;

/// Build a render pipeline with the given raw `cull` / `front_face` codes.
fn build(exec: &mut WgpuExecutor, cull: u32, front_face: u32) -> hl_gpu::Result<()> {
    let mut session = new_session(exec);
    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[
            Cmd::CreateShader {
                id: 1,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::VERTEX, "vmain", VS),
            },
            Cmd::CreateShader {
                id: 2,
                kind: ShaderPayloadKind::Glsl,
                spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS),
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
                    cull,
                    front_face,
                    sample_count: 1,
                    label: String::new(),
                },
            ),
        ],
    )
    .map(|_| ())
}

#[test]
fn every_legal_cull_and_winding_still_builds() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    // THE POSITIVE CONTROL for the refusals below: every defined code must still build, or a blanket
    // refusal would satisfy those assertions while breaking every real guest.
    for cull in 0..=2u32 {
        for front_face in 0..=1u32 {
            build(&mut exec, cull, front_face).unwrap_or_else(|e| {
                panic!("cull={cull} front_face={front_face} is legal and must build, got {e}")
            });
        }
    }
}

#[test]
fn an_unrecognised_cull_code_is_refused_rather_than_meaning_no_culling() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    for cull in [3u32, 7, u32::MAX] {
        let outcome = build(&mut exec, cull, 0);
        assert!(
            outcome.is_err(),
            "cull={cull} is not a defined code and must be refused, not silently treated as no culling"
        );
    }
}

#[test]
fn an_unrecognised_winding_code_is_refused_rather_than_meaning_counter_clockwise() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    for front_face in [2u32, 9, u32::MAX] {
        let outcome = build(&mut exec, 0, front_face);
        assert!(
            outcome.is_err(),
            "front_face={front_face} is not a defined code and must be refused, not silently CCW"
        );
    }
}
