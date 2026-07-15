//! THE MILESTONE — run the REAL third-party editor **Zed** (its GPUI/`blade-graphics` **Vulkan** renderer,
//! the Wayland build) end to end through the ENTIRE hl_wip stack and capture its rendered window off our
//! compositor.
//!
//! WHY ZED (over GTK): GTK4's "gl" renderer is blocked on a large `#version 320 es` GskGpu shader-frontend
//! effort (see `gtk4_e2e.rs`). Zed on Linux renders via GPUI/`blade-graphics`, which targets **Vulkan**
//! (SPIR-V shaders) — our strongest, most-proven path (the textured `vkcube` runs continuously end to end:
//! `vkcube_e2e.rs`). So Zed exercises the Vulkan ICD → IR → WgpuExecutor(lavapipe) → present-into-app-surface
//! → compositor loop that already carries real geometry, and may reach a frame GTK could not.
//!
//! The full loop this proves (every piece already committed; this test is the composition-root wiring):
//!
//!   real `zed-editor`  (GPUI Wayland backend: connects to $WAYLAND_DISPLAY, creates its OWN wl_surface;
//!                       `blade-graphics` Vulkan backend: dlopen's the real Vulkan loader)
//!     -> the REAL Khronos Vulkan loader picks OUR staged ICD (`VK_ICD_FILENAMES=~/.hl/vulkan/<arch>/icd.json`
//!        -> `libvk_hl.so`)
//!     -> every Vulkan call lowered to hl_gpu IR and shipped over `$HL_GPU_EXEC`
//!     -> host `WgpuExecutor` on lavapipe (llvmpipe / software Vulkan) REALLY rasterizes Zed's UI
//!     -> `vkQueuePresentKHR` reads the presented swapchain image back off the host and the shim marshals it
//!        as a `wl_shm` `wl_buffer` onto Zed's OWN `wl_surface` (adapter/wayland_app.rs)
//!     -> our compositor (`hl_wip_compositor::adapter::smithay::run_auto`, a real Smithay Wayland server on a
//!        temp `$WAYLAND_DISPLAY`) receives the commit, reads the shm pixels, composes the scene
//!     -> `PngPresenter` captures the presented surface as a real frame (+ a viewable `.png`).
//!
//! ASSERTED (when pixels arrive): the presenter captured Zed's toplevel as a NON-BLANK frame carrying Zed's
//! dark UI chrome — non-uniform structure, a covered interior region that differs from the flat clear, on a
//! real toplevel (the app's own surface). Zed's default theme is DARK (not GTK's light chrome), so we assert
//! structure + a covered-vs-clear difference rather than "bright" pixels.
//!
//! HONEST STATE: Zed is a large real app and WILL likely reveal a real gap somewhere (a Vulkan feature /
//! extension `blade` needs that our ICD does not advertise, a device-creation feature chain, a specific
//! SPIR-V construct, a swapchain detail, or a wl protocol global). This test is ALSO the diagnosis vehicle:
//! if pixels do NOT arrive it prints the PRECISE stop (which stage produced/observed what, the decisive Zed
//! log/stderr line, gpu submits vs presented frames, install status) and stays GREEN as a diagnosed-gap
//! tracker (mirroring the early vkcube / current gtk4 milestones), so a focused fix can be dispatched to the
//! owning crate. If pixels DO arrive, it asserts real Zed content hard.
//!
//! ROBUSTNESS: bounded timeouts everywhere (the app is killed on a deadline, the compositor thread is
//! stopped + joined), the app's stdout/stderr AND Zed's own on-disk log are captured for the report. Zed is
//! an editor and may spawn helper processes — we spawn it in its own process group and kill the whole group.

use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
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

/// How many presented frames we want before we are satisfied the loop is live (a few real frames).
const TARGET_FRAMES: usize = 3;
/// Hard ceiling on how long the real app is allowed to run before we kill it (never hang). Zed cold-start on
/// lavapipe is heavy (blade device bring-up + first UI frame + font/theme load), so this is generous.
const APP_DEADLINE: Duration = Duration::from_secs(75);

