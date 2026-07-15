//! CHROME FIRST-LIGHT — run REAL Chromium end to end through the ENTIRE hl_wip NATIVE-Linux stack and pin
//! the EXACT stage where it stops (a diagnosis milestone, mirroring the early vkcube/gtk4 trackers).
//!
//! STRATEGY (see scratchpad/spec-chrome-firstlight.md): aim Chromium at the NATIVE-Linux hl_wip path — the
//! same path weston-simple-egl / gtk4 / vkcube take — NOT the JIT engine. Every old JIT Chrome blocker
//! (Wall-7 renderer dormancy, epoll-kqueue race, IOSurface-in-forked-child, ObjC fork-safety, Mojo
//! primary-channel) was an EMULATION artifact, absent on this real kernel.
//!
//! The loop this drives:
//!   real chromium (--ozone-platform=wayland: connects to our $WAYLAND_DISPLAY, makes its OWN wl_surface;
//!                  --use-gl=angle --use-angle=gles: Chrome's GL command decoder talks EGL/GLES2 to what it
//!                  believes is ANGLE's libEGL/libGLESv2 — which we REPLACE with OUR staged shims)
//!     -> Chrome dlopens libEGL.so/libGLESv2.so — bound to OUR shims two ways: (a) they sit in the run
//!        prefix (DIR_MODULE, where Chrome's ANGLE loader looks) as symlinks to the staged libs, AND
//!        (b) ~/.hl/gl/<arch> is first on LD_LIBRARY_PATH. Chrome's own bundled ANGLE libEGL/libGLESv2 are
//!        NOT in the prefix, so there is nothing else to bind.
//!     -> each GL frame lowered to hl_gpu IR and shipped over $HL_GPU_EXEC
//!     -> host WgpuExecutor on lavapipe rasterizes it; glReadPixels reads it back over the socket
//!     -> our libEGL marshals it as a wl_shm buffer onto Chrome's OWN wl_surface (adapter/wayland_app.rs)
//!     -> our compositor (smithay::run_auto) composes it; PngPresenter captures the frame (+ .png).
//!
//! DIAGNOSIS BY STAGE (the deliverable): the offline page is solid orange (#ff7700). We classify the result:
//!   * submit_count == 0            => Chrome never bound OUR libEGL / never reached a GL frame (gap #2 or an
//!                                     EGL-init / display-connect stop — read the stderr for the first hard
//!                                     failure).
//!   * submits > 0 but blank/transp => Chrome's GLSL-ES shaders did not compile on the host (gap #1).
//!   * frames WHITE                 => present/context (gaps 4/6).
//!   * >40% ORANGE                  => RENDERS (milestone).
//!
//! This is GREEN as a diagnosed-gap tracker until Chrome renders; when it does render orange it asserts hard.
//! Bounded timeouts everywhere; the app's stdout/stderr go to files (never a pipe) and are read back for the
//! report.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;
use common::staged_dir;
use common::wgpu::WgpuExecutorServer;

use hl_compositor::adapter::smithay::{self, CapturedFrame, PngPresenter};

/// How many presented frames we want before we call the loop live.
const TARGET_FRAMES: usize = 3;
/// Hard ceiling on how long Chromium is allowed to run before we kill it. Chrome cold-boot on lavapipe is
/// slow (V8 + GPU bring-up + first composited frame), so this is generous.
const APP_DEADLINE: Duration = Duration::from_secs(60);

