//! REAL GL TEXTURE WRAP — a real EGL + GLES2 offscreen program that proves the sampler address mode
//! (GL_TEXTURE_WRAP_S = GL_REPEAT vs GL_CLAMP_TO_EDGE) is honored end-to-end on lavapipe. A 2x2 texture with
//! a RED column (texel 0) and a GREEN column (texel 1) is sampled with NEAREST over a UV.x ramping 0->2 per
//! half: the LEFT half's GL_REPEAT texture TILES a second time (R G R G stripes), the RIGHT half's
//! GL_CLAMP_TO_EDGE texture holds the GREEN edge past u=1 (R G G G). The decisive taps are past u=1.0 where
//! REPEAT wraps back to the RED texel while CLAMP stays on the GREEN edge — an exact-pixel address-mode
//! detector. Asserts from BOTH the guest glReadPixels and an independent host read, writes
//! /tmp/hl-demo/gl_texture_wrap.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_texture_wrap_repeat_vs_clamp_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-gltexwrap-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_texture_wrap");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_texture_wrap.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("gltexwrap");
    let adapter = exec.adapter_name();
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "must rasterize on the software Vulkan device, got {adapter:?}"
    );

    let run = Command::new(&bin)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_GL_SURFACE_W", W.to_string())
        .env("HL_GL_SURFACE_H", H.to_string())
        .output()
        .expect("spawn gl_texture_wrap");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_texture_wrap stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_TEXWRAP_OK"), "guest asserted REPEAT-vs-CLAMP taps over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read + PNG. BGRA: RED (255,0,0)->(0,0,255), GREEN (0,255,0)->(0,255,0).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 6;
    let red = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        near(px[i + 2], 255) && near(px[i + 1], 0) && near(px[i], 0)
    };
    let green = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        near(px[i + 2], 0) && near(px[i + 1], 255) && near(px[i], 0)
    };

    write_png(&demo_png_dir().join("gl_texture_wrap.png"), W, H, &bgra_to_rgba(px));

    // LEFT half = GL_REPEAT: the R G R G tiling. The x=20 tap is PAST u=1.0 and REPEAT wraps it back to RED.
    assert!(red(4, 32), "REPEAT u≈0.28 must be RED");
    assert!(green(12, 32), "REPEAT u≈0.78 must be GREEN");
    assert!(red(20, 32), "REPEAT u≈1.28 (PAST 1.0) must WRAP back to RED");
    assert!(green(28, 32), "REPEAT u≈1.78 must WRAP to GREEN");

    // RIGHT half = GL_CLAMP_TO_EDGE: R then GREEN held. The x=52 tap is PAST u=1.0 and CLAMP holds the edge.
    assert!(red(36, 32), "CLAMP u≈0.28 must be RED");
    assert!(green(44, 32), "CLAMP u≈0.78 must be GREEN");
    assert!(green(52, 32), "CLAMP u≈1.28 (PAST 1.0) must HOLD the GREEN edge (no wrap)");
    assert!(green(60, 32), "CLAMP u≈1.78 must HOLD the GREEN edge");

    // The decisive difference: the same past-1.0 UV is RED under REPEAT (x=20) but GREEN under CLAMP (x=52).
    assert!(red(20, 32) && green(52, 32), "the same past-1.0 UV must differ: REPEAT->RED, CLAMP->GREEN");

    let _ = std::fs::remove_dir_all(&out_dir);
}
