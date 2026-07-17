//! REAL GL SCALING BLIT — a real EGL + GLES offscreen program that renders a 2x2 SOURCE FBO and
//! glBlitFramebuffer()s it 2x-UPSCALED into a larger DESTINATION FBO, once with GL_NEAREST and once with
//! GL_LINEAR, resampled on lavapipe. A scaling blit (source extent != destination extent) lowers to
//! `Enc::BlitTexture` carrying the filter — the exact op that was DROPPED before this change (only the
//! equal-size blit lowered, to `Enc::CopyTextureToTexture`). This proves the new
//! `frame::blit_copy_enc` scaling branch end-to-end.
//!
//! The guest asserts exact pixels over its own glReadPixels (nearest = each source texel replicated into an
//! exact block; linear 2x2->3x3 = the middle column is exactly (al+bl)/2). This harness INDEPENDENTLY reads
//! the host executor's 64x64 upscale textures and re-asserts: the nearest upscale is four crisp 32x32
//! uniform blocks (the source colors, as a set); the linear upscale is a monotonic, interpolated gradient
//! (not blocks). Both are written to /tmp/hl-demo/ as PNGs for a visual confrontation.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{demo_png_dir, staged_dir, write_png};

const BIG: u32 = 64;

/// Parse every `r,g,b` triple that follows a `=` on a labeled guest line (e.g. `... (0,0)=200,30,40 ...`).
fn triples_after_eq(line: &str) -> Vec<[u8; 3]> {
    let mut out = Vec::new();
    for seg in line.split('=').skip(1) {
        let nums: Vec<u8> = seg
            .trim_start()
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .take(3)
            .filter_map(|s| s.parse::<u8>().ok())
            .collect();
        if nums.len() == 3 {
            out.push([nums[0], nums[1], nums[2]]);
        }
    }
    out
}