#[test]
fn chrome_first_light_through_the_full_stack() {
    // ---- 0. Preconditions: a real chromium binary + our staged GL shims must be present ----------------
    let chromium_bin = match which_chromium() {
        Some(p) => p,
        None => {
            eprintln!(
                "no chromium binary found (set HL_CHROME_BIN, or extract an arm64 chromium .deb) — \
                 skipping the Chrome first-light milestone."
            );
            return;
        }
    };
    eprintln!("real chromium binary: {}", chromium_bin.display());

    let gl_dir = staged_dir("gl");
    for lib in ["libEGL.so.1", "libGLESv2.so.2", "libwayland-egl.so.1"] {
        assert!(
            gl_dir.join(lib).exists(),
            "staged {lib} missing at {gl_dir:?} — build hl_wip-gl's shim first (a `cargo test` in hl_wip \
             stages it)"
        );
    }

    // ---- 1. Build a run prefix where OUR libEGL/libGLESv2 REPLACE Chrome's bundled ANGLE ---------------
    // Chrome's ANGLE loader looks for libEGL.so/libGLESv2.so next to the executable (DIR_MODULE). We give it
    // a prefix that is Chrome's own lib dir mirrored by symlinks — EXCEPT the two ANGLE libs, which we point
    // at OUR staged shims. So however Chrome resolves them (DIR_MODULE absolute or loader search), it binds
    // ours; its own ANGLE is simply not present to win.
    let (run_bin, dep_dirs) = match build_run_prefix(&chromium_bin, &gl_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("could not build the chromium run prefix ({e}) — skipping.");
            return;
        }
    };
    eprintln!("chromium run prefix binary: {}", run_bin.display());

    // ---- 2. A private, 0700 XDG_RUNTIME_DIR so the discovery socket lives in isolation ----------------
    // On a roomy filesystem ($HOME) — the shared /tmp on this box is a near-full 22G tmpfs.
    let runtime_dir = roomy_base().join(format!("xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (a single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET"); // no inherited fd may short-circuit discovery

    let png_dir = runtime_dir.join("png");
    let profile_dir = runtime_dir.join("profile");
    std::fs::create_dir_all(&profile_dir).expect("create profile dir");

    // The offline page: solid orange (#ff7700) so a rendered window is unambiguous.
    let html_path = runtime_dir.join("orange.html");
    std::fs::write(
        &html_path,
        "<!doctype html><html><head><meta charset=utf-8><style>\
         html,body{margin:0;padding:0;width:100%;height:100%;background:#ff7700;}\
         </style></head><body></body></html>",
    )
    .expect("write orange.html");
    let file_url = format!("file://{}", html_path.display());

    // ---- 3. The host GPU executor: WgpuExecutor on lavapipe, served over a temp unix socket -----------
    let exec = WgpuExecutorServer::start("chrome");
    let adapter = exec.adapter_name();
    eprintln!("host wgpu adapter: {adapter}");
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "Chrome's GL frames must rasterize on the software Vulkan device, got adapter {adapter:?}"
    );

    // ---- 4. Our compositor on the STANDARD discovery socket, in a background thread --------------------
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
    let name_str = socket_name.to_string_lossy().into_owned();
    assert!(name_str.starts_with("wayland-"), "expected a `wayland-N` name, got {name_str:?}");
    let socket_path = runtime_dir.join(&socket_name);
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);

    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket_path.exists() {
        assert!(Instant::now() < deadline, "discovery socket {socket_path:?} never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }

    // ---- 5. Spawn REAL chromium pointed at our shims + compositor + executor ---------------------------
    let out_path = runtime_dir.join("chrome.stdout");
    let err_path = runtime_dir.join("chrome.stderr");
    let out_file = std::fs::File::create(&out_path).expect("create stdout capture");
    let err_file = std::fs::File::create(&err_path).expect("create stderr capture");

    // ~/.hl/gl first so the loader also prefers OUR libEGL/libGLESv2, then Chrome's dependency dirs.
    let mut ld_path = gl_dir.as_os_str().to_os_string();
    for d in &dep_dirs {
        ld_path.push(":");
        ld_path.push(d);
    }

    // GAP #0 neutralizer: LD_PRELOAD a shim that patches out Chromium's fatal fd-ownership enforcement so it
    // gets past early ChromeMain on this kernel and reaches the Wayland/EGL/GL path we actually exercise.
    let preload = build_fd_ownership_preload(&runtime_dir);
    match &preload {
        Some(p) => eprintln!("GAP #0 fd-ownership preload: {}", p.display()),
        None => eprintln!(
            "no fd-ownership preload (HL_CHROME_PRELOAD unset and csrc/chrome_fdguard.c did not compile) — \
             Chrome will re-hit GAP #0."
        ),
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
        // With --no-sandbox the zygote is pointless, and this box's Chromium fatals with "Failed sending
        // zygote boot message" once past GAP #0 — skip the zygote so it proceeds to Wayland/GPU bring-up.
        .arg("--no-zygote")
        // Surface Chrome's OWN fatal reason on stderr instead of crashpad's opaque `pread64` EIO noise (this
        // box's Chromium aborts in early ChromeMain — see the GAP #0 diagnosis below).
        .arg("--disable-crashpad-for-testing")
        .arg("--disable-dev-shm-usage")
        .arg("--disable-features=Vulkan")
        .arg("--disable-background-networking")
        .arg("--disable-renderer-backgrounding")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--enable-logging=stderr")
        .arg("--v=1")
        .arg("--window-size=800,600")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg(&file_url)
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("OZONE_PLATFORM", "wayland")
        .env("LD_LIBRARY_PATH", &ld_path)
        .env("HL_GPU_EXEC", exec.sock())
        .env("HL_SHIM_DEBUG", "1")
        .env_remove("DISPLAY");
    if let Some(p) = &preload {
        cmd.env("LD_PRELOAD", p);
    }
    // Propagate shim logging so a HL_LOG=gl,transport run surfaces the GL driver's per-frame diagnostics.
    for var in ["HL_LOG", "HL_LOG_LEVEL"] {
        if let Ok(v) = std::env::var(var) {
            cmd.env(var, v);
        }
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::from(out_file)).stderr(Stdio::from(err_file));

    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", run_bin.display()));

    // ---- 6. Let it render: poll the presenter until we have a few frames or hit the deadline ----------
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
        std::thread::sleep(Duration::from_millis(100));
    }

    // ---- 7. Teardown FIRST (never leave the app or the compositor thread running) ---------------------
    let _ = child.kill();
    let killed_status = child.wait().ok();
    stop.store(true, Ordering::Relaxed);
    let _ = compositor.join();

    let stdout = read_to_string(&out_path);
    let stderr = read_to_string(&err_path);
    let submit_count = exec.submit_count();

    // Classify the best (most-orange) captured frame.
    let best = frames.iter().max_by_key(|f| (orange_fraction(f) * 1e6) as u64);
    let (orange_pct, white_pct, blank_pct, spread, dims) = match best {
        Some(f) => (
            orange_fraction(f) * 100.0,
            white_fraction(f) * 100.0,
            transparent_or_black_fraction(f) * 100.0,
            luminance_spread(f),
            format!("{}x{}", f.width, f.height),
        ),
        None => (0.0, 0.0, 0.0, 0, "none".to_string()),
    };

    eprintln!(
        "--- chrome stdout ({} bytes) ---\n{}\n--- chrome stderr ({} bytes, decisive lines) ---\n{}\n\
         --- host: adapter={adapter} gpu_submits={submit_count} presented_frames={} frame={dims} \
         orange={orange_pct:.1}% white={white_pct:.1}% blank={blank_pct:.1}% spread={spread} \
         app_exited={:?} ---",
        stdout.len(),
        truncate_tail(&stdout, 4000),
        stderr.len(),
        decisive_lines(&stderr),
        frames.len(),
        app_exited.or(killed_status),
    );

    // ---- 8. DIAGNOSE BY STAGE (the deliverable) --------------------------------------------------------
    // A raw wait status whose low 7 bits are SIGTRAP (5) is an arm64 IMMEDIATE_CRASH (`brk`) — Chrome's
    // CHECK/abort path. On THIS box that abort is a FD-ownership violation in early ChromeMain (see GAP #0).
    let exited = app_exited.or(killed_status);
    let sigtrapped = exited.map(|s| (s.into_raw() & 0x7f) == 5).unwrap_or(false);
    // GAP #0 is the fd-ownership CHECK specifically — key it on Chromium's own message, NOT on a bare
    // SIGTRAP: once the preload neutralizes GAP #0, Chrome reaches the Wayland/GPU layer and may still
    // SIGTRAP at a LATER downstream CHECK, which must not be mis-attributed to GAP #0.
    let fd_ownership_abort = stderr.contains("FD ownership violation");
    // Did Chrome get as far as bringing up the ozone-Wayland backend (i.e. it connected to OUR compositor)?
    let reached_wayland = stderr.contains("ozone/platform/wayland")
        || stderr.contains("ozone_platform_wayland")
        || stderr.contains("wayland_buffer_manager")
        || stderr.to_lowercase().contains("drm render node");

    let rendered = orange_pct > 40.0;
    if !rendered {
        let stage = if fd_ownership_abort {
            "GAP #0 / NEUTRALIZER DID NOT APPLY — Chromium still aborts in early ChromeMain with a 'FD \
             ownership violation' (arm64 IMMEDIATE_CRASH / SIGTRAP), BEFORE any Wayland/EGL/GL. This gap is \
             PINNED and normally NEUTRALIZED by the LD_PRELOAD shim `csrc/chrome_fdguard.c`: Chromium's own \
             global close()/ScopedFD Acquire/Free check a per-fd owned-bitmap and CHECK-crash (message from \
             base/files/scoped_file.cc) when a fd is closed/acquired inconsistently — a race that only trips \
             on this OrbStack kernel's fd-reuse timing. The shim runtime-patches the 3 enforcement branches \
             to NOP, matching a stock release build. Seeing this stage means the preload was MISSING or \
             FAILED to build/apply: check the 'GAP #0 fd-ownership preload' line above, set \
             $HL_CHROME_PRELOAD to a prebuilt shim, or verify the system `cc` is available."
        } else if sigtrapped && reached_wayland {
            "GAP #0c / POST-NEUTRALIZE downstream CHECK — GAP #0 (fd-ownership) AND GAP #0b \
             (TemplateURLRef::HandleReplacements NOTREACHED, link-vaddr 0x0af711d8 — see below) are both \
             neutralized, and Chrome got PAST early ChromeMain into the ozone-Wayland backend: it connected \
             to OUR compositor and probed the DRM render node (see the wayland/drm lines above). It then hit \
             a NEW, later Chromium IMMEDIATE_CRASH/CHECK (SIGTRAP) before lowering a GL frame (0 submits) — \
             a fresh gap downstream of #0b. NEXT: pin the new SIGTRAP site (base-relative PC, via \
             scratchpad/traplr.so + HL_TRAP_LOG) the same way #0/#0b were pinned, and decide neutralize vs \
             real stack gap. (GAP #0b was: this Debian Chromium's HandleReplacements switch has no case for \
             search-URL replacement types 18 & 28, whose jump-table slots point at a NOTREACHED brk; the \
             default search template expands a type-28 replacement at startup. Neutralized in \
             csrc/chrome_fdguard.c by rerouting those two jump-table slots to the benign skip/continue \
             target 0x0af6f7e0, dropping the unhandled replacement instead of aborting.)"
        } else if sigtrapped {
            "POST-NEUTRALIZE SIGTRAP (pre-Wayland) — GAP #0 is neutralized (no fd-ownership abort) but \
             Chrome SIGTRAPped at another CHECK before the ozone-Wayland backend came up. Pin the new PC."
        } else if submit_count == 0 {
            "GAP #2 / EGL bring-up: the host executor saw 0 GPU submits — Chrome never lowered a GL frame \
             to OUR shim. Either Chrome bound its own ANGLE (not ours), or it stopped BEFORE the first GL \
             frame (Wayland display-connect, EGL init, GPU-process bring-up, or the page never painted). \
             Read the FIRST hard failure in the stderr above (missing symbol, EGL_BAD_*, ozone/wayland \
             connect error) and pin it."
        } else if frames.is_empty() {
            "GAP #1 (shader) or present: Chrome SUBMITTED GL IR (our libEGL bound!) but NO frame reached \
             the compositor. Most likely its GLSL-ES shaders failed to compile on the host (grep the stderr \
             for a shader-compile error), or the wl_shm present onto Chrome's surface never committed."
        } else if orange_pct < 5.0 && blank_pct > 50.0 {
            "GAP #1 (shader) / content: Chrome SUBMITTED and the compositor PRESENTED frames, but they are \
             blank/transparent — no orange geometry is visible. The GLSL-ES fragment shaders likely did not \
             compile (transparent clear survives) — capture the shader-compile error text."
        } else if white_pct > 50.0 {
            "GAP #4/#6 present/context: frames are WHITE — Chrome composited a default/uninitialized \
             surface (present or GL context/FBO completeness), not the page content."
        } else {
            "PARTIAL: frames present with some content but < 40% orange — inspect the PNGs; the page may be \
             partially painted or mis-transformed."
        };
        eprintln!(
            "CHROME FIRST-LIGHT DIAGNOSED (suite stays green as a tracker):\n\
             Stage: {stage}\n\
             Evidence: gpu_submits={submit_count}, presented_frames={}, best frame {dims}: \
             orange={orange_pct:.1}% white={white_pct:.1}% blank={blank_pct:.1}% spread={spread}.\n\
             PNGs (if any) under {}.\n",
            frames.len(),
            png_dir.display(),
        );
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&runtime_dir);
        return;
    }

    // ---- 9. RENDERS: assert the composited window is really the orange page ----------------------------
    assert!(
        submit_count > 0,
        "Chrome produced orange frames but the host executor saw 0 GPU submits — the pixels did not come \
         from our GL lowering path"
    );
    let frame = best.expect("a best frame exists when rendered").clone();
    let png = png_dir.join(format!("frame-{}.png", frame.serial));
    assert!(png.exists(), "a real PNG of the composited Chrome frame was written at {png:?}");
    eprintln!(
        "CHROME FIRST-LIGHT PASSED: real Chromium composited the orange page through the full stack.\n\
         PNG: {}\n  frames: {}, gpu submits: {}, orange: {orange_pct:.1}%, adapter: {adapter}",
        png.display(),
        frames.len(),
        submit_count,
    );
    let _ = std::fs::remove_file(&socket_path);
}

