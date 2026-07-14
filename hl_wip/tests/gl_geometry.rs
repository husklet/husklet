//! REAL GL GEOMETRY — the milestone: a real EGL + GLES2 offscreen program whose vertex+fragment GLSL is
//! FORWARDED verbatim and compiled by naga (`glsl-in`) on the host, so a real triangle genuinely
//! RASTERIZES on lavapipe (software Vulkan) and is read back over the socket.
//!
//! `csrc/gl_geometry.c` is a header-free real GLES2 app (the EGL/GLES2 ABI is self-declared, exactly what
//! `-lEGL -lGLESv2` gives). `LD_LIBRARY_PATH=~/.hl/gl/<arch>` makes the `libEGL.so.1`/`libGLESv2.so.2` it
//! loads OURS, `EGL_PLATFORM=surfaceless` selects the headless path, and `$HL_GPU_EXEC` is the host
//! `WgpuExecutor` on the software Vulkan device (lavapipe / `llvmpipe`). The scene clears RED and draws a
//! GREEN triangle covering the center.
//!
//! This is the milestone the GLSL-forwarding change exists for. Before it, the GL shim pre-translated
//! GLSL→MSL and tagged the payload `ShaderPayloadKind::LegacyMsl`, which the wgpu/naga executor REJECTED —
//! so geometry never rasterized (only the clear did). Now the shim forwards GLSL-ES as
//! `ShaderPayloadKind::Glsl` (two modules, vertex + fragment); `hl_wip-gpu-wgpu` compiles it via naga's
//! `glsl-in` → WGSL → a real wgpu render pipeline. We assert a COVERED interior pixel is the geometry color
//! and an uncovered corner is the clear color — from BOTH the guest's own `glReadPixels` and an independent
//! host-side read of the same rasterized target.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_triangle_rasterizes_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-glgeom-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_geometry");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_geometry.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the GLES2 geometry program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exec = WgpuExecutorServer::start("glgeom");
    let adapter = exec.adapter_name();
    eprintln!("host wgpu adapter: {adapter}");
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "the geometry must rasterize on the software Vulkan device, got adapter {adapter:?}"
    );

    let run = Command::new(&bin)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_GL_SURFACE_W", W.to_string())
        .env("HL_GL_SURFACE_H", H.to_string())
        .output()
        .expect("spawn gl_geometry");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_geometry stdout ---\n{stdout}\n--- gl_geometry stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "GLES2 geometry program exited non-zero (code {:?}) — the triangle did not rasterize as expected",
        run.status.code()
    );

    // ---- ASSERT the guest observed a rasterized triangle over the socket -------------------------
    assert!(
        stdout.contains("GL_GEOMETRY_OK"),
        "the GLES2 app drew a green triangle over a red clear and read it back over the socket"
    );
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // ---- Independently assert the SAME rasterized target off the host executor -------------------
    // The GL default target is Bgra8Unorm; read_texture returns native (B,G,R,A) order. Green geometry
    // (0,1,0) → BGRA (0,255,0,255); the red clear (1,0,0) → BGRA (0,0,255,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no {W}x{H} render target captured off the host executor for the GL geometry frame; captured \
             texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    });
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 2;

    let ci = (((H / 2) * W + (W / 2)) * 4) as usize; // covered center → green
    assert!(
        near(px[ci], 0) && near(px[ci + 1], 255) && near(px[ci + 2], 0) && px[ci + 3] == 255,
        "host-side render target center (BGRA) should be the GREEN triangle, got {:?}",
        &px[ci..ci + 4]
    );

    let corner = &px[0..4]; // uncovered corner → red clear
    assert!(
        near(corner[0], 0) && near(corner[1], 0) && near(corner[2], 255) && corner[3] == 255,
        "host-side render target corner (BGRA) should be the RED clear, got {corner:?}"
    );

    // A real geometry raster: both geometry-colored and clear-colored pixels are present (not a full-frame
    // clear, not an empty draw).
    let green = px.chunks_exact(4).filter(|t| near(t[0], 0) && near(t[1], 255) && near(t[2], 0)).count();
    let red = px.chunks_exact(4).filter(|t| near(t[0], 0) && near(t[1], 0) && near(t[2], 255)).count();
    assert!(
        green > 0 && red > 0,
        "both triangle ({green}) and clear ({red}) pixels present — geometry truly rasterized"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
