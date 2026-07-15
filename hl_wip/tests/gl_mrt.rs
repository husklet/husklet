//! REAL GL MULTIPLE RENDER TARGETS (MRT) — a real EGL + GLES3 offscreen program that binds an FBO with TWO
//! color attachments, glDrawBuffers([ATTACHMENT0, ATTACHMENT1]), and a single draw whose fragment shader
//! writes BOTH outputs — rasterized on lavapipe as one pass with two color targets. Each attachment is read
//! back (glReadBuffer + glReadPixels) and asserted distinct: attachment 0 = RED, attachment 1 = GREEN.
//!
//! Exercises the hl_wip-gl MRT path (framebuffer multi-attachment + build_mrt_geometry_frame + multi-`out`
//! GLSL emission + glReadBuffer readback selection). Asserts from BOTH the guest per-attachment glReadPixels
//! and an independent host read of the captured textures, and writes /tmp/hl-demo/gl_mrt_attach0.png +
//! gl_mrt_attach1.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles3_mrt_two_attachments_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glmrt-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_mrt");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_mrt.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("glmrt");
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
        .expect("spawn gl_mrt");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_mrt stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_MRT_OK"), "guest asserted the two attachments' distinct values over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read: the two attachment textures are both 64x64 and, being offscreen FBO targets, are
    // Rgba8Unorm (RGBA byte order — unlike the default surface's BGRA). Collect every 64x64 capture and
    // require the SET to be exactly {solid RED, solid GREEN} — both MRT outputs genuinely landed on the GPU,
    // each holding its own distinct value. (Their IR ids are shared-namespace-allocated, so match by content.)
    let cap = exec.captured();
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let want = (W * H * 4) as usize;
    let is_solid = |px: &[u8], r: u8, g: u8, b: u8| {
        px.chunks_exact(4).all(|t| near(t[0], r) && near(t[1], g) && near(t[2], b)) // RGBA
    };

    let mut red_seen = false;
    let mut green_seen = false;
    let mut n_targets = 0;
    for px in cap.textures.values() {
        if px.len() != want {
            continue;
        }
        n_targets += 1;
        if is_solid(px, 255, 0, 0) {
            red_seen = true;
            write_png(&demo_png_dir().join("gl_mrt_attach0.png"), W, H, px); // already RGBA
        } else if is_solid(px, 0, 255, 0) {
            green_seen = true;
            write_png(&demo_png_dir().join("gl_mrt_attach1.png"), W, H, px); // already RGBA
        }
    }

    assert_eq!(n_targets, 2, "MRT must materialize exactly two 64x64 attachment textures, found {n_targets}");
    assert!(red_seen, "attachment 0 must be solid RED (its own fragment output o0)");
    assert!(green_seen, "attachment 1 must be solid GREEN (its own fragment output o1)");

    let _ = std::fs::remove_dir_all(&out_dir);
}