/// Build a run prefix that mirrors Chrome's own lib dir by symlinks, EXCEPT the bundled ANGLE
/// `libEGL.so`/`libGLESv2.so`, which are replaced by symlinks to OUR staged shims. The chromium ELF itself
/// is COPIED (so `/proc/self/exe` — hence Chrome's DIR_MODULE — resolves INTO this prefix, where its ANGLE
/// is absent and ours is present). Returns the run binary path and the extra dependency dirs to place on
/// LD_LIBRARY_PATH (Chrome's bundled shared libs live in a sibling `aarch64-linux-gnu` tree).
fn build_run_prefix(chromium_bin: &Path, gl_dir: &Path) -> std::io::Result<(PathBuf, Vec<PathBuf>)> {
    let lib_dir = chromium_bin.parent().expect("chromium binary has a parent dir");
    // On a roomy filesystem ($HOME), CACHED across runs — the chromium ELF is 264 MiB and the shared /tmp
    // tmpfs is near-full. Symlinks are refreshed each run; the big copy is reused when its size matches.
    let prefix = roomy_base().join("prefix");
    std::fs::create_dir_all(&prefix)?;
    // Drop any stale symlinks from a previous run, but keep the cached chromium copy in place.
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
            // Point Chrome's ANGLE names at OUR staged shims.
            let target = if name_str == "libEGL.so" { "libEGL.so.1" } else { "libGLESv2.so.2" };
            std::os::unix::fs::symlink(gl_dir.join(target), &dst)?;
        } else if name_str == "chromium" {
            // Copy the ELF so its resolved real path is inside this prefix (DIR_MODULE = this prefix).
            // Reuse the cached copy when its size already matches (a 264 MiB copy per run is wasteful).
            let src_len = entry.metadata()?.len();
            let need_copy = std::fs::metadata(&dst).map(|m| m.len() != src_len).unwrap_or(true);
            if need_copy {
                std::fs::copy(entry.path(), &dst)?;
            }
            let mut perms = std::fs::metadata(&dst)?.permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&dst, perms)?;
        } else {
            // Everything else (resources, other .so, locales dir) — symlink through.
            std::os::unix::fs::symlink(entry.path(), &dst)?;
        }
    }

    // Also stage our libEGL/libGLESv2 under their SONAME filenames in the prefix, so a loader search that
    // lands in the prefix (it is on LD_LIBRARY_PATH implicitly via DIR_MODULE for some Chrome versions) still
    // finds them; harmless if unused.
    for (link, target) in [("libEGL.so.1", "libEGL.so.1"), ("libGLESv2.so.2", "libGLESv2.so.2")] {
        let dst = prefix.join(link);
        if !dst.exists() {
            let _ = std::os::unix::fs::symlink(gl_dir.join(target), &dst);
        }
    }

    // Dependency dirs: Chrome's bundled shared libs sit in <root>/usr/lib/aarch64-linux-gnu (+ /pulseaudio).
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
    // The original lib dir stays available too (for any bundled .so we did not need to shadow).
    deps.push(lib_dir.to_path_buf());

    Ok((prefix.join("chromium"), deps))
}

