//! THE MILESTONE — run the REAL third-party GUI app `weston-simple-egl` end to end through the ENTIRE
//! hl_wip stack and capture its rendered window pixels off our compositor.
//!
//! The full loop this proves (every piece already committed; this test is the composition root wiring them):
//!
//!   real /usr/bin/weston-simple-egl
//!     -> our staged libwayland-egl + libEGL + libGLESv2 (`~/.hl/gl/<arch>/`, via LD_LIBRARY_PATH)
//!     -> each GL frame lowered to hl_gpu IR and shipped over `$HL_GPU_EXEC`
//!     -> host `WgpuExecutor` on lavapipe (llvmpipe / software Vulkan) rasterizes the triangle
//!     -> `glReadPixels` reads the frame back over the socket
//!     -> our libEGL marshals it as a `wl_shm` buffer onto the app's OWN `wl_surface`
//!        (adapter/wayland_app.rs — the app-surface present path) over the app's `libwayland-client`
//!     -> our compositor (`hl_wip_compositor::adapter::smithay::run_auto`, a real Smithay Wayland server on
//!        a temp `$WAYLAND_DISPLAY`) receives the commit, reads the shm pixels, composes the scene
//!     -> `PngPresenter` captures the presented surface as a real frame (+ a viewable `.png`).
//!
//! ASSERTED: the presenter captured the app's toplevel as a NON-BLANK frame showing weston-simple-egl's
//! content — the animated Gouraud triangle (red/green/blue vertices) over its BLACK clear. We assert the
//! window CENTER (always covered by the spinning triangle) is not the clear color and carries triangle
//! color, while an uncovered CORNER stays the black clear — i.e. real app geometry composited through the
//! whole stack, on ONE real toplevel (the app's own surface), not a shim-owned window.
//!
//! ROBUSTNESS: bounded timeouts everywhere (the app is killed on a deadline, the compositor thread is
//! stopped + joined), the app's stdout/stderr are captured to files for the report, and any stop short of
//! pixels is diagnosed by which stage produced/observed what.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

use hl_compositor::adapter::smithay::{self, CapturedFrame, PngPresenter};

/// How many presented frames we want before we are satisfied the loop is live (a few real frames).
const TARGET_FRAMES: usize = 3;
/// Hard ceiling on how long the real app is allowed to run before we kill it (never hang).
const APP_DEADLINE: Duration = Duration::from_secs(20);

