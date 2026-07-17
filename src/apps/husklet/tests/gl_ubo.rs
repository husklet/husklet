//! REAL GL UNIFORM BLOCK (UBO) — a real EGL + GLES3 offscreen program whose per-draw transform lives in a
//! std140 uniform BLOCK, set the UBO way (`glBufferData` + `glBindBufferBase`, NOT `glUniform*`), rasterized
//! on lavapipe. The regression guard for GskGpu/GTK4's "presents but blank" bug.
//!
//! `../../surface/hl-gl/tests/fixtures/gl_ubo.c` is a header-free real GLES3 app (the EGL/GLES3 ABI is self-declared). It draws a full-NDC
//! quad transformed by an MVP that lives in `layout(std140, binding = 0) uniform Block { mat4 mvp; vec4
//! uColor; };`. The block's bytes are uploaded into a buffer and bound with `glBindBufferBase(GL_UNIFORM_
//! BUFFER, 0, ubo)` — the shim MUST route THAT buffer's bytes to the shader's IR binding 0. The MVP squeezes
//! the quad into the RIGHT HALF of the frame and `uColor` paints it RED; so a correct route yields RED on the
//! right, BLUE (the clear) on the left.
//!
//! Why this FAILS before the routing fix: the shim reflected the block into the default-uniform layout that
//! `glUniform*` fills — but this app never calls `glUniform*`, so those bytes stay ALL ZEROS. `mvp` is then
//! the zero matrix, every vertex collapses to `gl_Position = 0` (w = 0, degenerate), and NOTHING rasterizes:
//! the whole frame stays the BLUE clear. The fix snapshots the `glBindBufferBase`d buffer's std140 bytes per
//! draw and binds them VERBATIM at binding 0, so the real `mvp`/`uColor` reach the shader. We assert the
//! transformed RED geometry from BOTH the guest's own `glReadPixels` AND an independent host-side read.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn real_gles3_std140_uniform_block_transform_rasterizes_on_lavapipe() {
    let gl_dir = staged_dir("gl");
    assert!(
        gl_dir.join("libEGL.so.1").exists(),
        "staged libEGL.so.1 missing at {gl_dir:?} — build hl-gl's shim first"
    );
    assert!(
        gl_dir.join("libGLESv2.so.2").exists(),
        "staged libGLESv2.so.2 missing at {gl_dir:?}"
    );

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-glubo-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_ubo");

    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_ubo.c"
        ))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the GLES3 UBO program:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exec = WgpuExecutorServer::start("glubo");
    let adapter = exec.adapter_name();
    eprintln!("host wgpu adapter: {adapter}");
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "the geometry must rasterize on the software Vulkan device, got adapter {adapter:?}"
    );

    let run = Command::new(&bin)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("EGL_PLATFORM", "surfaceless")
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_GL_SURFACE_W", W.to_string())
        .env("HL_GL_SURFACE_H", H.to_string())
        .output()
        .expect("spawn gl_ubo");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- gl_ubo stdout ---\n{stdout}\n--- gl_ubo stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "GLES3 UBO program exited non-zero (code {:?}) — the std140 block's mvp/uColor did not reach the \
         shader (blank frame: mvp = 0 collapses the geometry)",
        run.status.code()
    );

    // ---- ASSERT the guest observed the transformed geometry over the socket ------------------------
    assert!(
        stdout.contains("GL_UBO_OK"),
        "the GLES3 app read RED on the right half + BLUE on the left back over the socket (the block's \
         transform + color reached the shader)"
    );
    assert!(
        exec.submit_count() > 0,
        "guest submitted batches to the host executor"
    );

    // ---- Independently assert the SAME rasterized target off the host executor ---------------------
    // The GL default target is Bgra8Unorm; read_texture returns native (B,G,R,A) order. RED (255,0,0,255)
    // → BGRA (0,0,255,255); the BLUE clear (0,0,255,255) → BGRA (255,0,0,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no {W}x{H} render target captured off the host executor for the GL UBO frame; captured \
             texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    });
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 4;

    // Right half (x=48, y=32) → the transformed RED quad. BGRA (0,0,255,255).
    let hi = ((32 * W + 48) * 4) as usize;
    let (hb, hg, hr, ha) = (px[hi], px[hi + 1], px[hi + 2], px[hi + 3]);
    assert!(
        near(hr, 255) && near(hg, 0) && near(hb, 0) && ha == 255,
        "host-side render target right-half (BGRA) should be the transformed RED fill BGRA (0,0,255,255), \
         got ({hb},{hg},{hr},{ha}) — the block's mvp/uColor did not reach the shader"
    );

    // Left half (x=16, y=32) → outside the transformed quad, the BLUE clear. BGRA (255,0,0,255).
    let lo = ((32 * W + 16) * 4) as usize;
    let (lb, lg, lr, la) = (px[lo], px[lo + 1], px[lo + 2], px[lo + 3]);
    assert!(
        near(lr, 0) && near(lg, 0) && near(lb, 255) && la == 255,
        "host-side render target left-half (BGRA) should be the BLUE clear BGRA (255,0,0,255), got \
         ({lb},{lg},{lr},{la}) — the geometry was NOT confined to the transformed right half"
    );

    // The transform must confine RED to roughly HALF the frame — proof the geometry was actually moved
    // (not full-screen from an identity/garbage matrix, not empty from a zero matrix).
    let red = px
        .chunks_exact(4)
        .filter(|t| near(t[2], 255) && near(t[1], 0) && near(t[0], 0) && t[3] == 255)
        .count();
    let total = (W * H) as usize;
    assert!(
        red > total / 4 && red < (total * 3) / 4,
        "the transformed RED quad must cover roughly half the frame (proof of a real MVP), got {red}/{total}"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}