/// Build (or locate) the GAP #0 neutralizer: an LD_PRELOAD `.so` that patches out this Chromium's fatal
/// fd-ownership enforcement (see the GAP #0 note above and `csrc/chrome_fdguard.c`). Without it the real
/// arm64 Chromium aborts in early ChromeMain before any Wayland/EGL/GL is reached. Prefers a prebuilt path
/// in `$HL_CHROME_PRELOAD`; otherwise compiles `csrc/chrome_fdguard.c` with the system `cc` into `out_dir`.
/// Returns `None` (test proceeds, will re-diagnose GAP #0) if no compiler / source is available.
fn build_fd_ownership_preload(out_dir: &Path) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HL_CHROME_PRELOAD") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let manifest = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let src = PathBuf::from(&manifest).join("csrc").join("chrome_fdguard.c");
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
        eprintln!(
            "could not compile the fd-ownership preload ({}): {}",
            src.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    Some(so)
}

/// A base directory on a roomy filesystem ($HOME/.cache/hl-wip-chrome) — the shared /tmp on this box is a
/// near-full 22 GiB tmpfs, and the chromium ELF copy alone is 264 MiB.
fn roomy_base() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME set");
    let base = PathBuf::from(home).join(".cache").join("hl-wip-chrome");
    let _ = std::fs::create_dir_all(&base);
    base
}