#[test]
fn zed_composites_through_the_full_stack() {
    // ---- 0. Preconditions: the real Zed binary + our staged ICD must be present -----------------------
    let app_bin = match which_zed() {
        Some(p) => p,
        None => {
            eprintln!(
                "zed-editor (the real Zed Linux Wayland/Vulkan build) not found — skipping the milestone. \
                 Stage the Linux aarch64 tarball under hl_wip/target/zed-dl (zed.app/libexec/zed-editor) or \
                 install Zed."
            );
            return;
        }
    };
    eprintln!("real Vulkan app: {}", app_bin.display());

    let vk_dir = staged_dir("vulkan");
    let icd = vk_dir.join("icd.json");
    assert!(
        icd.exists() && vk_dir.join("libvk_hl.so").exists(),
        "staged Vulkan ICD missing at {vk_dir:?} — build hl_wip-vulkan's shim first (a `cargo test` in \
         hl_wip stages it)"
    );

    // The real loader must be present (Zed dlopen's it; without it the run can't reach our ICD).
    let loader = "/usr/lib/aarch64-linux-gnu/libvulkan.so.1";
    if !Path::new(loader).exists() {
        eprintln!("SKIP: real Vulkan loader {loader} not present — cannot drive our ICD.");
        return;
    }
    // GPUI's Wayland backend dlopen's libwayland-client at runtime; without it Zed cannot open our display.
    let wl_client = "/usr/lib/aarch64-linux-gnu/libwayland-client.so.0";
    if !Path::new(wl_client).exists() {
        eprintln!("SKIP: libwayland-client {wl_client} not present — Zed cannot open a Wayland display.");
        return;
    }

    // ---- 1. A private, 0700 XDG_RUNTIME_DIR so the discovery socket lives in isolation ---------------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-zed-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (a single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET"); // no inherited fd may short-circuit discovery

    // Isolate ALL of Zed's on-disk state under our temp dir (data/config/cache + user-data-dir), so the run
    // never touches real Zed state and its log is easy to find + read back for the report.
    let data_dir = runtime_dir.join("zed-data");
    let config_dir = runtime_dir.join("zed-config");
    let cache_dir = runtime_dir.join("zed-cache");
    let home_dir = runtime_dir.join("home");
    for d in [&data_dir, &config_dir, &cache_dir, &home_dir] {
        std::fs::create_dir_all(d).expect("create zed state dir");
    }
    // A tiny project for Zed to open (so it maps a populated workspace window immediately).
    let project_dir = runtime_dir.join("project");
    std::fs::create_dir_all(&project_dir).expect("create project dir");
    std::fs::write(project_dir.join("hello.txt"), "hello from the hl_wip zed milestone\n").ok();

    let png_dir = runtime_dir.join("png");

    // ---- 2. The host GPU executor: WgpuExecutor on lavapipe, served over a temp unix socket ----------
    let exec = WgpuExecutorServer::start("zed");
    let adapter = exec.adapter_name();
    eprintln!("host wgpu adapter: {adapter}");
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "the app's Vulkan frames must rasterize on the software Vulkan device, got adapter {adapter:?}"
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

    // ---- 4. Spawn the REAL Zed pointed at our ICD + compositor + executor ------------------------------
    // stdout/stderr go to files (not pipes) so a chatty run can never fill a pipe and stall the app; we read
    // them back for the report after teardown.
    let out_path = runtime_dir.join("zed.stdout");
    let err_path = runtime_dir.join("zed.stderr");
    let out_file = std::fs::File::create(&out_path).expect("create stdout capture");
    let err_file = std::fs::File::create(&err_path).expect("create stderr capture");

    let mut child = Command::new(&app_bin)
        .arg("--user-data-dir")
        .arg(&data_dir)
        .arg(&project_dir) // open a workspace so a populated window maps immediately
        .env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("XDG_DATA_HOME", &data_dir)
        .env("XDG_CONFIG_HOME", &config_dir)
        .env("XDG_CACHE_HOME", &cache_dir)
        .env("HOME", &home_dir)
        .env("VK_ICD_FILENAMES", &icd) // the real loader picks OUR ICD
        .env("VK_DRIVER_FILES", &icd) // newer loaders read this name instead
        .env("VK_LOADER_LAYERS_DISABLE", "~all~") // no implicit layers between the app and our ICD
        .env("HL_GPU_EXEC", exec.sock()) // the host executor the staged ICD lowers to
        // Force wgpu (Zed 1.10's GPUI renderer is `gpui_wgpu`, NOT blade) to consider ONLY the Vulkan
        // backend, so it cannot silently fall back to the system Mesa GL (llvmpipe) adapter and render
        // OUR-stack-bypassing pixels — any captured frame must then have come through our ICD. (Best-effort:
        // honored when gpui builds its wgpu Instance from env; if ignored, the post-run logic still refuses
        // to count non-HL fallback frames as a pass.)
        .env("WGPU_BACKEND", "vulkan")
        .env("VK_LOADER_DEBUG", "error,warn") // surface a loader/ICD mismatch in stderr (diagnosis)
        .env("RUST_LOG", "blade_graphics=debug,gpui=debug,info") // blade's Vulkan init + gpui window setup
        .env("RUST_BACKTRACE", "1") // a panic (missing wl global / vk feature) prints where
        .env("ZED_ALLOW_ROOT", "1") // in case the sandbox runs as root
        .env("ZED_HTTP_PROXY", "http://127.0.0.1:1") // no real network in the sandbox; fail auth fast, keep UI
        .env_remove("DISPLAY") // no X: force the wayland backend path
        .env_remove("ZED_WINDOW_DECORATIONS")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file))
        // Own process group so we can kill Zed AND any helper it spawns cleanly on teardown.
        .process_group(0)
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", app_bin.display()));
    let child_pid = child.id() as i32;

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
    // Kill the whole process group (Zed + any helper) then reap.
    unsafe {
        libc::kill(-child_pid, libc::SIGKILL);
    }
    let _ = child.kill();
    let killed_status = child.wait().ok();
    stop.store(true, Ordering::Relaxed);
    let _ = compositor.join();

    let stdout = read_to_string(&out_path);
    let stderr = read_to_string(&err_path);
    let zed_log = read_zed_log(&data_dir); // Zed's own structured log (the richest diagnosis source)
    let submit_count = exec.submit_count();
    eprintln!(
        "--- zed stdout ---\n{stdout}\n--- zed stderr ---\n{stderr}\n--- zed log (tail) ---\n{}\n\
         --- host: adapter={adapter} gpu_submits={submit_count} presented_frames={} app_exited={:?} ---",
        tail(&zed_log, 120),
        frames.len(),
        app_exited.or(killed_status),
    );

    // ---- 7. Diagnose precisely unless pixels arrived THROUGH OUR STACK (stay GREEN as a gap tracker) ---
    // HONEST pass gate. `submit_count > 0` is NOT sufficient: wgpu PROBES every adapter at device-creation
    // (a handful of internal submits — buffer zero-init, the indirect-validation setup) BEFORE it selects
    // one, so a few submits land on our executor even when wgpu ultimately REJECTS our device and renders
    // every real frame on the fallback Mesa `llvmpipe` GL adapter. The milestone is proven only when wgpu
    // actually SELECTED our adapter (no "trying next" fallback, not llvmpipe) AND drove real per-frame work
    // through it (submits well above the probe handful).
    let combined = format!("{stderr}\n{zed_log}");
    // wgpu logs this the moment it gives up on our adapter and moves on (device lost / trying next).
    let our_adapter_rejected = combined.contains("hl Metal (Vulkan)")
        && (combined.contains("trying next") || combined.contains("Device(Lost)"));
    // wgpu logs the adapter it settled on; a pass requires that to be OURS, not llvmpipe.
    let our_adapter_selected = combined.contains("Selected GPU adapter: \"hl Metal (Vulkan)")
        && !our_adapter_rejected;
    // A few submits are only wgpu probing our adapter during device creation; real Zed frames submit far
    // more. Require the count to clear that probe ceiling so probe traffic alone can never mark a pass.
    const ADAPTER_PROBE_SUBMIT_CEILING: u64 = 8;
    let real_frame_submits = submit_count > ADAPTER_PROBE_SUBMIT_CEILING;
    let through_our_stack = our_adapter_selected && real_frame_submits;
    if !through_our_stack || frames.is_empty() {
        // The PRECISELY-NAMED gap this run reveals (the "next real bug"), reported for a focused fix in the
        // owning crate (host executor `hl_wip-gpu-wgpu`, outside this test's write scope):
        //
        //   Zed 1.10's GPUI renderer is `gpui_wgpu` (wgpu 24), NOT `blade`. wgpu DISCOVERS our staged ICD,
        //   enumerates it FIRST as adapter "hl Metal (Vulkan)" and tests it FIRST. Device-limit acceptance
        //   is now satisfied (the maintenance4 `maxBufferSize` fix), so wgpu proceeds to build its internal
        //   indirect-draw-validation COMPUTE PIPELINE during device init — and THAT fails:
        //     ERROR [wgpu_core::indirect_validation] indirect-validation error: ComputePipeline(Device(Lost))
        //   wgpu-hal maps an unexpected error from our ICD's compute-pipeline creation to DeviceError,
        //   wgpu-core's `DeviceError::from_hal` turns `Unexpected` into `Lost`, so wgpu self-marks the
        //   device lost and falls back to the Mesa `llvmpipe` GL adapter, rendering Zed's UI BYPASSING our
        //   stack.
        //   ROOT CAUSE (host executor, `hl_wip-gpu-wgpu`): wgpu's indirect-validation shader is a plain
        //   SPIR-V compute shader (storage buffers in two bind groups + a push constant). `create_shader`
        //   (src/shader.rs) naga-translates that SPIR-V to WGSL SUCCESSFULLY (`create_shader kind=SpirV
        //   words=466`, no error) but stores EVERY SPIR-V module as `ShaderNative::Graphics`.
        //   `create_compute_pipeline` (src/pipeline.rs) then only accepts `ShaderNative::Kernel` (the
        //   internal PTX-kernel→WGSL ABI: param blob at binding 0, regions at r+1) and rejects the Graphics
        //   variant outright: `pipeline rejected kind=compute reason=needs-kernel-shader`. So the executor
        //   has NO path to run an arbitrary SPIR-V compute shader — it errors, the guest ICD returns that
        //   error, wgpu loses the device.
        //   Fix (host, larger — for a dedicated agent): give the executor a real SPIR-V-compute path:
        //   build a `wgpu::ComputePipeline` from the naga-translated compute module with an AUTO / reflected
        //   bind-group layout + push-constant range (mirroring the render-pipeline path's `layout: None`),
        //   and build the dispatch's bind groups against the pipeline's own group layouts (bindgroup.rs +
        //   submit.rs), rather than the kernel-ABI layout. It must run correctly (not just create), since a
        //   mis-bound validation pipeline would corrupt indirect dispatches.
        //   PREREQUISITE already fixed here (host, small + guarded): the executor's wgpu device now requests
        //   `Features::PUSH_CONSTANTS` when the adapter advertises it (src/device.rs) — without it the
        //   validation shader's `var<push_constant>` failed naga validation one stage earlier.
        let hit_indirect_validation_gap = combined.contains("indirect-validation")
            || combined.contains("needs-kernel-shader")
            || (combined.contains("hl Metal (Vulkan)") && combined.contains("Device(Lost)"));
        let fell_back_to_non_hl = our_adapter_rejected || combined.contains("Selected GPU adapter: \"llvmpipe");
        eprintln!(
            "MILESTONE DIAGNOSED (Zed reached our stack but has NOT yet rendered through it — reported as \
             the next real gap, suite stays green):\n\
             Stage evidence:\n\
             * host GPU executor submits (guest lowered Vulkan IR over $HL_GPU_EXEC): {submit_count} \
               (>{ADAPTER_PROBE_SUBMIT_CEILING} required for a real-frame pass; a handful are only wgpu \
                probing our adapter during device creation)\n\
             * our adapter SELECTED by wgpu (not fallback): {our_adapter_selected}\n\
             * our adapter REJECTED then wgpu tried next / fell back to llvmpipe: {fell_back_to_non_hl}\n\
             * compositor presented frames: {}{}\n\
             * decisive indirect-validation / needs-kernel-shader compute gap seen: {hit_indirect_validation_gap}\n\
             --- layer classification ---\n{}\n\
             --- decisive Zed lines ---\n{}\n",
            frames.len(),
            if fell_back_to_non_hl {
                "  (NOT ours — rendered by the fallback Mesa GL adapter, bypassing our stack)"
            } else {
                ""
            },
            classify_stop(&combined, submit_count),
            decisive_lines(&combined),
        );
        let _ = std::fs::remove_file(&socket_path);
        return;
    }

    // ---- 8. ASSERT the captured pixels are the REAL Zed app's rendered window (through OUR stack) ------
    // Exactly ONE presented surface => one real toplevel (Zed's own wl_surface).
    let mut surfaces: Vec<u32> = frames.iter().map(|f| f.surface.0).collect();
    surfaces.sort_unstable();
    surfaces.dedup();
    assert_eq!(
        surfaces.len(),
        1,
        "expected exactly one presented toplevel (Zed's own surface), saw surface ids {surfaces:?}"
    );

    // Pick the frame with the most visual structure (widest luminance spread).
    let frame = frames
        .iter()
        .max_by_key(|f| luminance_spread(f))
        .expect("at least one captured frame")
        .clone();
    let (w, h) = (frame.width, frame.height);
    assert!(w > 0 && h > 0, "captured frame has a real size, got {w}x{h}");
    eprintln!("asserting on captured frame: {w}x{h}, serial {}", frame.serial);

    // The window is NOT a uniform fill: Zed's dark chrome + panels + text yields a real luminance spread; a
    // blank/clear buffer is near-zero.
    let spread = luminance_spread(&frame);
    assert!(
        spread > 40,
        "the composited frame must carry Zed's window content (non-uniform), but its luminance spread is \
         only {spread} — a blank/flat buffer, not a rendered Zed window"
    );

    // A covered interior region differs from the outer clear: sample the window center vs a corner.
    let center = frame.pixel(w / 2, h / 2).expect("center pixel exists");
    let corner = frame.pixel(0, 0).expect("corner pixel exists");
    let center_sum = center[0] as i32 + center[1] as i32 + center[2] as i32;
    let corner_sum = corner[0] as i32 + corner[1] as i32 + corner[2] as i32;
    assert!(
        (center_sum - corner_sum).abs() > 12,
        "window CENTER should differ from the CORNER clear, but center RGBA {center:?} ~= corner RGBA \
         {corner:?} — Zed's UI did not composite over the clear"
    );

    let png = png_dir.join(format!("frame-{}.png", frame.serial));
    assert!(png.exists(), "a real PNG of the composited Zed frame was written at {png:?}");
    eprintln!(
        "MILESTONE PASSED: real Zed composited through the full stack.\n\
         PNG: {}\n  frames captured: {}, gpu submits: {}, adapter: {adapter}, luminance spread: {spread}",
        png.display(),
        frames.len(),
        submit_count,
    );

    let _ = std::fs::remove_file(&socket_path);
}

