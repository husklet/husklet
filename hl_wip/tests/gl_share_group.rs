//! REAL GL SHARE GROUP — a real EGL + GLES2 program with TWO contexts in a share group. A 2x2 texture with
//! four distinct texels (RED, GREEN, BLUE, WHITE) is created + filled while context A is current, then
//! SAMPLED while context B (created with A as its share_context) is current, rasterized on lavapipe. Proves
//! the object survives the eglMakeCurrent switch and B reads back exactly A's four texels — each screen
//! quadrant holds one, and the four are a permutation of the uploaded set.
//!
//! MODEL: the hl GL shim keeps ONE process-global object namespace across contexts (shim/egl state.rs),
//! so the share is expressed as a shared namespace rather than by tracking the share_context graph — but
//! the cross-context data path (real upload in A, real sample in B, real readback) is genuine end-to-end.
//! Asserts from BOTH the guest glReadPixels and an independent host read, writes /tmp/hl-demo/gl_share_group.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_share_group_texture_crosses_contexts_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glshare-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_share_group");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_share_group.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("glshare");
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
        .expect("spawn gl_share_group");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_share_group stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_SHARE_OK"), "guest asserted B saw A's four texels over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read + PNG. Classify each screen quadrant; require a permutation of {RED,GREEN,BLUE,WHITE}.
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    // px is BGRA; classify by (R,G,B) = (px[2],px[1],px[0]).
    let classify = |x: u32, y: u32| -> i32 {
        let i = ((y * W + x) * 4) as usize;
        let (r, g, b) = (px[i + 2], px[i + 1], px[i]);
        if near(r, 255) && near(g, 0) && near(b, 0) {
            0
        } else if near(r, 0) && near(g, 255) && near(b, 0) {
            1
        } else if near(r, 0) && near(g, 0) && near(b, 255) {
            2
        } else if near(r, 255) && near(g, 255) && near(b, 255) {
            3
        } else {
            -1
        }
    };

    write_png(&demo_png_dir().join("gl_share_group.png"), W, H, &bgra_to_rgba(px));

    let quads = [classify(16, 16), classify(48, 16), classify(16, 48), classify(48, 48)];
    let mut seen = [false; 4];
    for &c in &quads {
        assert!(c >= 0 && c <= 3, "a screen quadrant was not one of A's four texels: {quads:?}");
        assert!(!seen[c as usize], "A texel class {c} appeared twice — not a clean permutation: {quads:?}");
        seen[c as usize] = true;
    }
    assert!(seen.iter().all(|&s| s), "all four of A's texels (RED,GREEN,BLUE,WHITE) must cross into B: {quads:?}");

    let _ = std::fs::remove_dir_all(&out_dir);
}