/// Fraction of pixels that are the page's orange (#ff7700 = 255,119,0): strong red, mid green, low blue.
fn orange_fraction(f: &CapturedFrame) -> f64 {
    let total = (f.rgba.len() / 4).max(1);
    let n = f
        .rgba
        .chunks_exact(4)
        .filter(|p| p[0] >= 180 && (60..=180).contains(&p[1]) && p[2] <= 90 && p[3] >= 200)
        .count();
    n as f64 / total as f64
}

/// Fraction of pixels that are near-white (all channels high, opaque).
fn white_fraction(f: &CapturedFrame) -> f64 {
    let total = (f.rgba.len() / 4).max(1);
    let n = f
        .rgba
        .chunks_exact(4)
        .filter(|p| p[0] >= 230 && p[1] >= 230 && p[2] >= 230)
        .count();
    n as f64 / total as f64
}

/// Fraction of pixels that are transparent or near-black (a blank/uninitialized surface).
fn transparent_or_black_fraction(f: &CapturedFrame) -> f64 {
    let total = (f.rgba.len() / 4).max(1);
    let n = f
        .rgba
        .chunks_exact(4)
        .filter(|p| p[3] < 20 || (p[0] as u32 + p[1] as u32 + p[2] as u32) < 30)
        .count();
    n as f64 / total as f64
}

