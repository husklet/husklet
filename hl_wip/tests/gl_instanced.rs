//! REAL GL INSTANCED — a real EGL + GLES3 offscreen program that draws 4 quads in ONE glDrawArraysInstanced
//! call, each placed + colored by a per-instance attribute (glVertexAttribDivisor(.,1)), rasterized on
//! lavapipe. Instance k lands at pixel column 8/24/40/56 (row 32) in RED/GREEN/BLUE/YELLOW. Asserts each
//! cell's exact color from BOTH the guest glReadPixels and an independent host read, and writes the readback
//! to /tmp/hl-demo/gl_instanced.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles3_instanced_grid_places_each_instance_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glinst-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_instanced");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_instanced.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("glinst");
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
        .expect("spawn gl_instanced");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_instanced stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_INSTANCED_OK"), "guest asserted the 4-instance grid over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read + PNG.
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let rgb_at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i + 2], px[i + 1], px[i]) // (R,G,B) from BGRA
    };
    let is = |x, y, r: u8, g: u8, b: u8| {
        let (rr, gg, bb) = rgb_at(x, y);
        near(rr, r) && near(gg, g) && near(bb, b)
    };

    write_png(&demo_png_dir().join("gl_instanced.png"), W, H, &bgra_to_rgba(px));

    assert!(is(8, 32, 255, 0, 0), "instance 0 cell must be RED, got {:?}", rgb_at(8, 32));
    assert!(is(24, 32, 0, 255, 0), "instance 1 cell must be GREEN, got {:?}", rgb_at(24, 32));
    assert!(is(40, 32, 0, 0, 255), "instance 2 cell must be BLUE, got {:?}", rgb_at(40, 32));
    assert!(is(56, 32, 255, 255, 0), "instance 3 cell must be YELLOW, got {:?}", rgb_at(56, 32));

    let _ = std::fs::remove_dir_all(&out_dir);
}
