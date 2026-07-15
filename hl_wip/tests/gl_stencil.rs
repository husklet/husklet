//! REAL GL STENCIL TEST — a real EGL + GLES2 offscreen program proving the stencil test GATES rendering to
//! a stamped region, rasterized on lavapipe. Pass A stamps stencil=1 inside a centered rect (color writes
//! OFF); pass B draws a fullscreen RED quad gated to stencil==1, so ONLY the center becomes RED while the
//! BLUE clear survives at the border. This is the regression guard for the shim's stencil lowering: the
//! draws build pipelines with a real `DepthState` stencil state + `Enc::SetStencilReference` and MUST run in
//! a pass carrying a matching `Depth24PlusStencil8` attachment (wgpu enforces this).
//!
//! It proves the gate is REAL two ways: (1) the ENABLED run yields a stencil-gated square — center RED,
//! border BLUE; (2) the DISABLED run (glDisable(GL_STENCIL_TEST) before pass B) yields a FULL-RED frame
//! (the ungated fullscreen quad). If the stencil state were dropped, the enabled run would ALSO be full RED.
//! Asserts from BOTH the guest glReadPixels and an independent host read, and writes /tmp/hl-demo/gl_stencil.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

/// Compile + run the guest once against a fresh lavapipe executor, returning the host-captured BGRA plane.
/// `disable` sets `HL_STENCIL_DISABLE` so pass B runs UNGATED (the regression control).
fn run_variant(bin: &std::path::Path, gl_dir: &std::path::Path, disable: bool) -> Vec<u8> {
    let exec = WgpuExecutorServer::start(if disable { "glstencil-off" } else { "glstencil-on" });
    let adapter = exec.adapter_name();
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "must rasterize on the software Vulkan device, got {adapter:?}"
    );

    let mut cmd = Command::new(bin);
    cmd.env("LD_LIBRARY_PATH", gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_GL_SURFACE_W", W.to_string())
        .env("HL_GL_SURFACE_H", H.to_string());
    if disable {
        cmd.env("HL_STENCIL_DISABLE", "1");
    }
    let run = cmd.output().expect("spawn gl_stencil");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_stencil ({}) stdout ---\n{stdout}\n--- stderr ---\n{stderr}", if disable { "disabled" } else { "enabled" });

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_STENCIL_OK"), "guest asserted its stencil result over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    let cap = exec.captured();
    cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor").clone()
}

#[test]
fn real_gles2_stencil_test_gates_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glstencil-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_stencil");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_stencil.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    // ---- ENABLED: the stencil gate is on → a center RED square inside a BLUE border. ----
    let px = run_variant(&bin, &gl_dir, false);

    // BGRA plane. RED (1,0,0) -> (B,G,R,A)=(0,0,255,255); BLUE (0,0,1) -> (255,0,0,255).
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |px: &[u8], x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (B,G,R,A)
    };
    let is_red = |t: (u8, u8, u8, u8)| near(t.2, 255) && near(t.1, 0) && near(t.0, 0) && t.3 == 255;
    let is_blue = |t: (u8, u8, u8, u8)| near(t.0, 255) && near(t.1, 0) && near(t.2, 0) && t.3 == 255;

    write_png(&demo_png_dir().join("gl_stencil.png"), W, H, &bgra_to_rgba(&px));

    // Exact stencil-gated pixels: the interior of the stamped rect ([16,48)) is RED; the border is BLUE.
    // Sample well inside each region (a texel or two off the [16,48) boundary avoids the seam).
    assert!(is_red(at(&px, 32, 32)), "center must be the gated RED quad, got {:?}", at(&px, 32, 32));
    assert!(is_red(at(&px, 20, 20)), "inside-rect must be RED, got {:?}", at(&px, 20, 20));
    assert!(is_red(at(&px, 44, 44)), "inside-rect must be RED, got {:?}", at(&px, 44, 44));
    assert!(is_blue(at(&px, 4, 4)), "corner must stay BLUE (gated out), got {:?}", at(&px, 4, 4));
    assert!(is_blue(at(&px, 60, 4)), "corner must stay BLUE (gated out), got {:?}", at(&px, 60, 4));
    assert!(is_blue(at(&px, 32, 4)), "top-edge must stay BLUE (gated out), got {:?}", at(&px, 32, 4));

    // Whole-frame proof it is a GATED SQUARE, not a full fill: RED covers roughly the central rect
    // (~[16,48)^2 = 1/4 of the frame) and BLUE covers the surrounding border — BOTH present in quantity.
    let red = px.chunks_exact(4).filter(|t| is_red((t[0], t[1], t[2], t[3]))).count();
    let blue = px.chunks_exact(4).filter(|t| is_blue((t[0], t[1], t[2], t[3]))).count();
    let total = (W * H) as usize;
    assert!(
        red > total / 8 && red < total * 3 / 8,
        "RED must be the ~1/4-frame stamped square, not a full fill: {red}/{total}"
    );
    assert!(blue > total / 2, "BLUE border must dominate the frame: {blue}/{total}");

    // ---- DISABLED: the gate is off → the fullscreen quad paints the WHOLE frame RED (the control). ----
    let px_off = run_variant(&bin, &gl_dir, true);
    let red_off = px_off.chunks_exact(4).filter(|t| is_red((t[0], t[1], t[2], t[3]))).count();
    assert!(
        red_off > total * 15 / 16,
        "with the stencil test DISABLED the ungated fullscreen quad must fill the frame RED: {red_off}/{total}"
    );
    // And with the gate off there is essentially NO surviving BLUE — the exact opposite of the enabled run.
    let blue_off = px_off.chunks_exact(4).filter(|t| is_blue((t[0], t[1], t[2], t[3]))).count();
    assert!(blue_off < total / 16, "disabled run must not leave a BLUE border: {blue_off}/{total}");

    let _ = std::fs::remove_dir_all(&out_dir);
}
