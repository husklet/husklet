//! REAL GL ANIMATION — a real EGL + GLES2 offscreen program that draws the same quad across 3 frames,
//! translating it via a uniform each frame and reading each frame back with glReadPixels, rasterized on
//! lavapipe. Proves per-frame fluency: the quad is at 3 successively different, correct positions (columns
//! 16, 32, 48 at row 32) and does NOT accumulate (eglSwapBuffers resets the draw-list each frame). The guest
//! asserts all 3 frames over the socket; this harness additionally turns each frame's readback into a PNG at
//! /tmp/hl-demo/gl_anim_frame{0,1,2}.png and independently confirms the final frame off the host executor.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_uniform_animation_moves_quad_across_frames_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glanim-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_anim");
    let dump_dir = out_dir.join("dump");
    std::fs::create_dir_all(&dump_dir).unwrap();

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_anim.c"
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

    let exec = WgpuExecutorServer::start("glanim");
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
        .env("HL_ANIM_DUMP", &dump_dir)
        .output()
        .expect("spawn gl_anim");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_anim stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_ANIM_OK"),
        "guest asserted the quad at 3 successively different, correct positions over the socket"
    );
    assert!(
        exec.submit_count() >= 3,
        "guest submitted a batch per frame (>=3)"
    );

    // Turn each frame's raw RGBA readback (GL bottom-left row order) into an upright PNG, and check the
    // quad's column per frame directly from the dump — an independent confirmation of the 3 positions.
    let cols = [16u32, 32, 48];
    let png_dir = demo_png_dir();
    for (k, &col) in cols.iter().enumerate() {
        let raw =
            std::fs::read(dump_dir.join(format!("frame{k}.bin"))).expect("frame dump present");
        assert_eq!(raw.len(), (W * H * 4) as usize, "frame{k} dump size");
        // Flip bottom-left -> top-left for an upright PNG.
        let mut upright = vec![0u8; raw.len()];
        for y in 0..H {
            let src = ((H - 1 - y) * W * 4) as usize;
            let dst = (y * W * 4) as usize;
            upright[dst..dst + (W * 4) as usize].copy_from_slice(&raw[src..src + (W * 4) as usize]);
        }
        write_png(
            &png_dir.join(format!("gl_anim_frame{k}.png")),
            W,
            H,
            &upright,
        );

        // In the dump (bottom-left), row 32 is the vertical middle either way; sample the 3 columns.
        let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
        let is_green = |x: u32| {
            let i = ((32 * W + x) * 4) as usize;
            near(raw[i], 0) && near(raw[i + 1], 255) && near(raw[i + 2], 0) && raw[i + 3] == 255
        };
        assert!(
            is_green(col),
            "frame{k}: quad must be GREEN at column {col}"
        );
        for &other in cols.iter().filter(|&&c| c != col) {
            let i = ((32 * W + other) * 4) as usize;
            assert!(
                near(raw[i], 0) && near(raw[i + 1], 0) && near(raw[i + 2], 0),
                "frame{k}: column {other} must be the BLACK clear (quad did not accumulate)"
            );
        }
    }

    // Independently confirm the FINAL frame off the host executor (latest-wins capture): quad at column 48.
    let cap = exec.captured();
    let px = cap
        .rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor");
    write_png(&png_dir.join("gl_anim.png"), W, H, &bgra_to_rgba(px));
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let green_at = |x: u32| {
        let i = ((32 * W + x) * 4) as usize; // BGRA
        near(px[i + 2], 0) && near(px[i + 1], 255) && near(px[i], 0) && px[i + 3] == 255
    };
    assert!(
        green_at(48),
        "final frame's quad must be GREEN at column 48 on the host target"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
