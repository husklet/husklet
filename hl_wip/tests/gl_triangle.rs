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
//! ASSERTED — both the CLEAR and the GEOMETRY path end to end: `glClearColor`+`glClear` clears a 64x64
//! target on lavapipe, then a real GLES2 vertex+fragment program draws a GREEN triangle over it and
//! `glReadPixels` reads it back over the socket. We assert the guest saw the clear AND (independently) read
//! the same RASTERIZED target back off the host executor (center = triangle, corner = clear color).
//!
//! This used to report a GAP: the GL shim pre-translated GLSL→MSL and tagged the payload
//! `ShaderPayloadKind::LegacyMsl`, which the wgpu/naga executor rejected — so geometry never rasterized.
//! The driver now forwards GLSL-ES verbatim as `ShaderPayloadKind::Glsl` (two modules, vertex + fragment)
//! and the executor compiles it via naga's `glsl-in`, so the triangle genuinely rasterizes. See
//! `gl_geometry.rs` for the dedicated geometry milestone assertion.

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

    // ---- ASSERT the geometry (triangle) path now RASTERIZES — the fixed behavior ------------------
    // The GL driver forwards GLSL-ES verbatim as ShaderPayloadKind::Glsl and the wgpu/naga executor
    // compiles it, so the real GLES2 triangle rasterizes on lavapipe (was the LegacyMsl gap this test used
    // to report). The scene clears R=0.25 G=0.5 B=0.75 then draws a GREEN triangle covering the center.
    assert!(
        stdout.contains("GL_TRIANGLE_DRAW_OK"),
        "the GLES2 triangle should rasterize on lavapipe (GLSL forwarded to naga), stdout:\n{stdout}"
    );

    // ---- Independently assert the SAME rasterized target off the host executor -------------------
    let cap = exec.captured();
    let Some(px) = cap.rgba8_texture(W, H) else {
        panic!(
            "no 64x64 render target captured off the host executor for the GL frame; captured \
             texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    };
    // The GL default target is Bgra8Unorm; read_texture returns native (B,G,R,A) order. Center is covered
    // by the GREEN triangle (0,255,0); a corner keeps the clear color (BGRA 191,128,64).
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 2;
    let c = (((H / 2) * W + (W / 2)) * 4) as usize;
    assert!(
        near(px[c], 0) && near(px[c + 1], 255) && near(px[c + 2], 0) && px[c + 3] == 255,
        "host-side render target center (BGRA) should be the GREEN triangle, got {:?}",
        &px[c..c + 4]
    );
    assert!(
        near(px[0], 191) && near(px[1], 128) && near(px[2], 64) && px[3] == 255,
        "host-side render target corner (BGRA) should be the clear color, got {:?}",
        &px[0..4]
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
