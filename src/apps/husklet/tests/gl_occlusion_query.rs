//! REAL GL OCCLUSION QUERY — a real EGL + GLES3 program that wraps `GL_ANY_SAMPLES_PASSED` occlusion
//! queries around two draws and proves `glGetQueryObjectuiv` reports coverage that reflects REALITY. A
//! visible green draw makes its query read `GL_TRUE` (non-zero); a fully-scissored red draw
//! (`glScissor(0,0,0,0)`) makes its query read `GL_FALSE` (0). This is the GL analogue of the Vulkan
//! occlusion fix (commit 5551f63a): the shim previously resolved every occlusion query to a fake constant
//! `0`; now `hl-gl` accumulates each draw's scissor-clipped footprint into the open query (see
//! `src/model/es3.rs::Queries::{begin,accumulate,end}` + `src/service/record.rs::draw_coverage`).
//!
//! Asserted from the guest (q1 != 0, q2 == 0, q1 != q2, center GREEN) AND from an independent host read of
//! the rendered target; the readback is written to /tmp/hl-demo/gl_occlusion_query.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles3_occlusion_query_reflects_scissor_coverage_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glocc-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_occlusion_query");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_occlusion_query.c"
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

    let exec = WgpuExecutorServer::start("glocc");
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
        .expect("spawn gl_occlusion_query");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_occlusion_query stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_OCCLUSION_OK"),
        "guest asserted the occlusion query split on visibility (q1 visible != q2 scissored)"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // The exact query values the guest read back, parsed from stdout — asserted here too so the harness
    // owns the truth, not just the guest.
    let parse = |key: &str| -> u32 {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.trim().parse::<u32>().ok())
            .unwrap_or_else(|| panic!("missing {key} in guest stdout:\n{stdout}"))
    };
    let q1 = parse("GL_OCCLUSION_Q1:");
    let q2 = parse("GL_OCCLUSION_Q2:");
    assert_ne!(
        q1, 0,
        "the un-scissored (visible) query must report samples passed"
    );
    assert_eq!(
        q2, 0,
        "the zero-area-scissor query must report NO samples passed"
    );

    // Host read + PNG. The visible draw is GREEN; the scissored RED never reaches the framebuffer.
    let cap = exec.captured();
    let px = cap
        .rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor");
    write_png(
        &demo_png_dir().join("gl_occlusion_query.png"),
        W,
        H,
        &bgra_to_rgba(px),
    );
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;

    // Center is the visible GREEN quad. BGRA order from rgba8_texture: GREEN (0,255,0) -> (0,255,0).
    let i = ((32 * W + 32) * 4) as usize;
    let (b, g, r, a) = (px[i], px[i + 1], px[i + 2], px[i + 3]);
    assert!(
        near(r, 0) && near(g, 255) && near(b, 0) && a == 255,
        "center must be the VISIBLE green quad, got RGBA ({r},{g},{b},{a})"
    );

    // The scissored RED draw must be nowhere — a shim that ignored the scissor would paint it over the green.
    let red = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 255) && near(t[1], 0) && near(t[0], 0))
        .count();
    assert_eq!(
        red, 0,
        "the fully-scissored RED draw must NOT reach the framebuffer, found {red} red texels"
    );
    let green = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 0) && near(t[1], 255) && near(t[0], 0))
        .count();
    assert!(
        green > (W * H / 2) as usize,
        "the visible green quad must fill the frame, got {green} texels"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
