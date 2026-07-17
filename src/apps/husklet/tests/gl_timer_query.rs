//! REAL GL TIMER QUERY (HONEST-ERROR demo) — a real EGL + GLES3 program that attempts an
//! `EXT_disjoint_timer_query` `GL_TIME_ELAPSED_EXT` query and confirms the driver returns an HONEST
//! `GL_INVALID_ENUM` instead of faking a timestamp. Timer queries are NOT OpenGL ES 3.0 core (they belong
//! to the `EXT_disjoint_timer_query` extension), and `hl-gl` advertises GLES 3.0 core with an EMPTY
//! extension list, so `glBeginQuery(GL_TIME_ELAPSED_EXT, …)` is an invalid target — see
//! `hl-gl/src/service/es3.rs::is_query_target`. This demo documents that gap by asserting the honest
//! refusal (no monotonic-counter fake), matching the no-false-success audit. It needs no host executor
//! (nothing rasterizes) but drives the REAL staged shim end to end.

use std::process::Command;

mod common;
use common::staged_dir;

#[test]
fn real_gles3_timer_query_returns_honest_invalid_enum() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-gltimer-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_timer_query");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_timer_query.c"
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
        .expect("spawn gl_timer_query");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_timer_query stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_TIMER_HONEST_ERROR_OK"),
        "the driver must refuse GL_TIME_ELAPSED_EXT with GL_INVALID_ENUM (no faked timer), got:\n{stdout}"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
