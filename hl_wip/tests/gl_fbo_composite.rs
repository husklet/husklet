//! REAL GL MULTI-FBO COMPOSITE — a real EGL + GLES2 program that renders into an OFFSCREEN framebuffer, then
//! samples that texture from the DEFAULT framebuffer to composite over the window, rasterized on lavapipe.
//! The regression guard for the multi-FBO frame-graph (GskGL / GTK4 and every offscreen-FBO GL app).
//!
//! `csrc/gl_fbo_composite.c` is a header-free real GLES2 app (the EGL/GLES2 ABI is self-declared). Pass 1
//! fills a tiny 16x16 offscreen FBO solid RED; pass 2 binds the DEFAULT framebuffer, clears the 64x64 window
//! BLUE, then draws a fullscreen quad whose fragment SAMPLES the offscreen texture — so the whole window
//! becomes RED. It glReadPixels the DEFAULT framebuffer at the full 64x64 and asserts a WINDOW-corner pixel
//! (60,60), far beyond the 16x16 offscreen extent, is RED.
//!
//! Why this FAILS before the per-FBO frame-graph fix: the old frame lowering collapsed the ENTIRE recorded
//! frame onto the FIRST geometry draw's FBO — the 16x16 offscreen target — so the presented + read-back
//! target was 16x16, and a window-corner pixel at (60,60) read back ZERO (outside the 16x16 plane), and no
//! 64x64 render target was ever created. The fix lowers a render pass PER bound framebuffer: pass 1 renders
//! the offscreen texture, pass 2 (the default framebuffer) samples it and composites into the WINDOW color
//! target at window dimensions — which is what eglSwapBuffers/glReadPixels present + read back. We assert the
//! composited window from BOTH the guest's own glReadPixels and an independent host-side read of the 64x64
//! target off the executor.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_offscreen_fbo_composited_over_the_window_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(
        gl_dir.join("libEGL.so.1").exists(),
        "staged libEGL.so.1 missing at {gl_dir:?} — build hl_wip-gl's shim first"
    );
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-glfbo-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_fbo_composite");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_fbo_composite.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the GLES2 multi-FBO composite program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exec = WgpuExecutorServer::start("glfbo");
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
        .expect("spawn gl_fbo_composite");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_fbo_composite stdout ---\n{stdout}\n--- gl_fbo_composite stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "GLES2 multi-FBO composite program exited non-zero (code {:?}) — the window read back the offscreen \
         extent instead of the composited window (the frame collapsed onto the offscreen FBO)",
        run.status.code()
    );

    // ---- ASSERT the guest read the composited WINDOW back over the socket ------------------------
    assert!(
        stdout.contains("GL_FBO_COMPOSITE_OK"),
        "the GLES2 app rendered into an offscreen FBO, sampled it from the default framebuffer, and read the \
         composited WINDOW (a corner pixel beyond the offscreen extent) back over the socket"
    );
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // ---- Independently assert the SAME composited target off the host executor -------------------
    // A 64x64 default (window) target must exist — the fix creates it. Before the fix only a 16x16 offscreen
    // target existed and this lookup returns None. The GL default target is Bgra8Unorm; read_texture returns
    // native (B,G,R,A). The composited RED (1,0,0) → BGRA (0,0,255,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no {W}x{H} WINDOW render target captured off the host executor — the multi-FBO frame collapsed \
             onto the offscreen target; captured texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    });
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 3;

    // A WINDOW-corner pixel (60,60), far outside the 16x16 offscreen extent, must be the composited RED.
    let corner = (((H - 4) * W + (W - 4)) * 4) as usize;
    let (b, g, r, a) = (px[corner], px[corner + 1], px[corner + 2], px[corner + 3]);
    assert!(
        near(r, 255) && near(g, 0) && near(b, 0) && a == 255,
        "host-side WINDOW corner (60,60) (BGRA) should be the composited RED offscreen sample BGRA \
         (0,0,255,255), got ({b},{g},{r},{a}) — a window pixel beyond the 16x16 offscreen proves the present \
         target is the window, not the collapsed atlas"
    );

    // The composited RED covers the whole window (the fullscreen quad sampled a solid-red offscreen), so most
    // of the 64x64 frame carries it — a real cross-FBO composite, not a clear and not a single small draw.
    let red = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 255) && near(t[1], 0) && near(t[0], 0) && t[3] == 255)
        .count();
    assert!(
        red > (W * H / 2) as usize,
        "the composited offscreen sample must cover most of the window, only {red} pixels matched"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
