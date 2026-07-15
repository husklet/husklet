//! REAL GL UBO RANGES — a real EGL + GLES3 program that binds TWO ranges of ONE buffer to TWO distinct
//! uniform-block binding points via `glBindBufferRange`, and has the fragment shader read BOTH blocks and
//! combine them ASYMMETRICALLY. The combine takes `.r`/`.b` from block 0 and `.g` from block 1, so the
//! exact output pixel (255,255,128,255) is a unique fingerprint that BOTH ranges reached their own binding
//! (a mis-bind or a swap yields a different, documented color). This is the regression guard for
//! `hl_wip-gl`'s multi-block UBO routing (`src/service/record.rs::assemble_multi_block_ubo_bytes` +
//! `src/adapter/glsl.rs::uniform_blocks`), which assembles the flattened binding-0 block from each block's
//! own `glBindBufferRange`d range. Asserted from the guest AND an independent host read; the frame is
//! written to /tmp/hl-demo/gl_ubo_ranges.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles3_bind_buffer_range_routes_two_blocks_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glubor-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_ubo_ranges");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_ubo_ranges.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("glubor");
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
        .expect("spawn gl_ubo_ranges");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_ubo_ranges stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(
        stdout.contains("GL_UBO_RANGES_OK"),
        "guest asserted the two glBindBufferRange ranges each fed their own block"
    );
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read + PNG. Expected fragment (colorA.r, colorB.g, colorA.b, 1) = (255,255,128,255).
    // BGRA order from rgba8_texture: (128,255,255,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    write_png(&demo_png_dir().join("gl_ubo_ranges.png"), W, H, &bgra_to_rgba(px));
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;

    let i = ((32 * W + 32) * 4) as usize;
    let (b, g, r, a) = (px[i], px[i + 1], px[i + 2], px[i + 3]);
    assert!(
        near(r, 255) && near(g, 255) && near(b, 128) && a == 255,
        "center must be (R=colorA.r, G=colorB.g, B=colorA.b) = RGBA (255,255,128,255), got ({r},{g},{b},{a}) \
         — a glBindBufferRange range did not reach its binding"
    );
    // The combined color must fill the frame (a real full-quad draw fed by both ranges).
    let hit = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 255) && near(t[1], 255) && near(t[0], 128) && t[3] == 255)
        .count();
    assert_eq!(hit, (W * H) as usize, "the whole frame must be the combined color, got {hit}/{}", W * H);

    let _ = std::fs::remove_dir_all(&out_dir);
}