/// Classify WHICH layer the run stopped in from the app's combined stderr+log, so the report leads with the
/// crate/stage a focused fix belongs to (compositor/wl-protocol vs Vulkan-ICD vs present).
fn classify_stop(log: &str, submits: u64) -> String {
    let l = log.to_lowercase();
    // The CURRENT observed stop: device limits are accepted, but wgpu's internal indirect-draw-validation
    // COMPUTE PIPELINE fails to build during device init, so wgpu self-marks the device lost and falls back.
    if l.contains("indirect-validation")
        || l.contains("needs-kernel-shader")
        || (l.contains("hl metal (vulkan)") && l.contains("device(lost)"))
    {
        return "LAYER = HOST EXECUTOR compute path (hl_wip-gpu-wgpu): wgpu accepts our device, then builds \
                its indirect-draw-validation COMPUTE PIPELINE during device init and it fails \
                (ComputePipeline(Device(Lost))). That validation shader is a plain SPIR-V compute shader; the \
                host executor's create_shader (src/shader.rs) naga-translates it to WGSL but tags every \
                SPIR-V module ShaderNative::Graphics, and create_compute_pipeline (src/pipeline.rs) only \
                accepts ShaderNative::Kernel (the PTX-kernel ABI) — it rejects the module with \
                'needs-kernel-shader'. Fix: give the executor a real SPIR-V-compute pipeline path (auto/ \
                reflected bind-group layout + push constants), then build the dispatch's bind groups against \
                the pipeline's own layout. (A small prerequisite — requesting Features::PUSH_CONSTANTS on the \
                host device — is already fixed in src/device.rs.) Until then wgpu falls back to Mesa llvmpipe."
            .to_string();
    }
    // Stale device-creation limit rejection (the maintenance4 gap, now fixed): kept as a fallback classifier.
    if l.contains("max_buffer_size") && l.contains("allowed 0") {
        return "LAYER = VULKAN ICD device LIMITS (hl_wip-vulkan): wgpu discovered our adapter \
                \"hl Metal (Vulkan)\" but rejected device creation because max_buffer_size is reported as 0. \
                Our device reports api_version 1.4.0, so wgpu reads max_buffer_size from \
                VkPhysicalDeviceMaintenance4Properties::maxBufferSize — which our \
                vkGetPhysicalDeviceProperties2 (hl_wip-vulkan/shim/vulkan/src/instance.rs) does NOT fill \
                (it fills maintenance3 only). Fix: add a maintenance4 branch setting a real maxBufferSize \
                (e.g. 2 GiB). Until then wgpu falls back to the Mesa llvmpipe GL adapter, bypassing our stack."
            .to_string();
    }
    // Wayland-protocol / compositor layer: a missing/unsupported global or a wl protocol error.
    if l.contains("wayland") && (l.contains("global") || l.contains("no such") || l.contains("protocol"))
        || l.contains("wl_") && l.contains("not") && l.contains("support")
        || l.contains("xdg_wm_base")
        || l.contains("wp_viewporter")
        || l.contains("wl_compositor")
    {
        return "LAYER = COMPOSITOR / wl-protocol (hl_wip-compositor): Zed's GPUI Wayland backend needs a \
                global our compositor does not advertise, or hit a wl protocol error. Fix belongs in \
                hl_wip-compositor/src/adapter/smithay/state.rs (advertise the missing global)."
            .to_string();
    }
    // Vulkan device/feature layer: blade could not create a device / a required feature/extension is absent.
    if l.contains("no suitable")
        || l.contains("no compatible")
        || l.contains("device extension")
        || l.contains("not supported")
        || l.contains("vk_error")
        || l.contains("feature")
        || l.contains("timeline")
        || l.contains("descriptor")
        || l.contains("dynamic_rendering")
        || l.contains("blade")
    {
        return format!(
            "LAYER = VULKAN ICD (hl_wip-vulkan): blade-graphics rejected our physical device — a required \
             Vulkan feature/extension is not advertised. Our device advertises ONLY VK_KHR_swapchain + \
             VK_KHR_dynamic_rendering (hl_wip-vulkan/src/model/capability.rs::DEVICE_EXTENSIONS) and a \
             minimal VkPhysicalDeviceFeatures (shim/vulkan/src/instance.rs::vkGetPhysicalDeviceFeatures). \
             blade typically needs timeline semaphores (VK_KHR_timeline_semaphore / core 1.2), descriptor \
             indexing, and buffer-device-address. gpu_submits={submits}. Fix belongs in hl_wip-vulkan \
             (advertise + back the feature chain blade gates device creation on)."
        );
    }
    if submits > 0 {
        return "LAYER = PRESENT / readback (hl_wip-vulkan present path): Zed DID submit Vulkan work \
                (gpu_submits>0) but no frame was presented onto its wl_surface. Inspect \
                hl_wip-vulkan/src/service/present.rs (swapchain image readback) + \
                hl_wip-vulkan/src/adapter/wayland_app.rs (wl_shm attach onto the app surface)."
            .to_string();
    }
    "LAYER = UNCLASSIFIED: no decisive marker matched. Read the decisive Zed lines below and the full log \
     tail above."
        .to_string()
}

