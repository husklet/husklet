//! Every translation pass that encodes a rule of the TARGET language must run on both dialect routes.
//!
//! `glsl_es::Source::normalize` exists for GLSL-ES source syntax, and it is skipped when `is_es()` is
//! false. The GL driver rewrites its shaders to desktop form before they arrive, so its output takes the
//! other route — and any pass parked inside `normalize` that is really about what naga or WGSL can accept
//! silently stopped applying to the driver's own shaders. That has now produced defects three times:
//! two-row matrices in std140, matrix varyings, and the two below. The rule that separates them is simple:
//!
//!   A pass encoding an interface, layout or type rule of the TARGET belongs on both routes.
//!   A pass about GLSL-ES SOURCE SYNTAX belongs behind the dialect check.
//!
//! Each case here drives the SAME shader in both spellings. Both must compile: an ES-only pass shows up as
//! the desktop spelling failing while the ES one succeeds, which is exactly the signature all four defects
//! had, and which presents to a user as a shader that mysteriously fails to translate — and therefore as a
//! region of a window that never gets drawn.

mod gpu_harness;
use gpu_harness::{glsl, new_session};

use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

/// Compile a fragment shader through the real executor path.
fn compile(exec: &mut WgpuExecutor, source: &str) -> hl_gpu::Result<()> {
    let mut session = new_session(exec);
    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(glsl_stage::FRAGMENT, "fmain", source),
        }],
    )
    .map(|_| ())
}

/// The same shader body in both spellings: ES (which takes `normalize`) and desktop (which does not).
fn both_routes(body: &str) -> [(&'static str, String); 2] {
    [
        ("es", format!("#version 300 es\nprecision highp float;\n{body}")),
        ("desktop", format!("#version 460\n{body}")),
    ]
}

fn assert_both(name: &str, body: &str) {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");
    for (dialect, source) in both_routes(body) {
        compile(&mut exec, &source).unwrap_or_else(|e| {
            panic!(
                "{name} ({dialect}): must compile on BOTH routes — a pass encoding a target-language \
                 rule is sitting behind the dialect check: {e}"
            )
        });
    }
}

/// A `switch` whose cases return. `switch` is valid GLSL in both dialects; what cannot take it is the
/// TARGET — naga's `wgsl-out` refuses a fall-through case block — so the lowering is not a dialect
/// concern and must not be gated as one.
#[test]
fn a_returning_switch_compiles_on_both_routes() {
    assert_both(
        "returning switch",
        "float pick(int k){ switch(k){ case 0: return 1.0; case 1: return 0.5; default: return 0.0; } }\n\
         layout(location = 0) out vec4 o;\n\
         layout(std140, binding = 0) uniform HlUniforms { int k; };\n\
         void main(){ o = vec4(pick(k)); }\n",
    );
}

/// Dual-source blending. naga's `glsl-in` cannot PARSE `index =` in any dialect, so dropping it is a
/// front-end accommodation rather than an ES one.
#[test]
fn dual_source_outputs_compile_on_both_routes() {
    assert_both(
        "dual source",
        "layout(location = 0, index = 0) out vec4 a;\n\
         layout(location = 0, index = 1) out vec4 b;\n\
         void main(){ a = vec4(1.0); b = vec4(0.5); }\n",
    );
}

/// The two already fixed, kept here so the whole family is checked in one place and a regression in any of
/// them reads as the same defect rather than as four unrelated ones.
#[test]
fn the_previously_gated_passes_stay_on_both_routes() {
    assert_both(
        "two-row matrix in std140",
        "layout(std140, binding = 0) uniform HlUniforms { mat3x2 m; };\n\
         layout(location = 0) out vec4 o;\n\
         void main(){ o = vec4(m[0][0]); }\n",
    );
    assert_both(
        "matrix varying",
        "layout(location = 0) in mat3 v;\n\
         layout(location = 0) out vec4 o;\n\
         void main(){ o = vec4(v[0][0]); }\n",
    );
}
