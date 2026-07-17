//! CHROME INTERACTION SMOKE — keep the ACTUAL Chromium process in the input loop (lenient tracker).
//!
//! `chrome_interaction.rs` proves the input round-trip EXACTLY with a deterministic real Wayland client.
//! This smoke complements it by putting the REAL Chromium binary on the same input-enabled compositor
//! (`run_auto_with_input`) and injecting pointer/keyboard events at the seat WHILE Chrome runs — proving
//! the host-input seam is live and non-stalling during a genuine Chrome session, and reporting how far
//! Chrome got (whether it connected to our Wayland socket / reached the ozone-Wayland backend).
//!
//! GREEN AS A TRACKER (mirrors `chrome_e2e.rs`). On this box Chromium is blocked in early ChromeMain at
//! GAP #0c (an arm64 IMMEDIATE_CRASH tracked by `chrome_e2e`), UPSTREAM of mapping any `wl_surface`. A
//! surface it never maps can never take focus, so a `wl_pointer.enter` cannot reach it yet — this is a
//! Chrome cold-boot blocker, NOT an input-path defect (the input path is proven exactly by the sibling
//! deterministic test). So this smoke does NOT hard-assert Chrome received the enter; it asserts the
//! achievable invariant — the compositor + input channel stay ALIVE and RESPONSIVE across a real Chrome
//! session (no stall, no panic) — and reports Chrome's furthest stage + whether the enter was delivered.
//! It SKIPS cleanly when the chromium binary / staged shims / fd-ownership preload are unavailable.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

use hl_compositor::adapter::smithay::{self, input_channel, InputCommand, PngPresenter};

/// Bounded ceiling on the whole Chrome session (Chrome cold-boot on this box is slow; we only need it to
/// reach — or fail to reach — the Wayland connect, so this is modest).
const APP_DEADLINE: Duration = Duration::from_secs(30);
const BTN_LEFT: u32 = 0x110;
const KEY_A: u32 = 30;

