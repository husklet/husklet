//! EXACT-PIXEL PRESENT CAPSTONE — a real GPU guest renders a KNOWN quadrant pattern to a real WINDOW
//! surface, presents through the ENTIRE hl stack, and this test asserts the composited output pixels
//! EXACTLY (not a percentage). This is strictly stronger than `weston_simple_egl_e2e` / `vkcube_e2e`, which
//! only check %-orange / non-blank / luminance-spread of an ANIMATED app.
//!
//! The full loop (every piece already committed; this test is the composition root wiring them):
//!
//!   ../../surface/hl-gl/tests/fixtures/gl_present_pattern.c  (real GLES2, our staged libEGL/libGLESv2 via LD_LIBRARY_PATH)
//!     -> a real `wl_egl_window` ABI value `{version=3,64,64,…,surface=NULL}` -> self-owned toplevel
//!        -> with `$WAYLAND_DISPLAY` set, our libEGL stands up its OWN `wl_shm` xdg_toplevel
//!     -> the guest draws four flat-colored quads tiling NDC (deterministic; no animation)
//!     -> each GL frame lowers to hl_gpu IR shipped over `$HL_GPU_EXEC`
//!     -> host `WgpuExecutor` on lavapipe (software Vulkan) rasterizes the quads
//!     -> `eglSwapBuffers` reads the frame back, flips GL-bottom-left -> wayland-top-left, packs
//!        `WL_SHM_FORMAT_XRGB8888`, and commits it onto the self-owned toplevel
//!     -> our compositor (`hl_compositor::adapter::smithay::run_auto`, a real Smithay server on a temp
//!        `$WAYLAND_DISPLAY`) receives the commit, decodes the shm pixels, composes the scene
//!     -> `PngPresenter` captures the presented surface as a real frame (+ a viewable `.png` in /tmp/hl-demo).
//!
//! THE KNOWN PATTERN (as it appears in the COMPOSITED frame, top-left origin):
//!     top-left = RED (255,0,0,255)      top-right = GREEN (0,255,0,255)
//!     bottom-left = BLUE (0,0,255,255)  bottom-right = YELLOW (255,255,0,255)
//!
//! EXACTNESS: the whole path is integer-exact — the guest renders flat solid colors (no blend, no gradient),
//! lavapipe reads them back verbatim, `rgba_to_xrgb8888` reorders bytes losslessly, and the compositor's
//! `read_shm_rgba` (Xrgb8888) inverts that reorder exactly. So we assert `== [r,g,b,255]`, NOT a tolerance.
//! If a real color-space / Y-flip / premultiply discrepancy existed it would show here as a wrong exact
//! value — a finding, not something to paper over with a loose threshold.
//!
//! INPUT ROUND-TRIP: documented skip (see the bottom of this file). The guest presents through the shim's
//! self-owned wayland client and has NO `wl_seat`/input plumbing of its own to react to injected events.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

use hl_compositor::adapter::smithay::{self, CapturedFrame, PngPresenter};

const W: i32 = 64;
const H: i32 = 64;
/// A few identical presents is plenty to prove the loop is live and to pick a fully-covered frame.
const TARGET_FRAMES: usize = 3;
/// Hard ceiling on how long the guest may run before we kill it (never hang).
const APP_DEADLINE: Duration = Duration::from_secs(20);

/// The known composited pattern: (sample x, y, expected RGBA). Interior samples sit deep inside each
/// quadrant; the four corners pin the extreme pixels. Every value is exact.
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const YELLOW: [u8; 4] = [255, 255, 0, 255];

