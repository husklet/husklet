//! Out-of-range depth/stencil codes are REFUSED, not absorbed into a permissive answer.
//!
//! `DepthState::depth_compare` and the stencil face states' `compare` / `fail_op` / `depth_fail_op` /
//! `pass_op` are opaque `u32` codes, and until now nothing validated any of them. Both executors then
//! folded an unrecognised value into the most permissive answer available: the CPU oracle's
//! `compare::passes` returned `true`, and the wgpu path mapped it to `wgpu::CompareFunction::Always`. A
//! guest asking for a depth test the backend could not model therefore got NO depth test, and success.
//!
//! Both fallbacks carried a comment presenting that as deliberate — "so an honest bring-up never
//! hard-fails a draw on a code it does not model". It was not a policy, it was a claim about a path
//! nothing prevented from being taken; the sampler's compare, using the SAME constants, had been
//! range-checked in this very function all along. The comments now describe what is enforced, and this is
//! what enforces it.
//!
//! The refusals below are paired with an exhaustive POSITIVE CONTROL. A battery that only asserts
//! rejections is satisfied just as well by rejecting everything, which would rebuild the same defect one
//! layer up — a legal code and an illegal one indistinguishable again.

use hl_gpu::protocol::model::descriptor::{
    DepthState, RenderPipelineDesc, ShaderRef, StencilFaceState,
};
use hl_gpu::protocol::model::enums::{compare, stencil_op, TextureFormat, Topology};
use hl_gpu::runtime::service::validate::validate;
use hl_gpu::{Cmd, CpuExecutor, GpuExecutor, Limits};

fn limits() -> Limits {
    Limits::from_capabilities(CpuExecutor::new().capabilities())
}

fn pipeline(depth: DepthState) -> Vec<Cmd> {
    vec![Cmd::CreateRenderPipeline(
        1,
        RenderPipelineDesc {
            vertex: ShaderRef {
                module: 1,
                entry: "vmain".into(),
            },
            fragment: None,
            vertex_buffers: vec![],
            color_targets: vec![],
            depth: Some(depth),
            topology: Topology::TriangleList,
            cull: 0,
            front_face: 0,
            sample_count: 1,
            label: String::new(),
        },
    )]
}

fn depth_with(depth_compare: u32) -> DepthState {
    DepthState::depth_only(TextureFormat::Depth32Float, true, depth_compare)
}

fn depth_with_face(face: StencilFaceState) -> DepthState {
    let mut state = depth_with(compare::LESS);
    state.stencil_front = face;
    state
}

/// THE POSITIVE CONTROL: every code the protocol defines must still be accepted.
#[test]
fn every_defined_depth_and_stencil_code_is_accepted() {
    let limits = limits();
    for code in compare::NEVER..=compare::ALWAYS {
        validate(&limits, 0, &pipeline(depth_with(code)))
            .unwrap_or_else(|e| panic!("depth_compare={code} is defined and must be accepted: {e}"));

        let face = StencilFaceState {
            compare: code,
            ..StencilFaceState::DISABLED
        };
        validate(&limits, 0, &pipeline(depth_with_face(face)))
            .unwrap_or_else(|e| panic!("stencil compare={code} is defined and must be accepted: {e}"));
    }
    for op in stencil_op::KEEP..=stencil_op::DECREMENT_WRAP {
        for face in [
            StencilFaceState {
                fail_op: op,
                ..StencilFaceState::DISABLED
            },
            StencilFaceState {
                depth_fail_op: op,
                ..StencilFaceState::DISABLED
            },
            StencilFaceState {
                pass_op: op,
                ..StencilFaceState::DISABLED
            },
        ] {
            validate(&limits, 0, &pipeline(depth_with_face(face)))
                .unwrap_or_else(|e| panic!("stencil op={op} is defined and must be accepted: {e}"));
        }
    }
}

#[test]
fn an_out_of_range_depth_compare_is_refused_rather_than_disabling_the_depth_test() {
    let limits = limits();
    for code in [compare::ALWAYS + 1, 99, u32::MAX] {
        assert!(
            validate(&limits, 0, &pipeline(depth_with(code))).is_err(),
            "depth_compare={code} is undefined and must be refused — absorbing it into ALWAYS turns the \
             depth test the guest asked for into no depth test at all, and reports success"
        );
    }
}

#[test]
fn an_out_of_range_stencil_compare_or_op_is_refused() {
    let limits = limits();
    let bad_compare = StencilFaceState {
        compare: compare::ALWAYS + 1,
        ..StencilFaceState::DISABLED
    };
    assert!(
        validate(&limits, 0, &pipeline(depth_with_face(bad_compare))).is_err(),
        "an undefined stencil compare must be refused, not treated as always-pass"
    );

    for face in [
        StencilFaceState {
            fail_op: stencil_op::DECREMENT_WRAP + 1,
            ..StencilFaceState::DISABLED
        },
        StencilFaceState {
            depth_fail_op: 42,
            ..StencilFaceState::DISABLED
        },
        StencilFaceState {
            pass_op: u32::MAX,
            ..StencilFaceState::DISABLED
        },
    ] {
        assert!(
            validate(&limits, 0, &pipeline(depth_with_face(face))).is_err(),
            "an undefined stencil op must be refused, not silently KEEP"
        );
    }

    // The BACK face is checked too — a per-face oversight would leave half the state unguarded.
    let mut back_bad = depth_with(compare::LESS);
    back_bad.stencil_back = StencilFaceState {
        pass_op: 99,
        ..StencilFaceState::DISABLED
    };
    assert!(
        validate(&limits, 0, &pipeline(back_bad)).is_err(),
        "the back face must be validated as well as the front"
    );
}