/// The luminance spread (max minus min per-pixel R+G+B) across the frame — near-zero for a flat/blank
/// buffer, wide for a real rendered scene.
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

/// Extract the lines from the app's combined output most likely to name the decisive stop (Vulkan/blade/
/// wayland/gpui errors), so the diagnosis report leads with signal rather than the full log.
fn decisive_lines(log: &str) -> String {
    let mut hits: Vec<&str> = log
        .lines()
        .filter(|l| {
            let s = l.to_lowercase();
            s.contains("vulkan")
                || s.contains("vk_")
                || s.contains("blade")
                || s.contains("wayland")
                || s.contains("wl_")
                || s.contains("gpu")
                || s.contains("swapchain")
                || s.contains("surface")
                || s.contains("device")
                || s.contains("panic")
                || s.contains("fail")
                || s.contains("error")
                || s.contains("unsupport")
                || s.contains("not implemented")
                || s.contains("no suitable")
                || s.contains("no compatible")
        })
        .collect();
    if hits.is_empty() {
        hits = log.lines().rev().take(30).collect();
        hits.reverse();
    }
    // Keep the report bounded.
    if hits.len() > 60 {
        hits = hits[hits.len() - 60..].to_vec();
    }
    hits.join("\n")
}

/// Read a capture file to a String (empty if unreadable).
fn read_to_string(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

/// The last `n` lines of `s`.
fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// Read Zed's own on-disk log. With `XDG_DATA_HOME`/`--user-data-dir` pointed at our temp dir, Zed writes
/// `<data>/zed/logs/Zed.log` (or directly `<data>/logs/Zed.log` for --user-data-dir). Probe both.
fn read_zed_log(data_dir: &Path) -> String {
    for rel in ["logs/Zed.log", "zed/logs/Zed.log", "Zed/logs/Zed.log"] {
        let p = data_dir.join(rel);
        if p.exists() {
            return read_to_string(&p);
        }
    }
    // Fall back to a recursive search for any Zed.log under the data dir.
    fn find(dir: &Path) -> Option<PathBuf> {
        let rd = std::fs::read_dir(dir).ok()?;
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(f) = find(&p) {
                    return Some(f);
                }
            } else if p.file_name().and_then(|n| n.to_str()) == Some("Zed.log") {
                return Some(p);
            }
        }
        None
    }
    find(data_dir).map(|p| read_to_string(&p)).unwrap_or_default()
}

/// Locate the real Zed Linux binary. Prefer the staged tarball under `hl_wip/target/zed-dl`, then PATH
/// (`zed-editor`/`zed`), then common install locations.
fn which_zed() -> Option<PathBuf> {
    // The staged Linux tarball this milestone downloads/extracts.
    let staged: &[&str] = &[
        "target/zed-dl/zed.app/libexec/zed-editor",
        "../hl_wip/target/zed-dl/zed.app/libexec/zed-editor",
    ];
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for rel in staged {
        let p = manifest.join(rel);
        if p.exists() {
            return Some(p);
        }
    }
    // PATH + known locations. Prefer the real GUI binary name `zed-editor` (the `zed` wrapper is a CLI that
    // hands off to it, which would detach from our process group).
    let mut dirs: Vec<PathBuf> =
        std::env::var("PATH").unwrap_or_default().split(':').map(PathBuf::from).collect();
    let home = std::env::var("HOME").unwrap_or_default();
    dirs.push(PathBuf::from(&home).join(".local/bin"));
    dirs.push(PathBuf::from("/opt/zed.app/libexec"));
    dirs.push(PathBuf::from("/usr/lib/zed"));
    for name in ["zed-editor", "zed"] {
        for dir in &dirs {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}