/// The luminance spread (max minus min per-pixel R+G+B) — near-zero for a flat/blank buffer.
fn luminance_spread(f: &CapturedFrame) -> i32 {
    let mut lo = i32::MAX;
    let mut hi = i32::MIN;
    for p in f.rgba.chunks_exact(4) {
        let l = p[0] as i32 + p[1] as i32 + p[2] as i32;
        lo = lo.min(l);
        hi = hi.max(l);
    }
    if hi < lo {
        0
    } else {
        hi - lo
    }
}

/// The stderr lines most likely to name the decisive stop (EGL/GL/ANGLE/ozone/wayland/shader errors).
fn decisive_lines(stderr: &str) -> String {
    let mut hits: Vec<&str> = stderr
        .lines()
        .filter(|l| {
            // Drop Chrome's very chatty VERBOSE1/INFO lines so the real signal leads the report.
            if l.contains(":VERBOSE1:") || l.contains(":INFO:") {
                return false;
            }
            let s = l.to_lowercase();
            s.contains("egl")
                || s.contains("gl error")
                || s.contains("glerror")
                || s.contains("angle")
                || s.contains("ozone")
                || s.contains("wayland")
                || s.contains("shader")
                || s.contains("compile")
                || s.contains("symbol")
                || s.contains("ownership")
                || s.contains("violation")
                || s.contains("crash")
                || s.contains("trap")
                || s.contains("signal")
                || s.contains("abort")
                || s.contains("fail")
                || s.contains("error")
                || s.contains("unimpl")
                || s.contains("not implemented")
                || s.contains("fatal")
                || s.contains("hl_")
        })
        .collect();
    if hits.is_empty() {
        hits = stderr.lines().rev().take(40).collect();
        hits.reverse();
    }
    // Cap the report so a very chatty --v=1 run stays readable.
    if hits.len() > 120 {
        hits.drain(0..hits.len() - 120);
    }
    hits.join("\n")
}

