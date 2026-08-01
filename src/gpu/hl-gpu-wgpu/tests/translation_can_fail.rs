//! Translation CAN fail, loudly — so "linked = true" is not evidence that it succeeded.
//!
//! A report: the literal string "this is not GLSL and cannot translate" compiled, linked, and produced
//! both vertex and fragment IR, with no error. Two readings, with very different consequences. Either the
//! front end accepts arbitrary text — in which case a malformed guest shader silently becomes *something*
//! and renders it — or linking does not gate on translation, in which case a shader that genuinely failed
//! still reports a linked program and the failure surfaces later as missing geometry.
//!
//! This settles the first reading for this layer, which is where translation actually happens: the front
//! end is NOT tolerant. Arbitrary text is refused, as a typed error, at `CreateShader` — before any
//! pipeline exists and before anything can be drawn with it. So a guest that was told its program linked
//! was told that by something which had not consulted this layer yet.

mod gpu_harness;
use gpu_harness::{glsl, new_session};

use hl_gpu::protocol::model::kernel::glsl_stage;
use hl_gpu::{Cmd, ShaderPayloadKind};
use hl_gpu_wgpu::{DeviceConfig, WgpuExecutor};

fn create(exec: &mut WgpuExecutor, stage: u32, source: &str) -> hl_gpu::Result<()> {
    let mut session = new_session(exec);
    hl_gpu::runtime::submit(
        &mut session,
        exec,
        0,
        &[Cmd::CreateShader {
            id: 1,
            kind: ShaderPayloadKind::Glsl,
            spirv: glsl(stage, "main", source),
        }],
    )
    .map(|_| ())
}

/// The exact string from the report, plus other things that cannot be shaders.
#[test]
fn text_that_is_not_a_shader_is_refused_by_the_front_end() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    for source in [
        "this is not GLSL and cannot translate",
        "",
        "\0\u{1}\u{2} binary garbage \u{7f}",
        "#version 460\nvoid main() { this_function_does_not_exist(); }\n",
        "#version 460\nvoid main() { vec4 v = ; }\n",
        // Syntactically fine, semantically impossible: assigning a matrix to a scalar.
        "#version 460\nlayout(location=0) out vec4 o;\nvoid main() { float f = mat4(1.0); o = vec4(f); }\n",
    ] {
        for stage in [glsl_stage::VERTEX, glsl_stage::FRAGMENT] {
            let outcome = create(&mut exec, stage, source);
            assert!(
                outcome.is_err(),
                "the front end must REFUSE text that cannot be a shader, stage {stage}, source {source:?} \
                 — if this passes, a malformed guest shader silently becomes something and renders it"
            );
        }
    }
}

/// THE POSITIVE CONTROL. A refusal battery proves nothing on its own: if translation refused everything,
/// every assertion above would pass while the driver was completely broken.
#[test]
fn an_ordinary_shader_still_translates() {
    let mut exec = WgpuExecutor::new(DeviceConfig::default())
        .expect("a GPU adapter is required to prove the wgpu executor");

    create(
        &mut exec,
        glsl_stage::VERTEX,
        "#version 460\nvoid main() { gl_Position = vec4(0.0, 0.0, 0.0, 1.0); }\n",
    )
    .expect("a valid vertex shader must translate");
    create(
        &mut exec,
        glsl_stage::FRAGMENT,
        "#version 460\nlayout(location = 0) out vec4 o;\nvoid main() { o = vec4(1.0); }\n",
    )
    .expect("a valid fragment shader must translate");
}
