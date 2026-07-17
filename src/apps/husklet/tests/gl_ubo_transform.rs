//! REAL GL UBO TRANSFORM — a real EGL + GLES3 offscreen program whose MVP lives in a std140 uniform BLOCK
//! (set via glGenBuffers + glBufferData(std140) + glBindBufferBase + glUniformBlockBinding), rasterized on
//! lavapipe. The EXACT-RECT guard for UBO-block routing: the MVP both SCALES and TRANSLATES a full-NDC quad
//! into a KNOWN off-center rectangle (NDC x in [-0.25,0.75], y in [-0.5,0.5] -> pixel cols [24,56) x rows
//! [16,48) on a 64x64 target), filled GREEN from the block's uColor over a BLUE clear.
//!
//! This directly verifies the shim binds the `glBindBufferBase`d UBO buffer's std140 bytes at IR binding 0
//! (not a synthesized flat/zero block): a wrong route collapses the geometry (mvp=0 -> nothing) or misplaces
//! it (identity -> full frame), moving at least one of the sampled pixels off its expected color. We assert
//! the exact rect from BOTH the guest's own glReadPixels and an independent host-side read, and write the
//! host readback to /tmp/hl-demo/gl_ubo_transform.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles3_ubo_mvp_places_quad_at_exact_rect_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-gluboxf-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_ubo_transform");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_ubo_transform.c"
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

    let exec = WgpuExecutorServer::start("gluboxf");
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
        .expect("spawn gl_ubo_transform");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_ubo_transform stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_UBOXFORM_OK"),
        "guest asserted the quad at the exact rect over the socket"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // ---- Independent host-side read + PNG. Target is Bgra8Unorm (native B,G,R,A). GREEN (0,255,0) ->
    // BGRA (0,255,0); the BLUE clear (0,0,255) -> BGRA (255,0,0).
    let cap = exec.captured();
    let px = cap
        .rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor");
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (B,G,R,A)
    };
    let is_green = |x, y| {
        let (b, g, r, a) = at(x, y);
        near(r, 0) && near(g, 255) && near(b, 0) && a == 255
    };
    let is_blue = |x, y| {
        let (b, g, r, a) = at(x, y);
        near(r, 0) && near(g, 0) && near(b, 255) && a == 255
    };

    write_png(
        &demo_png_dir().join("gl_ubo_transform.png"),
        W,
        H,
        &bgra_to_rgba(px),
    );

    assert!(
        is_green(40, 32),
        "rect interior must be GREEN, got {:?}",
        at(40, 32)
    );
    assert!(
        is_blue(12, 32),
        "left of the rect must be the BLUE clear, got {:?}",
        at(12, 32)
    );
    assert!(
        is_blue(58, 32),
        "right of the rect must be the BLUE clear, got {:?}",
        at(58, 32)
    );
    assert!(
        is_blue(40, 6),
        "above the rect must be the BLUE clear, got {:?}",
        at(40, 6)
    );
    assert!(
        is_blue(40, 58),
        "below the rect must be the BLUE clear, got {:?}",
        at(40, 58)
    );

    // The transformed GREEN fill must cover roughly the rect's area (32 cols x 32 rows = 1024 px of 4096),
    // proving a real scale+translate (not full-frame identity, not a collapsed zero matrix).
    let green = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 0) && near(t[1], 255) && near(t[0], 0) && t[3] == 255)
        .count();
    assert!(
        (700..1400).contains(&green),
        "the transformed GREEN quad must cover ~1024 px (the exact rect), got {green}"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
