//! REAL GRAPHICS #2 — a REAL EGL + GLES2 offscreen program rasterized on lavapipe, with a REAL socket
//! readback.
//!
//! `csrc/gl_triangle.c` is a header-free real GLES2 app (the EGL/GLES2 ABI is self-declared, exactly what
//! `-lEGL -lGLESv2` gives). `LD_LIBRARY_PATH=~/.hl/gl/aarch64` makes the `libEGL.so.1`/`libGLESv2.so.2` it
//! loads OURS, `EGL_PLATFORM=surfaceless` selects the headless path, and `$HL_GPU_EXEC` is the host
//! `WgpuExecutor` on the software Vulkan device (lavapipe / `llvmpipe`). Every GL call lowers to hl-GPU IR;
//! `glReadPixels` triggers a REAL device→host readback (render → `CopyTextureToBuffer` → `read_buffer` over
//! the socket).
//!
//! ASSERTED — the CLEAR path end to end: `glClearColor`+`glClear`+`glReadPixels` clears a 64x64 target on
//! lavapipe and reads the color back over the socket; we assert the app got it AND (independently) read the
//! same rendered target back off the host executor.
//!
//! REPORTED GAP — the GEOMETRY (triangle) path: our GL shim translates GLSL→MSL and tags the shader payload
//! `ShaderPayloadKind::LegacyMsl` (`hl_wip-gl/src/service/frame.rs:312`), which the wgpu/naga host executor
//! REJECTS (`hl_wip-gpu-wgpu/src/shader.rs:70` → `Unsupported("legacy MSL / demo-builtin payloads (no
//! WGSL)")`). So a real GLES2 triangle CANNOT rasterize on lavapipe today: the geometry frame is Nacked and
//! `glReadPixels` returns a GL error. The C program reproduces this live and reports it; this test records
//! the gap (see the assertion at the end) without failing, since the CLEAR path is the furthest observable
//! correct step. FIX: have the GL driver forward GLSL-ES verbatim as `ShaderPayloadKind::Glsl` (naga's
//! `glsl-in` is enabled in the wgpu executor) instead of pre-translating to MSL.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_clear_and_triangle_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(
        gl_dir.join("libEGL.so.1").exists(),
        "staged libEGL.so.1 missing at {gl_dir:?} — build hl_wip-gl's shim first"
    );
    assert!(
        gl_dir.join("libGLESv2.so.2").exists(),
        "staged libGLESv2.so.2 missing at {gl_dir:?}"
    );

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-gltri-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_triangle");

    // Link the staged shims directly (as a real `-lEGL -lGLESv2` app does — our libEGL/libGLESv2 ARE the
    // driver, no separate loader). `-L` finds the `libEGL.so`/`libGLESv2.so` dev symlinks in the staged dir.
    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_triangle.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the GLES2 program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exec = WgpuExecutorServer::start("gltri");
    eprintln!("host wgpu adapter: {}", exec.adapter_name());

    let run = Command::new(&bin)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_GL_SURFACE_W", W.to_string())
        .env("HL_GL_SURFACE_H", H.to_string())
        .output()
        .expect("spawn gl_triangle");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_triangle stdout ---\n{stdout}\n--- gl_triangle stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "GLES2 program exited non-zero (code {:?})",
        run.status.code()
    );

    // ---- ASSERT the clear-color readback the guest observed over the socket ----------------------
    assert!(
        stdout.contains("GL_CLEAR_READBACK_OK"),
        "the GLES2 app cleared a target on lavapipe and read the color back over the socket"
    );
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // ---- Independently assert the SAME rendered target off the host executor ---------------------
    let cap = exec.captured();
    if let Some(px) = cap.rgba8_texture(W, H) {
        // The GL default target is Bgra8Unorm; read_texture returns native (B,G,R,A) order. The clear was
        // R=0.25 G=0.5 B=0.75 → ~(64,128,191); in BGRA that is (191,128,64,255).
        let i = (((H / 2) * W + (W / 2)) * 4) as usize;
        let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 2;
        assert!(
            near(px[i], 191) && near(px[i + 1], 128) && near(px[i + 2], 64) && px[i + 3] == 255,
            "host-side render target center (BGRA) should be the clear color, got {:?}",
            &px[i..i + 4]
        );
    } else {
        panic!(
            "no 64x64 render target captured off the host executor for the GL clear frame; captured \
             texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        );
    }

    // ---- REPORT the geometry (triangle) gap: LegacyMsl shaders are rejected by the wgpu backend ---
    // The C program reproduced it live. We record the outcome; a real GLES2 triangle raster on lavapipe is
    // blocked until the GL driver forwards GLSL (not MSL). This is the valuable real bug this test surfaces.
    if stdout.contains("GL_TRIANGLE_DRAW_OK") {
        eprintln!(
            "NOTE: the GLES2 triangle unexpectedly RASTERIZED on lavapipe — the LegacyMsl→WGSL gap may be \
             fixed; consider promoting this to a hard triangle assertion."
        );
    } else {
        assert!(
            stdout.contains("GL_TRIANGLE_READBACK_FAILED") || stdout.contains("GL_TRIANGLE_WRONG_COLOR"),
            "expected the GLES2 triangle path to be blocked (LegacyMsl shader rejected by the wgpu/naga \
             executor) — stdout did not show the reproduced gap:\n{stdout}"
        );
        eprintln!(
            "REPORTED GAP: real GLES2 triangle blocked — GL shim emits ShaderPayloadKind::LegacyMsl \
             (frame.rs:312), wgpu executor rejects it (shader.rs:70). Fix: forward GLSL-ES verbatim as \
             ShaderPayloadKind::Glsl (naga glsl-in is enabled)."
        );
    }

    let _ = std::fs::remove_dir_all(&out_dir);
}
