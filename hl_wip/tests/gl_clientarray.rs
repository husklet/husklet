//! REAL GL CLIENT-SIDE VERTEX ARRAYS — the milestone for running real client-array GL apps (weston-simple-egl,
//! legacy/immediate-ish GL, several toolkits): a real EGL + GLES2 offscreen program whose vertex data lives
//! in CLIENT memory (a stack array) with NO vertex buffer object bound, so `glVertexAttribPointer` is handed
//! a client pointer and buffer 0 is bound. A second test additionally uses `glDrawElements` with a CLIENT
//! index array (no element-array-buffer bound).
//!
//! Before the client-array lowering, the GL shim's frame builder assumed a bound VBO: it emitted a render
//! pipeline that needed vertex buffer slot 0 to be set but never set it, so the WgpuExecutor REJECTED the
//! draw ("requires vertex buffer 0 to be set") and nothing rasterized. Now the shim captures the client
//! array(s) at draw time into transient per-draw vertex/index buffers (uploaded via the same
//! CreateBuffer/WriteBuffer path a real VBO uses) and binds them, so the geometry genuinely rasterizes on
//! lavapipe (software Vulkan) and `glReadPixels` reads it back over the socket.
//!
//! Both tests assert a COVERED interior pixel is the geometry color and an uncovered corner is the clear
//! color — from BOTH the guest's own `glReadPixels` and an independent host-side read of the same target.

use std::process::Command;

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

const W: u32 = 64;
const H: u32 = 64;

/// Compile `csrc/<name>.c` against the staged GL shim, run it on the software-Vulkan WgpuExecutor, and
/// assert the guest printed `ok_marker` and a green-center / red-corner target was rasterized host-side.
fn run_client_array_program(name: &str, ok_marker: &str) {
    let gl_dir = staged_dir("gl");
    assert!(
        gl_dir.join("libEGL.so.1").exists(),
        "staged libEGL.so.1 missing at {gl_dir:?} — build hl_wip-gl's shim first"
    );
    assert!(gl_dir.join("libGLESv2.so.2").exists(), "staged libGLESv2.so.2 missing at {gl_dir:?}");

    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-realsw-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join(name);

    let compile = Command::new("gcc")
        .arg(format!("{manifest}/csrc/{name}.c"))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build {name}.c:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exec = WgpuExecutorServer::start(name);
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
        .expect("spawn client-array program");

    let stdout = String::from_utf8_lossy(&run.stdout);
    let stderr = String::from_utf8_lossy(&run.stderr);
    eprintln!("--- {name} stdout ---\n{stdout}\n--- {name} stderr ---\n{stderr}");

    assert!(
        run.status.success(),
        "{name} exited non-zero (code {:?}) — the client-array geometry did not rasterize as expected",
        run.status.code()
    );
    assert!(
        stdout.contains(ok_marker),
        "the client-array app drew a green triangle/quad over a red clear and read it back over the socket"
    );
    assert!(exec.submit_count() > 0, "guest submitted batches to the host executor");

    // ---- Independently assert the SAME rasterized target off the host executor -------------------
    // The GL default target is Bgra8Unorm; read_texture returns native (B,G,R,A) order. Green geometry
    // (0,1,0) → BGRA (0,255,0,255); the red clear (1,0,0) → BGRA (0,0,255,255).
    let cap = exec.captured();
    let px = cap.rgba8_texture(W, H).unwrap_or_else(|| {
        panic!(
            "no {W}x{H} render target captured off the host executor; captured texture sizes: {:?}",
            cap.textures.values().map(|v| v.len()).collect::<Vec<_>>()
        )
    });
    let near = |a: u8, b: u8| (a as i32 - b as i32).abs() <= 2;

    let ci = (((H / 2) * W + (W / 2)) * 4) as usize; // covered center → green
    assert!(
        near(px[ci], 0) && near(px[ci + 1], 255) && near(px[ci + 2], 0) && px[ci + 3] == 255,
        "host-side render target center (BGRA) should be the GREEN geometry, got {:?}",
        &px[ci..ci + 4]
    );
    let corner = &px[0..4]; // uncovered corner → red clear
    assert!(
        near(corner[0], 0) && near(corner[1], 0) && near(corner[2], 255) && corner[3] == 255,
        "host-side render target corner (BGRA) should be the RED clear, got {corner:?}"
    );

    let green = px.chunks_exact(4).filter(|t| near(t[0], 0) && near(t[1], 255) && near(t[2], 0)).count();
    let red = px.chunks_exact(4).filter(|t| near(t[0], 0) && near(t[1], 0) && near(t[2], 255)).count();
    assert!(
        green > 0 && red > 0,
        "both geometry ({green}) and clear ({red}) pixels present — client-array geometry truly rasterized"
    );

    let _ = std::fs::remove_dir_all(&out_dir);
}

/// `glDrawArrays` from a CLIENT-side vertex array, no VBO bound.
#[test]
fn real_gles2_client_vertex_array_rasterizes_on_lavapipe() {
    run_client_array_program("gl_clientarray", "GL_CLIENTARRAY_OK");
}

/// `glDrawElements` from a CLIENT-side vertex array AND a CLIENT-side index array, no buffers bound.
#[test]
fn real_gles2_client_index_array_rasterizes_on_lavapipe() {
    run_client_array_program("gl_clientindex", "GL_CLIENTINDEX_OK");
}