#[test]
fn real_gles_scaling_blit_upscales_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glblitscale-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_blit_scale");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_blit_scale.c"
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

    let exec = WgpuExecutorServer::start("glblitscale");
    let adapter = exec.adapter_name();
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "must rasterize on the software Vulkan device, got {adapter:?}"
    );

    let run = Command::new(&bin)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_GL_SURFACE_W", "64")
        .env("HL_GL_SURFACE_H", "64")
        .output()
        .expect("spawn gl_blit_scale");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_blit_scale stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_BLIT_SCALE_OK"),
        "guest must assert its exact nearest-block + linear-midpoint pixels over the socket"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // Parse the guest-reported source truth (four distinct texels) and gradient edges (al/bl).
    let src_line = stdout
        .lines()
        .find(|l| l.contains("GL_BLIT_SCALE_SRC"))
        .expect("source-truth line");
    let src_colors = triples_after_eq(src_line);
    assert_eq!(
        src_colors.len(),
        4,
        "four source texels reported, got {src_colors:?}"
    );
    let grad_line = stdout
        .lines()
        .find(|l| l.contains("GL_BLIT_SCALE_GRAD"))
        .expect("gradient line");
    let grad = triples_after_eq(grad_line); // [al, bl, mid]
    assert_eq!(grad.len(), 3, "al/bl/mid reported, got {grad:?}");
    let (al, bl) = (grad[0], grad[1]);

    // ---- Independent host read: the two 64x64 upscale textures off lavapipe. ----
    let cap = exec.captured();
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 2;
    let want = (BIG * BIG * 4) as usize;
    let bigs: Vec<&Vec<u8>> = cap.textures.values().filter(|v| v.len() == want).collect();
    assert!(
        bigs.len() >= 2,
        "expected two 64x64 upscale textures captured, got {}",
        bigs.len()
    );

    let px = |t: &[u8], x: u32, y: u32| {
        let i = ((y * BIG + x) * 4) as usize;
        [t[i], t[i + 1], t[i + 2]]
    };
    // Distinct red values across the middle row separate a NEAREST upscale (2 blocks) from a LINEAR gradient.
    let distinct_reds_mid = |t: &[u8]| {
        let mut reds: Vec<u8> = (0..BIG).map(|x| px(t, x, 32)[0]).collect();
        reds.sort_unstable();
        reds.dedup();
        reds.len()
    };
    let magenta = |c: [u8; 3]| near(c[0], 255) && near(c[1], 0) && near(c[2], 255);

    let nearest_big = *bigs
        .iter()
        .find(|t| distinct_reds_mid(t) <= 3)
        .expect("a crisp-block (nearest) 64x64");
    let linear_big = *bigs
        .iter()
        .find(|t| distinct_reds_mid(t) > 5)
        .expect("a gradient (linear) 64x64");

    write_png(
        &demo_png_dir().join("gl_blit_scale.png"),
        BIG,
        BIG,
        nearest_big,
    ); // already RGBA
    write_png(
        &demo_png_dir().join("gl_blit_scale_linear.png"),
        BIG,
        BIG,
        linear_big,
    );

    // NEAREST: four internally-uniform 32x32 blocks, no magenta sentinel, and the block-color SET equals the
    // reported source colors (order-independent — the GL blit is Y-flip consistent but corner-agnostic here).
    let mut block_colors = Vec::new();
    for (bx, by) in [(0u32, 0u32), (1, 0), (0, 1), (1, 1)] {
        let ref_c = px(nearest_big, bx * 32 + 8, by * 32 + 8);
        for y in (by * 32)..(by * 32 + 32) {
            for x in (bx * 32)..(bx * 32 + 32) {
                let c = px(nearest_big, x, y);
                assert!(
                    near(c[0], ref_c[0]) && near(c[1], ref_c[1]) && near(c[2], ref_c[2]),
                    "NEAREST block ({bx},{by}) must be uniform: ({x},{y})={c:?} vs {ref_c:?}"
                );
                assert!(
                    !magenta(c),
                    "NEAREST ({x},{y}) still the magenta sentinel — blit dropped this texel"
                );
            }
        }
        block_colors.push(ref_c);
    }
    for i in 0..4 {
        for j in (i + 1)..4 {
            assert!(
                block_colors[i] != block_colors[j],
                "the four NEAREST blocks must be distinct: {:?}",
                block_colors
            );
        }
    }
    for want_c in &src_colors {
        assert!(
            block_colors.iter().any(|b| near(b[0], want_c[0]) && near(b[1], want_c[1]) && near(b[2], want_c[2])),
            "each source color {want_c:?} must appear as a NEAREST block; blocks = {block_colors:?}"
        );
    }

    // LINEAR: a real interpolated gradient — red non-decreasing L→R on every row, the left/right edges clamp
    // to al/bl, the interior genuinely varies (not blocks), and no sentinel survives.
    for y in 0..BIG {
        for x in 1..BIG {
            let l = px(linear_big, x - 1, y)[0];
            let r = px(linear_big, x, y)[0];
            assert!(
                r as i32 + 1 >= l as i32,
                "LINEAR red must be non-decreasing L→R at ({x},{y}): {l} then {r}"
            );
        }
        let left = px(linear_big, 0, y);
        let right = px(linear_big, BIG - 1, y);
        assert!(
            near(left[0], al[0]) && near(left[1], al[1]) && near(left[2], al[2]),
            "LINEAR left edge clamps to al {al:?}, got {left:?}"
        );
        assert!(
            near(right[0], bl[0]) && near(right[1], bl[1]) && near(right[2], bl[2]),
            "LINEAR right edge clamps to bl {bl:?}, got {right:?}"
        );
        assert!(
            !magenta(px(linear_big, 32, y)),
            "LINEAR ({},{y}) still the magenta sentinel",
            32
        );
    }
    // The interior must interpolate — the mid column differs from both edges (proves a resample, not a copy).
    let midc = px(linear_big, 32, 32);
    assert!(
        midc != al && midc != bl,
        "LINEAR interior must interpolate between al {al:?} and bl {bl:?}, got mid {midc:?}"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
