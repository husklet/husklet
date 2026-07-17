//! REAL GL TRANSFORM FEEDBACK (DOCUMENTED-GAP demo) — a real EGL + GLES3 program that drives the full
//! transform-feedback lifecycle + reflection and HONESTLY documents that per-vertex varying CAPTURE into
//! the `GL_TRANSFORM_FEEDBACK_BUFFER` is not modeled by this deferred driver. What IS real and asserted
//! exactly: `glTransformFeedbackVaryings` → `glGetTransformFeedbackVarying` round-trips the varying name
//! "vValue" verbatim, and the begin/draw/end lifecycle raises no GL error. What is NOT modeled (the gap):
//! the driver has no CPU vertex-shader executor to evaluate varyings, so it does NOT write captured values
//! — rather than FAKE data, the demo pre-fills the TF buffer with a sentinel and asserts it is UNCHANGED.
//! See `hl-gl/src/model/es3.rs` (TransformFeedbacks) for the gap note. Needs no host executor.

use std::process::Command;

mod common;
use common::staged_dir;

#[test]
fn real_gles3_transform_feedback_reflects_but_does_not_fake_capture() {
    let gl_dir = staged_dir("gl");
    assert!(
        gl_dir.join("libEGL.so.1").exists(),
        "staged libEGL.so.1 missing at {gl_dir:?}"
    );
    assert!(
        gl_dir.join("libGLESv2.so.2").exists(),
        "staged libGLESv2.so.2 missing at {gl_dir:?}"
    );

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-gltf-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_transform_feedback");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_transform_feedback.c"
        ))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&bin)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GL_SURFACE_W", "64")
        .env("HL_GL_SURFACE_H", "64")
        .output()
        .expect("spawn gl_transform_feedback");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_transform_feedback stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_TF_DOCUMENTED_GAP_OK"),
        "the varying reflection must round-trip exactly, the lifecycle must be error-free, and NO fake \
         capture may appear in the TF buffer; got:\n{stdout}"
    );
    // The reflection is the real modeled part: the varying name comes back verbatim.
    assert!(
        stdout.contains("GL_TF_VARYING: \"vValue\""),
        "glGetTransformFeedbackVarying must reflect the name"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