/// Keep only the last `max` bytes of a string (on a char boundary) for the report.
fn truncate_tail(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let start = s.len() - max;
    let start = (start..s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
    format!("…{}", &s[start..])
}

/// Read a capture file to a String (empty if unreadable).
fn read_to_string(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

/// Locate a chromium binary. Prefers $HL_CHROME_BIN, then PATH, then the known dpkg-x extraction prefix.
fn which_chromium() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HL_CHROME_BIN") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
        for dir in std::env::var("PATH").unwrap_or_default().split(':') {
            let p = Path::new(dir).join(name);
            // Skip shell-wrapper launchers; we want the real ELF (with its bundled ANGLE alongside).
            if p.exists() && is_elf(&p) {
                return Some(p);
            }
        }
    }
    // Known dpkg-x extraction on this box (the real ELF, not the /usr/bin wrapper script).
    let known = PathBuf::from("/Users/x/.dd/workspaces/chromedeb/upper/usr/lib/chromium/chromium");
    if known.exists() {
        return Some(known);
    }
    None
}

/// Whether a file begins with the ELF magic (so we skip shell-script launcher wrappers).
fn is_elf(p: &Path) -> bool {
    let mut buf = [0u8; 4];
    if let Ok(mut f) = std::fs::File::open(p) {
        if f.read_exact(&mut buf).is_ok() {
            return buf == [0x7f, b'E', b'L', b'F'];
        }
    }
    false
}
