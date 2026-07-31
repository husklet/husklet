//! DEFECT RECORD — `isinf`/`isnan` in a SPIR-V payload are still refused.
//!
//! The GLSL route lowers both predicates to integer tests on the IEEE-754 bit pattern before naga parses
//! (`glsl_es::Source::rewrite_nonfinite_predicates`), because naga's `wgsl-out` emits no relational
//! function except `all`/`any` — WGSL has no `isNan`/`isInf` at all, gpuweb having removed them
//! (gpuweb#2311). That rewrite is TEXTUAL, so it reaches GLSL payloads only.
//!
//! A precompiled-shader guest sends SPIR-V. `OpIsInf`/`OpIsNan` survive `spv-in` as
//! `RelationalFunction::IsInf`/`IsNan` and hit the same `wgsl-out` wall, so the shader is refused. Same
//! defect, different payload kind — and the one route a textual source rewrite can never cover.
//!
//! This asserts the CURRENT REFUSAL, deliberately, following the `Expect::NagaLimit` precedent in
//! `glsl_es_corpus`: a limit that is on the record with its exact reason cannot be mistaken for working,
//! and cannot be faked into a green. It is NOT a statement that the refusal is correct — it is not. When
//! the IR-level lowering lands, this test flips to the fail-before for it, and the value assertions it
//! should be replaced by are the ones in `nonfinite_predicates.rs`.

use hl_gpu::{Cmd, FakeClock, GlobalLedger, GpuExecutor, Limits, Session, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// A fragment shader using both predicates, minted to REAL SPIR-V so the payload is what a precompiled
/// guest would actually send (`glsl-in` → validate → `spv-out`).
const FS: &str = r#"#version 460
layout(std140, binding = 0) uniform U { vec4 v; };
layout(location = 0) out vec4 o;
void main() { o = vec4(isinf(v.x) ? 1.0 : 0.0, isnan(v.y) ? 1.0 : 0.0, 0.0, 1.0); }
"#;

fn spirv_with_nonfinite_predicates() -> Vec<u32> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(
            &naga::front::glsl::Options::from(naga::ShaderStage::Fragment),
            FS,
        )
        .expect("glsl-in accepts isinf/isnan — the gap is wgsl-out, not the front end");
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .expect("the module validates — the predicates are legal naga IR");
    naga::back::spv::write_vec(&module, &info, &naga::back::spv::Options::default(), None)
        .expect("spv-out emits OpIsInf/OpIsNan")
}

#[test]
fn spirv_payloads_using_isinf_or_isnan_are_still_refused() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    let capabilities = exec.capabilities();
    let mut session = Session::new(
        Limits::from_capabilities(capabilities),
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );

    let refused = hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: spirv_with_nonfinite_predicates(),
        }],
    );
    let error = refused.expect_err(
        "KNOWN LIMIT: a SPIR-V payload using isinf/isnan is refused. If this now SUCCEEDS, the IR-level \
         lowering has landed — delete this record and assert the VALUES instead (nonfinite_predicates.rs)",
    );
    assert!(
        error.to_string().contains("Unsupported relational function"),
        "the refusal must still be the wgsl-out relational gap, not some unrelated failure: {error}"
    );

    // The refusal is a clean NACK, not a panic, and the session survives it — that much already holds.
    assert!(
        !matches!(error, hl_gpu::GpuError::Panicked(_)),
        "the limit must surface as a refusal, never as a backend panic"
    );
    hl_gpu::runtime::submit(
        &mut session,
        &mut exec,
        0,
        &[Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::SpirV,
            spirv: spirv_with_nonfinite_predicates(),
        }],
    )
    .expect_err("and it is refused consistently, leaving id 1 free to retry");
}
