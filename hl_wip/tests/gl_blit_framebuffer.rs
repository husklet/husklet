//! REAL GL BLIT FRAMEBUFFER — a real EGL + GLES offscreen program that renders a SOURCE FBO solid RED,
//! clears a DEST FBO solid BLUE, then glBlitFramebuffer()s an exact centered 32x32 sub-rect ([16,48]²)
//! src -> dst at the same coordinates (equal size, no scaling), rasterized on lavapipe. The equal-size blit
//! lowers to Enc::CopyTextureToTexture. The destination reads back RED inside the centered square and BLUE
//! everywhere outside — the copied region AND the untouched border are asserted at exact pixel boundaries.
//!
//! Exercises the hl_wip-gl blit path (record.rs blit_framebuffer records a BlitOp; frame.rs
//! build_multi_pass_frame lowers it to a CopyTextureToTexture after the render passes). Asserts from BOTH
//! the guest glReadPixels and an independent host read of the destination texture, writes
//! /tmp/hl-demo/gl_blit_framebuffer.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles_blit_framebuffer_subrect_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glblit-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_blit_framebuffer");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_blit_framebuffer.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("glblit");
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
        .expect("spawn gl_blit_framebuffer");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_blit_framebuffer stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_BLIT_OK"), "guest asserted the copied sub-rect + untouched border over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read: the destination FBO texture is 64x64 Rgba8Unorm with a RED center square on a BLUE field.
    // Both attachment textures are 64x64; pick the one that matches that pattern (the source is all RED).
    let cap = exec.captured();
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let want = (W * H * 4) as usize;
    // RGBA byte order (offscreen Rgba8Unorm).
    let dst = cap
        .textures
        .values()
        .find(|px| {
            px.len() == want && {
                let at = |x: u32, y: u32| {
                    let i = ((y * W + x) * 4) as usize;
                    (px[i], px[i + 1], px[i + 2])
                };
                let (cr, cg, cb) = at(32, 32); // center RED
                let (lr, lg, lb) = at(10, 32); // border BLUE
                near(cr, 255) && near(cg, 0) && near(cb, 0) && near(lr, 0) && near(lg, 0) && near(lb, 255)
            }
        })
        .expect("no destination FBO texture (RED square on BLUE) captured off the host executor");

    write_png(&demo_png_dir().join("gl_blit_framebuffer.png"), W, H, dst); // already RGBA

    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (dst[i], dst[i + 1], dst[i + 2])
    };
    let red = |t: (u8, u8, u8)| near(t.0, 255) && near(t.1, 0) && near(t.2, 0);
    let blue = |t: (u8, u8, u8)| near(t.0, 0) && near(t.1, 0) && near(t.2, 255);

    // Copied centered square [16,48)² is RED (host top-left rows; the centered rect is Y-flip symmetric).
    assert!(red(at(32, 32)), "center of the blitted square must be RED");
    assert!(red(at(20, 20)), "inside the blitted square must be RED");
    assert!(red(at(44, 44)), "inside the blitted square (far corner) must be RED");
    // Border outside the square stays the dst clear BLUE — untouched by the blit.
    assert!(blue(at(4, 4)), "untouched corner must stay BLUE");
    assert!(blue(at(10, 32)), "left of the square must stay BLUE");
    assert!(blue(at(54, 32)), "right of the square must stay BLUE");
    assert!(blue(at(32, 4)), "above the square must stay BLUE");
    assert!(blue(at(32, 60)), "below the square must stay BLUE");

    // The copied square must genuinely cover ~32x32 (~1024) RED texels — a real region copy, not a stray texel.
    let red_count = dst.chunks_exact(4).filter(|t| near(t[0], 255) && near(t[1], 0) && near(t[2], 0)).count();
    assert!(
        (700..1400).contains(&red_count),
        "the blitted RED square must be ~32x32 texels, got {red_count}"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