#[test]
fn known_pattern_composites_exact_through_the_full_stack() {
    // ---- 0. Preconditions: gcc + our staged GL shims ------------------------------------------------
    let gl_dir = staged_dir("gl");
    for lib in ["libEGL.so.1", "libGLESv2.so.2"] {
        assert!(
            gl_dir.join(lib).exists(),
            "staged {lib} missing at {gl_dir:?} — build hl-gl's shim first (a `cargo test` in hl \
             stages it)"
        );
    }
    if Command::new("gcc").arg("--version").output().is_err() {
        eprintln!("gcc not found — skipping the exact-present capstone.");
        return;
    }

    // ---- 1. Compile the KNOWN-pattern GLES2 guest ---------------------------------------------------
    let manifest = env!("CARGO_MANIFEST_DIR");
    let out_dir = std::env::temp_dir().join(format!("hl-present-exact-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bin = out_dir.join("gl_present_pattern");
    let compile = Command::new("gcc")
        .arg(format!(
            "{manifest}/../../surface/hl-gl/tests/fixtures/gl_present_pattern.c"
        ))
        .arg(format!("-L{}", gl_dir.display()))
        .args(["-lEGL", "-lGLESv2"])
        .arg("-o")
        .arg(&bin)
        .output()
        .expect("spawn gcc");
    assert!(
        compile.status.success(),
        "gcc failed to build the pattern guest:\n{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    // ---- 2. A private, 0700 XDG_RUNTIME_DIR so the discovery socket lives in isolation --------------
    use std::os::unix::fs::PermissionsExt;
    let runtime_dir =
        std::env::temp_dir().join(format!("hl-present-exact-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (a single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET");

    // The composited PNGs land in the shared demo dir so a human can eyeball the exact pattern.
    let png_dir = common::demo_png_dir();

    // ---- 3. The host GPU executor: WgpuExecutor on lavapipe -----------------------------------------
    let exec = WgpuExecutorServer::start("present-exact");
    let adapter = exec.adapter_name();
    eprintln!("host wgpu adapter: {adapter}");
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "the guest's GL frames must rasterize on the software Vulkan device, got adapter {adapter:?}"
    );

    // ---- 4. Our compositor on the standard discovery socket, in a background thread -----------------
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

    let socket_name = name_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("run_auto never reported a bound socket name");
    let socket_path = runtime_dir.join(&socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(
            Instant::now() < deadline,
            "discovery socket {socket_path:?} never appeared"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 5. Spawn the guest pointed at our shims + compositor + executor ----------------------------
    let out_path = out_dir.join("guest.stdout");
    let err_path = out_dir.join("guest.stderr");
    let out_file = std::fs::File::create(&out_path).expect("create stdout capture");
    let err_file = std::fs::File::create(&err_path).expect("create stderr capture");
    let mut child = Command::new(&bin)
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("LD_LIBRARY_PATH", &gl_dir)
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_SHIM_DEBUG", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(out_file))
        .stderr(std::process::Stdio::from(err_file))
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", bin.display()));

    // ---- 6. Let it render: poll the presenter until we have a few frames or hit the deadline --------
    let start = Instant::now();
    let mut frames: Vec<CapturedFrame> = Vec::new();
    let mut app_exited: Option<std::process::ExitStatus> = None;
    while start.elapsed() < APP_DEADLINE {
        frames = captures.lock().unwrap().clone();
        if frames.len() >= TARGET_FRAMES {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            app_exited = Some(status);
            frames = captures.lock().unwrap().clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // ---- 7. Teardown FIRST (never leave the guest or the compositor thread running) -----------------
    let _ = child.kill();
    let killed = child.wait().ok();
    stop.store(true, Ordering::Relaxed);
    let _ = compositor.join();

    let stdout = read_to_string(&out_path);
    let stderr = read_to_string(&err_path);
    let submit_count = exec.submit_count();
    eprintln!(
        "--- guest stdout ---\n{stdout}\n--- guest stderr ---\n{stderr}\n\
         --- host: adapter={adapter} gpu_submits={submit_count} presented_frames={} app_exited={:?} ---",
        frames.len(),
        app_exited.or(killed),
    );

    // ---- 8. Diagnose precisely if the pixels never arrived ------------------------------------------
    if frames.is_empty() {
        panic!(
            "CAPSTONE STOPPED before any composited frame.\n\
             * host GPU executor submits (guest lowered GL IR over $HL_GPU_EXEC): {submit_count}\n\
               (0 => the guest never reached eglSwapBuffers / never bound OUR libEGL — check LD_LIBRARY_PATH \
                and the guest stderr for a loader/EGL-init failure)\n\
             * compositor presented frames: 0\n\
               (>0 submits but 0 frames => the GL frame rasterized+read back but the self-owned wl_shm \
                toplevel present did not reach the compositor)\n\
             guest stdout:\n{stdout}\nguest stderr:\n{stderr}"
        );
    }
    assert!(
        submit_count > 0,
        "the guest produced composited frames but the host executor saw 0 GPU submits — the pixels did not \
         come from our GL lowering path"
    );

    // Exactly ONE presented surface => one real toplevel (the shim's self-owned surface for this guest).
    let mut surfaces: Vec<u32> = frames.iter().map(|f| f.surface.0).collect();
    surfaces.sort_unstable();
    surfaces.dedup();
    assert_eq!(
        surfaces.len(),
        1,
        "expected exactly one presented toplevel, saw surface ids {surfaces:?}"
    );

    // Assert on the LAST fully-formed frame (all frames are identical — the pattern is static).
    let frame = frames.last().expect("at least one captured frame").clone();
    assert_eq!(
        (frame.width, frame.height),
        (W, H),
        "the composited frame must be the {W}x{H} window, got {}x{}",
        frame.width,
        frame.height
    );

    // ---- 9. EXACT-PIXEL assertions: quadrant interiors + four corners -------------------------------
    // Interior samples deep inside each quadrant (1/4 and 3/4 of each axis).
    let checks: [(i32, i32, [u8; 4], &str); 8] = [
        (16, 16, RED, "top-left interior"),
        (48, 16, GREEN, "top-right interior"),
        (16, 48, BLUE, "bottom-left interior"),
        (48, 48, YELLOW, "bottom-right interior"),
        (0, 0, RED, "top-left corner"),
        (W - 1, 0, GREEN, "top-right corner"),
        (0, H - 1, BLUE, "bottom-left corner"),
        (W - 1, H - 1, YELLOW, "bottom-right corner"),
    ];
    let mut failures = Vec::new();
    for (x, y, want, label) in checks {
        let got = frame
            .pixel(x, y)
            .unwrap_or_else(|| panic!("pixel ({x},{y}) out of bounds"));
        eprintln!("  ({x:>2},{y:>2}) {label:<22} got {got:?} want {want:?}");
        if got != want {
            failures.push(format!("({x},{y}) {label}: got {got:?}, want {want:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "EXACT-PIXEL MISMATCH in the composited frame — a real present-path discrepancy (color-space / \
         Y-flip / channel-order / premultiply), NOT to be papered over:\n  {}",
        failures.join("\n  ")
    );

    // The whole frame must be exactly these four colors (no stray/blended pixels anywhere).
    let mut foreign = 0usize;
    for p in frame.rgba.chunks_exact(4) {
        let px = [p[0], p[1], p[2], p[3]];
        if px != RED && px != GREEN && px != BLUE && px != YELLOW {
            foreign += 1;
        }
    }
    assert_eq!(
        foreign, 0,
        "every composited pixel must be exactly one of the four quadrant colors — {foreign} pixel(s) were \
         some other value (a blend/gradient artifact would show here)"
    );

    // ---- 10. Confront the written PNG on disk -------------------------------------------------------
    let png = png_dir.join(format!("frame-{}.png", frame.serial));
    assert!(
        png.exists(),
        "a real PNG of the composited frame was written at {png:?}"
    );
    eprintln!(
        "CAPSTONE PASSED: known quadrant pattern composited EXACTLY through the full stack.\n\
         PNG: {}\n  frames: {}, gpu submits: {}, adapter: {adapter}",
        png.display(),
        frames.len(),
        submit_count,
    );

    let _ = std::fs::remove_dir_all(&out_dir);
    let _ = std::fs::remove_file(&socket_path);
}

/// Read a capture file to a String (empty if unreadable).
fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}
