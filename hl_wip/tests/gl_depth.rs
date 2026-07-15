//! REAL GL DEPTH TEST — a real EGL + GLES2 offscreen program proving the depth test occludes correctly,
//! rasterized on lavapipe. A NEAR GREEN quad (z=-0.5) over the left half is drawn FIRST, then a FAR RED
//! fullscreen quad (z=+0.5); with GL_LESS the far quad fails the depth test over the near geometry, so the
//! left stays GREEN and only the right becomes RED. This is the regression guard for the shim's depth
//! attachment: a pipeline built with a depth-stencil state MUST run in a pass carrying a matching depth
//! buffer (wgpu enforces this). Asserts from BOTH the guest glReadPixels and an independent host read, and
//! writes the readback to /tmp/hl-demo/gl_depth.png.

use std::process::Command;

mod common;
use common::wgpu::WgpuExecutorServer;
use common::{bgra_to_rgba, demo_png_dir, staged_dir, write_png};

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles2_depth_test_occludes_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(gl_dir.join("libEGL.so.1").exists(), "staged libEGL.so.1 missing at {gl_dir:?}");
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-demo-gldepth-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_depth");

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/gl_depth.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(compile.status.success(), "gcc failed:\n{}", String::from_utf8_lossy(&compile.stderr));

    let exec = WgpuExecutorServer::start("gldepth");
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
        .expect("spawn gl_depth");
    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_depth stdout ---\n{stdout}\n--- stderr ---\n{stderr}");

    assert!(run.status.success(), "guest exited non-zero (code {:?})", run.status.code());
    assert!(stdout.contains("GL_DEPTH_OK"), "guest asserted the depth occlusion over the socket");
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // Host read + PNG. BGRA: near GREEN (0,255,0) -> (0,255,0); far RED (255,0,0) -> (0,0,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).expect("no 64x64 target captured off the host executor");
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        (px[i], px[i + 1], px[i + 2], px[i + 3]) // (B,G,R,A)
    };

    write_png(&demo_png_dir().join("gl_depth.png"), W, H, &bgra_to_rgba(px));

    let (b, g, r, a) = at(16, 32); // left: near GREEN wins
    assert!(
        near(r, 0) && near(g, 255) && near(b, 0) && a == 255,
        "left half must be the NEAR GREEN quad (depth occlusion), got RGBA ({r},{g},{b},{a})"
    );
    let (b, g, r, a) = at(48, 32); // right: far RED only
    assert!(
        near(r, 255) && near(g, 0) && near(b, 0) && a == 255,
        "right half must be the FAR RED quad, got RGBA ({r},{g},{b},{a})"
    );

    // Both colors present in meaningful amounts — a genuine two-quad depth-resolved scene, not a full-frame
    // overwrite (which is exactly what a MISSING depth buffer would produce: all RED).
    let green = px.chunks_exact(4).filter(|t| near(t[2], 0) && near(t[1], 255) && near(t[0], 0)).count();
    let red = px.chunks_exact(4).filter(|t| near(t[2], 255) && near(t[1], 0) && near(t[0], 0)).count();
    assert!(
        green > (W * H / 8) as usize && red > (W * H / 8) as usize,
        "both NEAR ({green}) and FAR ({red}) regions must survive the depth test"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