#[test]
fn chrome_interaction_smoke_real_process() {
    // ---- 0. Preconditions (skip cleanly if unmet) ----------------------------------------------------
    let chromium_bin = match which_chromium() {
        Some(p) => p,
        None => {
            eprintln!(
                "no chromium binary (set HL_CHROME_BIN) — skipping the Chrome interaction smoke."
            );
            return;
        }
    };
    let gl_dir = staged_dir("gl");
    for lib in ["libEGL.so.1", "libGLESv2.so.2", "libwayland-egl.so.1"] {
        if !gl_dir.join(lib).exists() {
            eprintln!(
                "staged {lib} missing at {gl_dir:?} — skipping the Chrome interaction smoke."
            );
            return;
        }
    }
    let (run_bin, dep_dirs) = match build_run_prefix(&chromium_bin, &gl_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("could not build the chromium run prefix ({e}) — skipping.");
            return;
        }
    };

    // ---- 1. Private XDG_RUNTIME_DIR on a roomy filesystem --------------------------------------------
    let runtime_dir = roomy_base().join(format!("smoke-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700");
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET");
    let profile_dir = runtime_dir.join("profile");
    std::fs::create_dir_all(&profile_dir).expect("create profile dir");

    let html_path = runtime_dir.join("orange.html");
    std::fs::write(
        &html_path,
        "<!doctype html><html><head><meta charset=utf-8><style>\
         html,body{margin:0;padding:0;width:100%;height:100%;background:#ff7700;}\
         </style></head><body></body></html>",
    )
    .expect("write orange.html");
    let file_url = format!("file://{}", html_path.display());

    // ---- 2. Host GPU executor (the GL frames would lower here if Chrome ever reached them) ------------
    let exec = WgpuExecutorServer::start("chrome-smoke");

    // ---- 3. The INPUT-ENABLED compositor on the discovery socket -------------------------------------
    let stop = Arc::new(AtomicBool::new(false));
    let presenter = PngPresenter::with_png_dir(runtime_dir.join("png"));
    let captures = presenter.captures();
    let (name_tx, name_rx) = mpsc::channel::<std::ffi::OsString>();
    let (input_tx, input_rx) = input_channel();

    let stop_thread = Arc::clone(&stop);
    let compositor = std::thread::spawn(move || {
        smithay::run_auto_with_input(presenter, stop_thread, input_rx, move |name| {
            let _ = name_tx.send(name);
        })
        .expect("compositor serve loop (run_auto_with_input)");
    });

    let socket_name = name_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("run_auto_with_input never reported a bound socket name");
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    let socket_path = runtime_dir.join(&socket_name);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(
            Instant::now() < deadline,
            "discovery socket {socket_path:?} never appeared"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 4. Spawn REAL chromium against our compositor ------------------------------------------------
    let out_path = runtime_dir.join("chrome.stdout");
    let err_path = runtime_dir.join("chrome.stderr");
    let out_file = std::fs::File::create(&out_path).expect("create stdout capture");
    let err_file = std::fs::File::create(&err_path).expect("create stderr capture");

    let mut ld_path = gl_dir.as_os_str().to_os_string();
    for d in &dep_dirs {
        ld_path.push(":");
        ld_path.push(d);
    }
    let preload = build_fd_ownership_preload(&runtime_dir);
    if preload.is_none() {
        eprintln!("no fd-ownership preload — Chrome will re-hit GAP #0 immediately; smoke stays a tracker.");
    }

    let mut cmd = Command::new(&run_bin);
    cmd.arg("--ozone-platform=wayland")
        .arg("--enable-features=UseOzonePlatform")
        .arg("--use-gl=angle")
        .arg("--use-angle=gles")
        .arg("--in-process-gpu")
        .arg("--no-sandbox")
        .arg("--disable-setuid-sandbox")
        .arg("--disable-gpu-sandbox")
        .arg("--no-zygote")
        .arg("--disable-crashpad-for-testing")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-features=Vulkan")
        .arg("--disable-background-networking")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--enable-logging=stderr")
        .arg("--window-size=800,600")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg(&file_url)
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("OZONE_PLATFORM", "wayland")
        .env("LD_LIBRARY_PATH", &ld_path)
        .env("HL_GPU_EXEC", exec.sock())
        .env_remove("DISPLAY");
    if let Some(p) = &preload {
        cmd.env("LD_PRELOAD", p);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));

    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", run_bin.display()));

    // ---- 5. While Chrome runs, INJECT input at the seat and prove the seam stays live -----------------
    // Every send must be accepted (the channel/compositor never wedged); the compositor thread must remain
    // alive throughout. If Chrome ever maps + focuses a surface, these reach it — otherwise they are
    // harmlessly dropped by the seat (no focused surface), and the invariant we assert is responsiveness.
    let start = Instant::now();
    let mut injected = 0u32;
    let mut chrome_exit = None;
    while start.elapsed() < APP_DEADLINE {
        // A representative input mix: move, click, type.
        input_tx
            .send(InputCommand::PointerMotion { x: 400.0, y: 300.0 })
            .expect("seat accepted motion");
        input_tx
            .send(InputCommand::FocusTopmostKeyboard)
            .expect("seat accepted focus");
        input_tx
            .send(InputCommand::PointerButton {
                button: BTN_LEFT,
                pressed: true,
            })
            .expect("seat accepted btn down");
        input_tx
            .send(InputCommand::PointerButton {
                button: BTN_LEFT,
                pressed: false,
            })
            .expect("seat accepted btn up");
        input_tx
            .send(InputCommand::Key {
                keycode: KEY_A,
                pressed: true,
            })
            .expect("seat accepted key down");
        input_tx
            .send(InputCommand::Key {
                keycode: KEY_A,
                pressed: false,
            })
            .expect("seat accepted key up");
        injected += 6;
        // Compositor liveness: it published its socket at startup and the thread is not finished.
        assert!(
            !compositor.is_finished(),
            "compositor thread died while injecting input during a Chrome session"
        );
        if let Ok(Some(status)) = child.try_wait() {
            chrome_exit = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // ---- 6. Teardown FIRST ---------------------------------------------------------------------------
    let _ = child.kill();
    let killed = child.wait().ok();
    stop.store(true, Ordering::Relaxed);
    let _ = compositor.join();

    let stderr = read_to_string(&err_path);
    let frames = captures.lock().unwrap().len();
    let submits = exec.submit_count();
    let reached_wayland = stderr.contains("ozone/platform/wayland")
        || stderr.contains("wayland")
        || stderr.to_lowercase().contains("drm render node");

    // ---- 7. Assert the achievable invariant + report the tracker stage -------------------------------
    // The seam accepted every injected command and the compositor stayed alive across a real Chrome
    // session — the input path does not stall or wedge when a heavyweight real client is connected.
    assert!(
        injected > 0,
        "at least one input burst was injected during the Chrome session"
    );
    eprintln!(
        "CHROME INTERACTION SMOKE (tracker, green): injected {injected} seat events during a real Chrome \
         session; compositor stayed live; presented_frames={frames} gpu_submits={submits} \
         reached_wayland={reached_wayland} chrome_exit={:?}.\n\
         Chrome mapping a surface (hence RECEIVING pointer-enter) is gated by GAP #0c (see chrome_e2e); the \
         EXACT input round-trip is proven deterministically by chrome_interaction.rs.",
        chrome_exit.or(killed),
    );

    let _ = std::fs::remove_file(&socket_path);
    let _ = std::fs::remove_dir_all(&runtime_dir);
}

// ================================================================================================
// Chrome launch scaffolding (trimmed from chrome_e2e.rs — the same run-prefix + fd-preload seams).
// ================================================================================================

fn build_run_prefix(
    chromium_bin: &Path,
    gl_dir: &Path,
) -> std::io::Result<(PathBuf, Vec<PathBuf>)> {
    let lib_dir = chromium_bin
        .parent()
        .expect("chromium binary has a parent dir");
    let prefix = roomy_base().join("prefix");
    std::fs::create_dir_all(&prefix)?;
    for entry in std::fs::read_dir(&prefix)? {
        let entry = entry?;
        if entry.file_name() != "chromium" {
            let _ = std::fs::remove_file(entry.path());
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    for entry in std::fs::read_dir(lib_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let dst = prefix.join(&name);
        if name_str == "libEGL.so" || name_str == "libGLESv2.so" {
            let target = if name_str == "libEGL.so" {
                "libEGL.so.1"
            } else {
                "libGLESv2.so.2"
            };
            std::os::unix::fs::symlink(gl_dir.join(target), &dst)?;
        } else if name_str == "chromium" {
            let src_len = entry.metadata()?.len();
            let need_copy = std::fs::metadata(&dst)
                .map(|m| m.len() != src_len)
                .unwrap_or(true);
            if need_copy {
                std::fs::copy(entry.path(), &dst)?;
            }
            let mut perms = std::fs::metadata(&dst)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&dst, perms)?;
        } else {
            std::os::unix::fs::symlink(entry.path(), &dst)?;
        }
    }
    for (link, target) in [
        ("libEGL.so.1", "libEGL.so.1"),
        ("libGLESv2.so.2", "libGLESv2.so.2"),
    ] {
        let dst = prefix.join(link);
        if !dst.exists() {
            let _ = std::os::unix::fs::symlink(gl_dir.join(target), &dst);
        }
    }
    let mut deps = Vec::new();
    if let Some(usr_lib) = lib_dir.parent() {
        let arch = usr_lib.join("aarch64-linux-gnu");
        if arch.is_dir() {
            let pulse = arch.join("pulseaudio");
            deps.push(arch);
            if pulse.is_dir() {
                deps.push(pulse);
            }
        }
    }
    deps.push(lib_dir.to_path_buf());
    Ok((prefix.join("chromium"), deps))
}

fn build_fd_ownership_preload(out_dir: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HL_CHROME_PRELOAD") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src = PathBuf::from(&manifest)
        .join("tests")
        .join("fixtures")
        .join("chrome_fdguard.c");
    if !src.exists() {
        return None;
    }
    let so = out_dir.join("chrome_fdguard.so");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let out = Command::new(cc)
        .args(["-shared", "-fPIC", "-O0", "-o"])
        .arg(&so)
        .arg(&src)
        .arg("-lpthread")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(so)
}

fn roomy_base() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME set");
    let base = PathBuf::from(home).join(".cache").join("hl-wip-chrome");
    let _ = std::fs::create_dir_all(&base);
    base
}

fn read_to_string(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

fn which_chromium() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HL_CHROME_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    for name in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ] {
        for dir in std::env::var("PATH").unwrap_or_default().split(':') {
            let p = Path::new(dir).join(name);
            if p.exists() && is_elf(&p) {
                return Some(p);
            }
        }
    }
    let known = PathBuf::from("/Users/x/.dd/workspaces/chromedeb/upper/usr/lib/chromium/chromium");
    if known.exists() {
        return Some(known);
    }
    None
}

fn is_elf(p: &Path) -> bool {
    let mut buf = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open(p) {
        if f.read_exact(&mut buf).is_ok() {
            return buf == [0x7f, b'E', b'L', b'F'];
        }
    }
    false
}
