//! REAL SOFTWARE #1 — the prebuilt Khronos `eglinfo` utility queries OUR EGL.
//!
//! `eglinfo` is a real, separately-shipped diagnostic binary (Debian `mesa-utils-bin`). It links
//! `libEGL.so.1` at runtime; we override that resolution with `LD_LIBRARY_PATH=~/.hl/gl/aarch64` so the
//! `libEGL.so.1` it loads is OUR staged shim. It then calls `eglGetPlatformDisplay` / `eglInitialize` /
//! `eglQueryString` / `eglGetConfigs` and prints what our driver reports. We assert our identity strings
//! ("hl-gl", "OpenGL ES 3.0 hl-gl", our config table) appear — proving a real third-party program drives
//! our EGL implementation end to end. A host GPU executor is stood up on `$HL_GPU_EXEC` in case any EGL
//! entry point reaches the executor (eglinfo runs fine either way — see REALSOFTWARE.md).

use std::process::Command;

mod common;
use common::{staged_dir, Executor};

#[test]
fn real_eglinfo_reports_our_egl_identity() {
    let gl_dir = staged_dir("gl");
    assert!(
        gl_dir.join("libEGL.so.1").exists(),
        "staged libEGL.so.1 missing at {gl_dir:?} — build hl-gl's shim first"
    );

    let eglinfo = "/usr/bin/eglinfo";
    if !std::path::Path::new(eglinfo).exists() {
        eprintln!("SKIP: {eglinfo} not installed on this host");
        return;
    }

    // Host executor available on a temp socket (some EGL paths negotiate capabilities with it).
    let exec = Executor::start("eglinfo");

    let out = Command::new(eglinfo)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GPU_EXEC", exec.sock())
        .output()
        .expect("spawn eglinfo");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("--- eglinfo stdout ---\n{stdout}\n--- eglinfo stderr ---\n{stderr}");

    // The real binary talked to OUR EGL: vendor + version + renderer strings are ours.
    assert!(
        stdout.contains("EGL vendor string: hl-gl"),
        "eglinfo saw our EGL vendor 'hl-gl'"
    );
    assert!(stdout.contains("hl-gl"), "eglinfo saw our 'hl-gl' identity");
    assert!(
        stdout.contains("OpenGL ES 3.0 hl-gl"),
        "eglinfo saw our GLES 3.0 version string"
    );
    assert!(
        stdout.contains("hl-gl-metal"),
        "eglinfo saw our renderer string"
    );
    // It successfully enumerated at least one EGLConfig from our driver (the config table header).
    assert!(
        stdout.contains("Configurations:"),
        "eglinfo printed our EGLConfig table"
    );
}
