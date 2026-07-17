//! REAL GL PIXEL-PACK-BUFFER (PBO) READBACK — a real EGL + GLES3 program that reads the rendered frame back
//! into a `GL_PIXEL_PACK_BUFFER` via `glReadPixels` (offset 0, a NULL client pointer), then maps the PBO
//! with `glMapBufferRange` and asserts the pixels are exact. This is the regression guard for the shim's
//! PBO pack path: `glReadPixels` with a bound `GL_PIXEL_PACK_BUFFER` must write the packed pixels into the
//! buffer's storage at the byte offset (see `hl-gl/shim/egl/src/driver.rs::glReadPixels`), NOT to the
//! `pixels`-as-pointer (which for offset 0 is NULL → nothing written). Asserted from the guest's mapped
//! read AND an independent host read; the frame is written to /tmp/hl-demo/gl_pbo_readback.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles3_pbo_readback_maps_exact_pixels_on_lavapipe() {
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
    let out_dir = std::env::temp_dir().join(format!("hl-demo-glpbo-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_pbo_readback");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_pbo_readback.c"
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

    let exec = WgpuExecutorServer::start("glpbo");
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
        .expect("spawn gl_pbo_readback");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_pbo_readback stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "guest exited non-zero (code {:?})",
        run.status.code()
    );
    assert!(
        stdout.contains("GL_PBO_OK"),
        "guest asserted the mapped PBO held the exact rendered GREEN frame"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // Host read + PNG. The frame is solid GREEN. BGRA order: GREEN (0,255,0) -> (0,255,0).
    let cap = exec.captured();
    let px = cap
        .rgba8_texture(W, H)
        .expect("no 64x64 target captured off the host executor");
    write_png(
        &demo_png_dir().join("gl_pbo_readback.png"),
        W,
        H,
        &bgra_to_rgba(px),
    );
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;

    let i = ((32 * W + 32) * 4) as usize;
    let (b, g, r, a) = (px[i], px[i + 1], px[i + 2], px[i + 3]);
    assert!(
        near(r, 0) && near(g, 255) && near(b, 0) && a == 255,
        "center must be the rendered GREEN, got RGBA ({r},{g},{b},{a})"
    );
    let green = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 0) && near(t[1], 255) && near(t[0], 0))
        .count();
    assert_eq!(
        green,
        (W * H) as usize,
        "the whole frame must be GREEN, got {green}/{}",
        W * H
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