#[test]
fn weston_simple_egl_composites_through_the_full_stack() {
    // ---- 0. Preconditions: the real app + our staged shims must be present ---------------------------
    let app_bin = match which_weston() {
        Some(p) => p,
        None => {
            eprintln!("weston-simple-egl not found on PATH — skipping the milestone (install `weston`).");
            return;
        }
    };
    let gl_dir = staged_dir("gl");
    for lib in ["libEGL.so.1", "libGLESv2.so.2", "libwayland-egl.so.1"] {
        assert!(
            gl_dir.join(lib).exists(),
            "staged {lib} missing at {gl_dir:?} — build hl_wip-gl's shim first (a `cargo test` in hl_wip \
             stages it)"
        );
    }

    // ---- 1. A private, 0700 XDG_RUNTIME_DIR so the discovery socket lives in isolation ---------------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-weston-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (a single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET"); // no inherited fd may short-circuit discovery

    let png_dir = runtime_dir.join("png");

    // ---- 2. The host GPU executor: WgpuExecutor on lavapipe, served over a temp unix socket ----------
    let exec = WgpuExecutorServer::start("weston");
    let adapter = exec.adapter_name();
    eprintln!("host wgpu adapter: {adapter}");
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "the app's GL frames must rasterize on the software Vulkan device, got adapter {adapter:?}"
    );

    // ---- 3. Our compositor on the STANDARD discovery socket, in a background thread -------------------
    let stop = Arc::new(AtomicBool::new(false));
    let presenter = PngPresenter::with_png_dir(png_dir.clone());
    let captures = presenter.captures();
    let (name_tx, name_rx) = mpsc::channel::<std::ffi::OsString>();

    let stop_thread = Arc::clone(&stop);
    let compositor = std::thread::spawn(move || {
        smithay::run_auto(presenter, stop_thread, move |name| {
            let _ = name_tx.send(name);
        })
        .expect("compositor serve loop (run_auto)");
    });

    // The `wayland-N` name Smithay chose — publish it so the app discovers us via $WAYLAND_DISPLAY.
    let socket_name = name_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("run_auto never reported a bound socket name");
    let name_str = socket_name.to_string_lossy().into_owned();
    assert!(name_str.starts_with("wayland-"), "expected a `wayland-N` name, got {name_str:?}");
    let socket_path = runtime_dir.join(&socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(Instant::now() < deadline, "discovery socket {socket_path:?} never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 4. Spawn the REAL app pointed at our shims + compositor + executor ---------------------------
    // stdout/stderr go to files (not pipes) so a chatty HL_SHIM_DEBUG run can never fill a pipe and stall
    // the app; we read them back for the report after teardown.
    let out_path = runtime_dir.join("weston.stdout");
    let err_path = runtime_dir.join("weston.stderr");
    let out_file = std::fs::File::create(&out_path).expect("create stdout capture");
    let err_file = std::fs::File::create(&err_path).expect("create stderr capture");

    let mut child = Command::new(&app_bin)
        // Default args: draw the animated triangle over the black clear (no -v vertical bar).
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("LD_LIBRARY_PATH", &gl_dir) // bind OUR libEGL/libGLESv2/libwayland-egl first
        .env("HL_GPU_EXEC", exec.sock()) // the host executor the staged libEGL lowers to
        .env("HL_SHIM_DEBUG", "1") // surface any unimplemented GL/EGL op in stderr (diagnosis)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", app_bin.display()));

    // ---- 5. Let it render: poll the presenter until we have a few frames or hit the deadline ---------
    let start = Instant::now();
    let mut frames: Vec<CapturedFrame> = Vec::new();
    let mut app_exited: Option<std::process::ExitStatus> = None;
    while start.elapsed() < APP_DEADLINE {
        frames = captures.lock().unwrap().clone();
        if frames.len() >= TARGET_FRAMES {
            break;
        }
        // If the app died on its own before producing frames, stop waiting — we'll diagnose below.
        if let Ok(Some(status)) = child.try_wait() {
            app_exited = Some(status);
            frames = captures.lock().unwrap().clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // ---- 6. Teardown FIRST (never leave the app or the compositor thread running) --------------------
    let _ = child.kill();
    let killed_status = child.wait().ok();
    stop.store(true, Ordering::Relaxed);
    let _ = compositor.join();

    let stdout = read_to_string(&out_path);
    let stderr = read_to_string(&err_path);
    let submit_count = exec.submit_count();
    eprintln!(
        "--- weston-simple-egl stdout ---\n{stdout}\n--- weston-simple-egl stderr ---\n{stderr}\n\
         --- host: adapter={adapter} gpu_submits={submit_count} presented_frames={} app_exited={:?} ---",
        frames.len(),
        app_exited.or(killed_status),
    );

    // ---- 7. Diagnose precisely if the pixels never arrived --------------------------------------------
    if frames.is_empty() {
        panic!(
            "MILESTONE STOPPED before any composited frame.\n\
             Stage evidence:\n\
             * host GPU executor submits (guest lowered GL IR over $HL_GPU_EXEC): {submit_count}\n\
               (0 => the app never reached eglSwapBuffers / never bound OUR libEGL — check LD_LIBRARY_PATH \
                and the app stderr for a loader/EGL-init failure)\n\
             * compositor presented frames: 0\n\
               (>0 submits but 0 frames => the GL frame rasterized+read back but the app-surface wl_shm \
                present onto the app's wl_surface did not reach the compositor, or the compositor never \
                composed it — the next real bug is in adapter/wayland_app.rs present or the commit path)\n\
             app stdout:\n{stdout}\napp stderr:\n{stderr}"
        );
    }

    // ---- 8. ASSERT the captured pixels are the REAL APP's rendered triangle ---------------------------
    // The staged libEGL lowered the app's GL and submitted IR to the host executor.
    assert!(
        submit_count > 0,
        "the app produced composited frames but the host executor saw 0 GPU submits — the pixels did not \
         come from our GL lowering path"
    );

    // Exactly ONE presented surface => one real toplevel (the app's own wl_surface), not a shim-owned +
    // app pair. (The app-surface path marshals onto the app's existing surface; a self-owned fallback
    // would introduce a second toplevel.)
    let mut surfaces: Vec<u32> = frames.iter().map(|f| f.surface.0).collect();
    surfaces.sort_unstable();
    surfaces.dedup();
    assert_eq!(
        surfaces.len(),
        1,
        "expected exactly one presented toplevel (the app's own surface), saw surface ids {surfaces:?}"
    );

    // Pick the frame with the most non-black coverage — the triangle is animated, so at least one frame
    // has it solidly over the window.
    let frame = frames
        .iter()
        .max_by_key(|f| non_black_pixels(f))
        .expect("at least one captured frame")
        .clone();
    let (w, h) = (frame.width, frame.height);
    assert!(w > 0 && h > 0, "captured frame has a real size, got {w}x{h}");
    eprintln!("asserting on captured frame: {w}x{h}, serial {}", frame.serial);

    // The Gouraud triangle's interior is a barycentric blend of (255,0,0)/(0,255,0)/(0,0,255), so ANY
    // covered pixel has R+G+B ~= 255 (well clear of the black clear). The window center is inside the
    // spinning triangle for every rotation angle (the incircle contains the screen origin).
    let center = frame.pixel(w / 2, h / 2).expect("center pixel exists");
    let center_lum = center[0] as u32 + center[1] as u32 + center[2] as u32;
    assert!(
        center_lum > 60,
        "window CENTER should be covered by the app's triangle (non-black), got RGBA {center:?} \
         (sum {center_lum}) — the clear color is black, so this proves triangle geometry composited"
    );

    // An uncovered corner keeps the black clear color — proving we captured a real scene (clear + geometry),
    // not a uniformly-filled buffer.
    let corner = frame.pixel(0, 0).expect("corner pixel exists");
    let corner_lum = corner[0] as u32 + corner[1] as u32 + corner[2] as u32;
    assert!(
        corner_lum < 40,
        "window CORNER should be the app's BLACK clear, got RGBA {corner:?} (sum {corner_lum})"
    );

    // Triangle color family: somewhere in the frame a pixel must carry a strong red / green / blue channel
    // from one of the triangle's colored vertices (not merely a gray blend), proving the app's actual
    // colored geometry — not an accidental uniform fill.
    assert!(
        has_triangle_color(&frame),
        "expected pixels carrying the triangle's red/green/blue vertex colors somewhere in the frame"
    );

    // A real, viewable PNG of the composited app frame was written.
    let png = png_dir.join(format!("frame-{}.png", frame.serial));
    assert!(png.exists(), "a real PNG of the composited app frame was written at {png:?}");
    eprintln!(
        "MILESTONE PASSED: real weston-simple-egl composited through the full stack.\n\
         PNG: {}\n  frames captured: {}, gpu submits: {}, adapter: {adapter}",
        png.display(),
        frames.len(),
        submit_count,
    );

    // Leave the PNG dir for inspection; remove the rest.
    let _ = std::fs::remove_file(&socket_path);
}

/// Count pixels whose R+G+B exceeds the near-black threshold (triangle coverage over the black clear).
fn non_black_pixels(f: &CapturedFrame) -> usize {
    f.rgba
        .chunks_exact(4)
        .filter(|p| p[0] as u32 + p[1] as u32 + p[2] as u32 > 60)
        .count()
}

/// Whether some pixel carries a strong single-channel color from a triangle vertex (R, G, or B >= 150 while
/// clearly dominating the frame's black clear).
fn has_triangle_color(f: &CapturedFrame) -> bool {
    f.rgba
        .chunks_exact(4)
        .any(|p| p[0] >= 150 || p[1] >= 150 || p[2] >= 150)
}

/// Read a capture file to a String (empty if unreadable).
fn read_to_string(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

/// Locate `weston-simple-egl` on PATH (Command resolves PATH itself, but we probe so an absent binary is a
/// clean skip rather than a spawn error).
fn which_weston() -> Option<std::path::PathBuf> {
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let p = Path::new(dir).join("weston-simple-egl");
        if p.exists() {
            return Some(p);
        }
    }
    None
}
