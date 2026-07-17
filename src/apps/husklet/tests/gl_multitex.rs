//! REAL GL MULTI-TEXTURE — a real EGL + GLES2 offscreen program that samples TWO textures AND a uniform
//! block in one draw, rasterized on lavapipe. The regression guard for multi-texture GUI apps.
//!
//! `../../surface/hl-gl/tests/fixtures/gl_multitex.c` is a header-free real GLES2 app (the EGL/GLES2 ABI is self-declared). It draws a
//! fullscreen triangle whose fragment writes `vec4(texA.r, texB.g, uTint.z, 1.0)` — a per-channel
//! provenance: RED comes from sampler A, GREEN from sampler B, BLUE from the uniform block. With A = solid
//! red, B = solid green, and `uTint.z = 0.5`, a CORRECT bind yields RGBA (255,255,128,255) at every covered
//! pixel. Any binding swap or UBO/sampler collision changes at least one channel, so this pixel is a
//! detector for the bug.
//!
//! Why this FAILS before the translator/lowering fix: naga's `glsl-in` REJECTS a combined `uniform
//! sampler2D` outright (`NotImplemented("variable qualifier")`), so the old translator's sampler
//! declarations never compiled a shader module on the host at all — and even had they, the old lowering put
//! the UBO at binding 1 and the 2nd sampler ALSO at binding 1, aliasing them in wgpu's single bind-group
//! namespace. The fix emits each sampler as a separate `texture2D` + `sampler` recombined via a
//! `sampler2D(tex, samp)` constructor, at DISTINCT bindings (UBO 0, sampler k texture 1+2k / sampler 2+2k),
//! matching the frame's bind-group IR. We assert the combined pixel from BOTH the guest's own glReadPixels
//! and an independent host-side read of the same rasterized target.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_two_textures_plus_uniform_rasterize_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(
        gl_dir.join("libEGL.so.1").exists(),
        "staged libEGL.so.1 missing at {gl_dir:?} — build hl-gl's shim first"
    );
    assert!(
        gl_dir.join("libGLESv2.so.2").exists(),
        "staged libGLESv2.so.2 missing at {gl_dir:?}"
    );

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-glmtex-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_multitex");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_multitex.c"
        ))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the GLES2 multi-texture program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exec = WgpuExecutorServer::start("glmtex");
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
        .expect("spawn gl_multitex");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_multitex stdout ---\n{stdout}\n--- gl_multitex stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "GLES2 multi-texture program exited non-zero (code {:?}) — the two textures + uniform did not \
         combine as expected (a binding swap/collision or an uncompilable combined sampler)",
        run.status.code()
    );

    // ---- ASSERT the guest observed the correct combined pixel over the socket ---------------------
    assert!(
        stdout.contains("GL_MULTITEX_OK"),
        "the GLES2 app sampled BOTH textures + the uniform and read the combined pixel back over the socket"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // ---- Independently assert the SAME rasterized target off the host executor --------------------
    // The GL default target is Bgra8Unorm; read_texture returns native (B,G,R,A) order. The combined
    // fragment RGBA (255,255,128,255) → BGRA (128,255,255,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no {W}x{H} render target captured off the host executor for the GL multi-texture frame; \
             captured texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    });
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 3;

    let ci = (((H / 2) * W + (W / 2)) * 4) as usize; // covered center → combined
    let (b, g, r, a) = (px[ci], px[ci + 1], px[ci + 2], px[ci + 3]);
    assert!(
        near(r, 255) && near(g, 255) && near(b, 128) && a == 255,
        "host-side render target center (BGRA) should be the combined (R=uTexA, G=uTexB, B=uTint.z) pixel \
         BGRA (128,255,255,255), got ({b},{g},{r},{a}) — a binding swap or UBO/sampler collision"
    );

    // The combined color covers the whole triangle: many pixels carry the exact three-source signature, so
    // it is a real raster of both textures + the uniform (not a clear, not a single-source draw).
    let combined = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 255) && near(t[1], 255) && near(t[0], 128) && t[3] == 255)
        .count();
    assert!(
        combined > (W * H / 4) as usize,
        "the combined-source pixel must cover most of the frame, only {combined} matched"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
