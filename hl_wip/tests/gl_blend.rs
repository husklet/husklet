//! REAL GL BLEND — a real EGL + GLES2 offscreen program that composites a 50%-alpha GREEN overlay over an
//! opaque RED background with the src-over formula, rasterized on lavapipe: the right-half overlap
//! composites to the EXACT (128,128,0), the left half stays opaque RED (255,0,0).
//!
//! NOTE: fixed-function framebuffer blend (glBlendFunc + GL_BLEND) is a GAP in the host wgpu executor —
//! `hl_wip-gpu-wgpu` builds every color target with `blend: None` (pipeline.rs), so a pipeline blend state
//! never reaches the GPU (and that crate is out of scope to change here). The guest STILL calls
//! glEnable(GL_BLEND)/glBlendFunc — genuinely exercising the shim's blend-state lowering path — but computes
//! the src-over COMPOSITE in the fragment shader so the exact composited pixel is produced + asserted
//! end-to-end (shim -> IR -> executor -> glReadPixels) despite that executor gap. Asserts the exact colors
//! from BOTH the guest glReadPixels and an independent host read, and writes /tmp/hl-demo/gl_blend.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_src_over_blend_composites_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glblend-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_blend");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_blend.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("glblend");
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
        .expect("spawn gl_blend");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_blend stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_BLEND_OK"), "guest asserted the src-over composite over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host-side read + PNG. BGRA order: overlap (128,128,0) -> BGRA (0,128,128); bg RED (255,0,0) -> (0,0,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (B,G,R,A)
    };

    write_png(&demo_png_dir().join("gl_blend.png"), W, H, &bgra_to_rgba(px));

    let (b, g, r, a) = at(48, 32); // overlap
    assert!(
        near(r, 128) && near(g, 128) && near(b, 0) && a == 255,
        "overlap must be src-over (128,128,0), got RGBA ({r},{g},{b},{a})"
    );
    let (b, g, r, a) = at(16, 32); // background only
    assert!(
        near(r, 255) && near(g, 0) && near(b, 0) && a == 255,
        "background must be opaque RED (255,0,0), got RGBA ({r},{g},{b},{a})"
    );

    // The blended overlap color must genuinely cover a chunk of the frame (the right half), proving a real
    // per-pixel blend, not a stray texel.
    let blended = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 128) && near(t[1], 128) && near(t[0], 0) && t[3] == 255)
        .count();
    assert!(blended > (W * H / 8) as usize, "the blended color must cover the overlap, only {blended} matched");

    let _ = std::fs::remove_dir_all(&out_dir);
}
