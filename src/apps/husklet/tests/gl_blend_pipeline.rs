//! REAL GL FIXED-FUNCTION BLEND — a real EGL + GLES2 offscreen program that proves the GPU blend unit
//! (glEnable(GL_BLEND) + glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)) genuinely composites on
//! lavapipe. This SUPERSEDES the fragment-shader-composite workaround in gl_blend.rs: the wgpu executor now
//! honors the protocol blend field (commit ce66ccba) and the GL driver already lowers glBlendFunc, so the
//! composite is done by the blend unit — not by arithmetic in the shader.
//!
//! Three vertical strips over a BLACK clear, all driven by the SAME 50%-alpha GREEN output (0,1,0,0.5) over
//! an opaque RED background — the ONLY difference between the middle and right strips is the blend enable:
//!   LEFT   -> opaque RED background only              (255,0,0,255)
//!   MIDDLE -> GREEN a=0.5, blend DISABLED -> OVERWRITE (0,255,0,128)
//!   RIGHT  -> GREEN a=0.5, blend ENABLED  -> COMPOSITE (128,128,0,191)
//! Identical geometry + identical fragment output yet one overwrites and one composites: only a live
//! framebuffer-blend unit produces that. Asserts the exact colors from BOTH the guest glReadPixels and an
//! independent host read, and writes /tmp/hl-demo/gl_blend_pipeline.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_fixed_function_blend_composites_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glblendpipe-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_blend_pipeline");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_blend_pipeline.c"
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

    let exec = WgpuExecutorServer::start("glblendpipe");
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
        .expect("spawn gl_blend_pipeline");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_blend_pipeline stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_BLEND_PIPELINE_OK"),
        "guest asserted the fixed-function composite over the socket"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // Host read + PNG. BGRA: bg RED (255,0,0)->(0,0,255); overwrite GREEN (0,255,0)->(0,255,0);
    // composite (128,128,0)->(0,128,128).
    let cap = exec.captured();
    let px = cap
        .rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor");
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (B,G,R,A)
    };

    write_png(
        &demo_png_dir().join("gl_blend_pipeline.png"),
        W,
        H,
        &bgra_to_rgba(px),
    );

    let (b, g, r, a) = at(10, 32); // left: opaque RED bg
    assert!(
        near(r, 255) && near(g, 0) && near(b, 0) && near(a, 255),
        "left third must be opaque RED bg (255,0,0,255), got RGBA ({r},{g},{b},{a})"
    );
    let (b, g, r, a) = at(32, 32); // middle: blend OFF -> raw output overwrites
    assert!(
        near(r, 0) && near(g, 255) && near(b, 0) && near(a, 128),
        "middle third (blend DISABLED) must OVERWRITE to the raw output (0,255,0,128), got RGBA ({r},{g},{b},{a})"
    );
    let (b, g, r, a) = at(54, 32); // right: blend ON -> src-over composite
    assert!(
        near(r, 128) && near(g, 128) && near(b, 0) && near(a, 191),
        "right third (blend ENABLED) must src-over COMPOSITE to (128,128,0,191), got RGBA ({r},{g},{b},{a})"
    );

    // The composited olive must genuinely cover the right third — a real per-pixel blend, not a stray texel.
    let composite = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 128) && near(t[1], 128) && near(t[0], 0) && near(t[3], 191))
        .count();
    assert!(
        composite > (W * H / 8) as usize,
        "the composited color must cover the right third, only {composite} matched"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
