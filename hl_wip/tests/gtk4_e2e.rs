//! THE MILESTONE — run a REAL GTK4 application (`gtk4-demo` / `gtk4-widget-factory`, Ubuntu's
//! `gtk-4-examples`) end to end through the ENTIRE hl_wip stack and capture its rendered window off our
//! compositor. GTK4 is a heavyweight, real-world toolkit: this is the hardest GUI target we drive.
//!
//! The full loop this proves (every piece already committed; this test is the composition root wiring them):
//!
//!   real /usr/bin/gtk4-demo  (GDK_BACKEND=wayland: connects to $WAYLAND_DISPLAY, creates its OWN wl_surface;
//!                             GSK_RENDERER=gl: forces the GskGL renderer over GLES/EGL — not vulkan/cairo)
//!     -> GTK resolves GL through libepoxy, which dlopen()s + eglGetProcAddress-resolves OUR staged
//!        libEGL + libGLESv2 + libwayland-egl (`~/.hl/gl/<arch>/`, via LD_LIBRARY_PATH)
//!     -> each GskGL frame lowered to hl_gpu IR and shipped over `$HL_GPU_EXEC`
//!     -> host `WgpuExecutor` on lavapipe (llvmpipe / software Vulkan) rasterizes GTK's render nodes
//!     -> `glReadPixels` reads the frame back over the socket
//!     -> our libEGL marshals it as a `wl_shm` buffer onto the app's OWN `wl_surface`
//!        (adapter/wayland_app.rs — the app-surface present path) over the app's `libwayland-client`
//!     -> our compositor (`hl_wip_compositor::adapter::smithay::run_auto`, a real Smithay Wayland server on
//!        a temp `$WAYLAND_DISPLAY`) receives the commit, reads the shm pixels, composes the scene
//!     -> `PngPresenter` captures the presented surface as a real frame (+ a viewable `.png`).
//!
//! ASSERTED (when pixels arrive): the presenter captured GTK's toplevel as a NON-BLANK frame carrying GTK's
//! characteristic light-gray chrome (headerbar / toolbar / widgets) — non-uniform structure with light
//! chrome pixels, on one or more real toplevels (the app's own surfaces).
//!
//! HONEST STATE: GTK4 is heavy and will likely reveal a real gap somewhere in the GL/EGL/GLES → compositor
//! path. This test is also the diagnosis vehicle: if pixels do NOT arrive, it prints the PRECISE stop
//! (which stage produced/observed what, the decisive app stderr line, gpu submits vs presented frames) and
//! stays GREEN as a diagnosed-gap milestone tracker (mirroring the early vkcube milestone), so a focused fix
//! can be dispatched to the owning crate. If pixels DO arrive, it asserts real GTK content hard.
//!
//! ROBUSTNESS: bounded timeouts everywhere (the app is killed on a deadline, the compositor thread is
//! stopped + joined), the app's stdout/stderr are captured to files for the report.

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

use hl_compositor::adapter::smithay::{self, CapturedFrame, PngPresenter};

/// How many presented frames we want before we are satisfied the loop is live (a few real frames).
const TARGET_FRAMES: usize = 3;
/// Hard ceiling on how long the real app is allowed to run before we kill it (never hang). GTK4 on lavapipe
/// is slow to bring up (font/icon/theme load + first GskGL frame), so this is generous.
const APP_DEADLINE: Duration = Duration::from_secs(45);

#[test]
fn gtk4_composites_through_the_full_stack() {
    // ---- 0. Preconditions: a real GTK4 app + our staged GL shims must be present ----------------------
    let app_bin = match which_gtk4_app() {
        Some(p) => p,
        None => {
            eprintln!(
                "no GTK4 example app found (gtk4-demo / gtk4-widget-factory / gtk4-demo-application) — \
                 skipping the milestone (install `gtk-4-examples`)."
            );
            return;
        }
    };
    eprintln!("real GTK4 app: {}", app_bin.display());

    let gl_dir = staged_dir("gl");
    for lib in ["libEGL.so.1", "libGLESv2.so.2", "libwayland-egl.so.1"] {
        assert!(
            gl_dir.join(lib).exists(),
            "staged {lib} missing at {gl_dir:?} — build hl_wip-gl's shim first (a `cargo test` in hl_wip \
             stages it)"
        );
    }

    // ---- 1. A private, 0700 XDG_RUNTIME_DIR so the discovery socket lives in isolation ---------------
    let runtime_dir = std::env::temp_dir().join(format!("hl-wip-gtk4-xdg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).expect("chmod 0700");
    // SAFETY of env mutation: this test owns its whole test binary/process (a single #[test]).
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET"); // no inherited fd may short-circuit discovery

    let png_dir = runtime_dir.join("png");

    // ---- 2. The host GPU executor: WgpuExecutor on lavapipe, served over a temp unix socket ----------
    let exec = WgpuExecutorServer::start("gtk4");
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

    // ---- 4. Spawn the REAL GTK4 app pointed at our shims + compositor + executor -----------------------
    // stdout/stderr go to files (not pipes) so a chatty debug run can never fill a pipe and stall the app;
    // we read them back for the report after teardown.
    let out_path = runtime_dir.join("gtk4.stdout");
    let err_path = runtime_dir.join("gtk4.stderr");
    let out_file = std::fs::File::create(&out_path).expect("create stdout capture");
    let err_file = std::fs::File::create(&err_path).expect("create stderr capture");

    let mut cmd = Command::new(&app_bin);
    cmd.env("WAYLAND_DISPLAY", &socket_name)
        .env("XDG_RUNTIME_DIR", &runtime_dir)
        .env("GDK_BACKEND", "wayland") // force the wayland GDK backend (our compositor)
        .env("GSK_RENDERER", "gl") // force the GskGL renderer over OUR GLES/EGL (not vulkan/ngl-vulkan/cairo)
        .env("LD_LIBRARY_PATH", &gl_dir) // bind OUR libEGL/libGLESv2/libwayland-egl first (epoxy dlopen's these)
        .env("HL_GPU_EXEC", exec.sock()) // the host executor the staged libEGL lowers to
        .env("HL_SHIM_DEBUG", "1") // surface any unimplemented GL/EGL op in stderr (diagnosis)
        .env("GDK_DEBUG", "opengl") // GDK prints its GL/EGL context selection + errors (diagnosis)
        .env("GSK_DEBUG", "renderer") // GSK prints which renderer it realized (diagnosis)
        .env("GTK_A11Y", "none") // no at-spi bus in this sandbox; avoid an a11y stall
        .env_remove("DISPLAY"); // no X: force the wayland backend path
    // Propagate shim logging (HL_LOG / HL_LOG_LEVEL) into the child so a `HL_LOG=gl` run surfaces the GL
    // driver's per-frame diagnostics from inside the real app process for this milestone's diagnosis.
    for var in ["HL_LOG", "HL_LOG_LEVEL"] {
        if let Ok(v) = std::env::var(var) {
            cmd.env(var, v);
        }
    }
    cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    // gtk4-demo opens on a demo list; run it without a specific demo so a full window (sidebar + headerbar)
    // maps immediately. widget-factory / demo-application map a populated window on their own.

    let mut child = cmd
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
        "--- gtk4 stdout ---\n{stdout}\n--- gtk4 stderr ---\n{stderr}\n\
         --- host: adapter={adapter} gpu_submits={submit_count} presented_frames={} app_exited={:?} ---",
        frames.len(),
        app_exited.or(killed_status),
    );

    // ---- 7. The milestone is ACHIEVED: GTK now renders PROPER content, so blank/degenerate is a FAILURE ----
    // Every prior stop is fixed (see the history below) AND the last one — the GskGpu std140 PushConstants
    // MVP/clip/scale globals that GTK 4.22's GL renderer never delivers over any observed GL upload — is now
    // reconstructed by the driver (hl_wip-gl `service::frame::gsk_globals_std140`), so `push.mvp` actually
    // transforms the gl_VertexID-pulled vertices and GTK's real widgets land at the right positions. This test
    // therefore no longer "stays green as a diagnosed-gap tracker": it DEMANDS proper content and FAILS if the
    // frame regresses to a uniform clear / degenerate geometry (a collapsed MVP still composites a light-gray
    // frame that a bare spread/chrome check would wave through). The gate below requires GTK's rich, varied
    // widget surface — thousands of accent-blue and dark-text pixels, not the handful a near-uniform clear has.
    let best_spread = frames.iter().map(luminance_spread).max().unwrap_or(0);
    let has_real_content = frames
        .iter()
        .max_by_key(|f| luminance_spread(f))
        .map(|f| {
            let (blue, _warm, dark) = adwaita_color_counts(f);
            luminance_spread(f) > 40 && has_light_chrome(f) && blue > 3000 && dark > 1000
        })
        .unwrap_or(false);
    if !has_real_content {
        // PROGRESS TO DATE (each of these WAS the stop and is now cleared):
        //   * GDK Wayland display-open: our compositor now advertises wl_data_device_manager (+ the other
        //     required globals), so `gdk_wayland_display_open` accepts our display.
        //   * libepoxy GL-context classification: FIXED in hl_wip-gl/shim/egl/src/driver.rs
        //     `eglQueryContext` — it now answers EGL_CONTEXT_CLIENT_TYPE = EGL_OPENGL_ES_API (was 0).
        //     epoxy's `epoxy_egl_get_current_gl_context_api()` (dispatch_common.c) queries exactly that to
        //     classify the current context; a 0/EGL_NONE answer made `epoxy_get_proc_address` abort with
        //     "Couldn't find current GLX or EGL context". With the truthful client type, GDK reports
        //     "Max texture size: 16384" (was uninitialized garbage) and realizes GskGLRenderer over our
        //     EGL/GLES — `gpu_submits` is now > 0 (GTK's GL IR reaches the host executor).
        //
        // REMAINING GAP (the "next real bug", now precisely one stage further downstream than the old
        // frame-size wall — that wall is FIXED): the GL shim lowers GTK's frame correctly and CHEAPLY, and
        // the frame reaches the host executor, but GTK's shaders do not COMPILE on the host.
        //
        //   FIXED — frame-size collapse: the GL shim now caches guest GL resources across frames+draws by
        //   (GL name, content generation) — hl_wip-gl/src/model/context.rs `sampled_texture_ir` /
        //   `data_buffer_ir`, wired through service/frame.rs `lower_draw`. Before, every one of GTK's ~348
        //   draws re-`CreateTexture`d + re-uploaded the SAME glyph/mask atlas plane it sampled, so one frame
        //   was ~4.3 GiB of redundant `WriteBuffer` (1145 CreateTexture, 1833 CreateBuffer) — far over the
        //   64 MiB negotiated cap AND the 256 MiB transport cap, which desynced the socket and hung the app.
        //   With caching a GTK frame lowers to ~30 MiB (13 CreateTexture, ~4.5 MiB of uploads) — comfortably
        //   under the cap. The multi-FBO frame graph (per-FBO render target) already targets the 1378x774
        //   window, not a 16x16 atlas.
        //
        //   FIXED — GskGpu shader compilation on the host (naga glsl-in): GTK 4.14+'s "gl" renderer emits
        //   1200+-line `#version 320 es` GskGpu sources driven by the C preprocessor. These now COMPILE to
        //   WGSL through the pure-Rust ES→desktop lowering in hl_wip-gpu-wgpu/src/glsl_es.rs plus two
        //   naga-module post-passes in hl_wip-gpu-wgpu/src/wgsl.rs. The six constructs that blocked naga-24,
        //   each fixed truthfully against the REAL forwarded GskGpu source (not a synthetic sample):
        //     1. `#version 320 es` → `#version 460` AND an injected `#define __VERSION__ 460`. naga's
        //        preprocessor (pp_rs) seeds no built-in defines, so an unset `__VERSION__` evaluated to 0 and
        //        GskGpu's `#if __VERSION__ < 420 …` took the no-binding branch of its `layout(std140[, binding=0])`
        //        UBO — defining it makes the binding branch win (uniform block lands at binding 0).
        //     2. `gl_VertexID` hides inside `#define GSK_VERTEX_INDEX gl_VertexID`; the macro body is rewritten
        //        (word-boundary) to `int(gl_VertexIndex)` so post-expansion the token is naga's builtin.
        //     3. `IN(_loc)` / `PASS(_loc)` / `PASS_FLAT(_loc)` drop the location for GL's by-name binding; the
        //        macro definitions are rewritten to carry `layout(location = _loc)` (naga has no by-name binding
        //        and otherwise collides every input at location 0).
        //     4. `switch` cases end in `return`, which naga marks fall-through and its wgsl-out rejects; each
        //        `switch` is lowered to an equivalent `if/else if/else` chain (GskGpu never falls through a
        //        non-empty case).
        //     5. GskGpu's top-down forward prototypes (`main` → `main_clip_*` → `run`) make naga's validator
        //        reject the `Call`s (ForwardDependency); the parsed module's functions are topologically
        //        reordered (callee-before-caller) with all `Call`/`CallResult` handles remapped.
        //     6. sample-op `if/else if` helpers with no final `else` get a bare `return;` from naga that fails a
        //        value-returning function; each is replaced with a zero-value return of the result type.
        //
        //   FIXED — GskGpu instanced vertex layout: `Device::create_render_pipeline` now succeeds. GskGpu
        //   computes vertex position from `gl_VertexID` (vertex-pulling, no position attribute) and feeds
        //   per-instance `IN()` data (in_rect/in_tex_rect/in_color, ~48 bytes/instance) out of ONE big frame
        //   VBO. With the GL `base-instance` feature UNAVAILABLE (see "base-instance: ✗" above) it BAKES the
        //   per-instance region base (`first_instance * stride`, e.g. instance 542 → 26016) into the
        //   `glVertexAttribPointer` offset, so an attribute offset can hit the tens of thousands. wgpu rejects
        //   an attribute whose `offset + size` exceeds the vertex-buffer `array_stride` (48) — hence the old
        //   "stride 26032 exceeds the limit 48" NACK. hl_wip-gl/src/service/frame.rs now hoists the whole-stride
        //   base out of the attributes into the vertex-buffer BIND offset (`Enc::SetVertexBuffer { offset }`),
        //   leaving each attribute's in-stride offset in `[0, stride)`; the array_stride stays the packed
        //   instance size and the slot steps per-instance. An ordinary draw (every offset already < stride) is
        //   byte-identical.
        //
        //   FIXED — GskGpu bind-group binding alignment: the driver now binds each texture/sampler at the
        //   HOST binding index the compiled shader declares. GskGpu declares each texture as EITHER a
        //   `samplerExternalOES` OR a `sampler2D` triple in two preprocessor branches; the host executor's ES
        //   route (`glsl_es::split_global_samplers`) numbers `layout(binding=)` with a running counter over
        //   EVERY host-recognized sampler decl in text order — counting the inactive `samplerExternalOES`
        //   branches too — so the active `sampler2D GSK_TEXTURE0` lands on binding k=1 (tex 3 / sampler 4), not
        //   k=0. The driver's own reflection skipped the external decls + deduped, numbering GSK_TEXTURE0 as
        //   k=0 (tex 1 / sampler 2); its bind group thus overlapped the shader's live bindings only on the
        //   UBO, and the executor's used-binding filter cut it to 1 entry vs the layout's 3 ("bindings (1) does
        //   not match (3)"). hl_wip-gl now recovers the host `k` per bound sampler
        //   (adapter/glsl.rs `verbatim_sampler_bindings`, stored as `Program::samp_bindings`) and emits the
        //   texture/sampler at `1+2k / 2+2k`, so the driver's entries match naga's auto layout. ES2 programs
        //   (which emit their own `layout(binding=)`) keep the identity `k == declaration index`.
        //
        //   FIXED — GskGpu per-frame shader RECOMPILE (the stall that kept presented_frames at 0): GTK's
        //   GskGL renderer compiles+links each of its ~11 programs ONCE and then reuses them across every
        //   draw of every frame, but the GL driver's frame builder minted fresh IR shader ids and re-emitted
        //   `CreateShader` (+ `CreateRenderPipeline`) for the SAME program on EVERY draw — so the host re-ran
        //   naga's glsl→wgsl compile hundreds of times per frame (one real GTK frame = ~260 draws over ~11
        //   programs → ~549 redundant compiles, 0 failures), burning the whole frame budget and completing
        //   only ~2 submits with 0 presents. hl_wip-gl now has a program-keyed shader/pipeline residency cache
        //   (context.rs `program_shader_ir` / `program_pipeline_ir`, keyed by GL program name + a `link_gen`
        //   bumped on `glLinkProgram`, wired through service/frame.rs `lower_draw`) that mirrors the existing
        //   texture/buffer residency caches: a linked program's two shader modules + its render pipeline are
        //   `CreateShader`d/`CreateRenderPipeline`d ONCE and re-referenced by their stable IR ids on every
        //   later draw+frame. The recompile count drops from ~549 to ~30 (once per program, per link).
        //
        //   FIXED — GskGpu bind-group COMPLETENESS: `Device::create_bind_group` now succeeds. A GskGpu
        //   fragment program declares + samples THREE textures (compiled auto layout = UBO@0 + three
        //   texture/sampler pairs at 1+2k/2+2k = 7 bindings), but for a given draw only some of those
        //   samplers have a real GL texture with uploaded pixels bound; the driver's `lower_draw` USED to
        //   emit a bind-group entry only for a sampler whose texture had data, so a draw with one populated
        //   atlas supplied 3 entries and the 7-binding auto layout NACKed ("bindings (3) does not match (7)").
        //   hl_wip-gl/src/service/frame.rs `lower_draw` now NEVER skips a declared sampler: it binds the
        //   sampler's real bound texture when present, else a shared 1x1 transparent-black PLACEHOLDER
        //   texture + default sampler (created ONCE and cached on the context via
        //   `GlContext::default_placeholder`, mirroring the texture/buffer residency caches) at that sampler's
        //   host binding. The bind group now carries an entry for every declared sampler, so it matches the
        //   layout; the executor's used-binding filter then trims to the shader's sampled subset. With this,
        //   gpu submits progress past the bind-group wall (~10) and the compositor PRESENTS frames (~3).
        //
        //   REMAINING — with the bind group complete, GTK now SUBMITS and the compositor PRESENTS frames, but
        //   the presented pixels are a UNIFORM fill (luminance spread 0 — a flat buffer, not GTK's chrome).
        //   There are NO host validation errors (create_bind_group / pipelines / shaders all clean, 0
        //   shader_errors, 0 NACKs): the frame reaches the GPU and composites, but no GskGpu geometry is
        //   VISIBLE in the result. The next stop is the frame CONTENT / DATA routing, one stage past the now
        //   clean bind group — most likely the GskGpu std140 push-constant UBO + per-instance vertex data:
        //   GskGpu reads `layout(std140, binding = 0) uniform GskPushConstants { mat4 mvp; mat3x4 clip; vec2
        //   scale; } push;` and pulls per-instance geometry (in_rect/in_tex_rect) out of one frame VBO, so if
        //   the driver's flat glUniform*-style uniform block bytes or the instance vertex-attribute routing do
        //   not match that std140 layout, every primitive transforms to a degenerate/offscreen position and
        //   the target keeps only its clear (a uniform frame). This is distinct from — and downstream of —
        //   the now-fixed bind-group completeness; the next effort is the uniform/vertex DATA layout, not the
        //   bind-group binding set.
        eprintln!(
            "MILESTONE DIAGNOSED (GTK4 reaches real GL rendering + cheap frame lowering; its GskGpu shaders \
             compile AND are no longer recompiled per draw, its bind group is now COMPLETE so the host \
             accepts it, GTK now SUBMITS and the compositor PRESENTS frames — but the pixels are still blank, \
             so the next gap has moved one stage downstream to the frame CONTENT / data routing — suite stays \
             green):\n\
             Stage evidence:\n\
             * host GPU executor submits (guest lowered GL IR over $HL_GPU_EXEC): {submit_count}\n\
               (>0 => the epoxy/GskGpu bring-up is LIVE; GTK lowers real GL to the host)\n\
               (0 => a regression in the epoxy context classification / renderer realize — re-check \
                eglQueryContext(EGL_CONTEXT_CLIENT_TYPE) and the GDK_DEBUG=opengl / GSK_DEBUG=renderer lines)\n\
             * frame lowering: FIXED — cross-frame/draw resource caching (context.rs sampled_texture_ir / \
                data_buffer_ir) took a GTK frame from ~4.3 GiB (per-draw atlas re-upload, over every cap) to \
                ~30 MiB, well under the 64 MiB negotiated cap.\n\
             * GskGpu fragment shader compilation: FIXED — the `#version 320 es` GskGpu fragment programs now \
                compile to WGSL (hl_wip-gpu-wgpu glsl_es.rs ES→desktop lowering: __VERSION__ seed + UBO \
                binding, gl_VertexID-in-macro rewrite, IN/PASS location macros, switch→if/else; plus wgsl.rs \
                naga-module passes: topological function reorder + zero-value bare returns).\n\
             * GskGpu instanced vertex layout: FIXED — Device::create_render_pipeline now succeeds. GskGpu \
                bakes the per-instance region base (first_instance*stride, base-instance being unavailable) into \
                the glVertexAttribPointer offset; hl_wip-gl/src/service/frame.rs now hoists that whole-stride \
                base into the vertex-buffer bind offset (SetVertexBuffer offset), keeping each attribute offset \
                within the array_stride (48) and the slot per-instance.\n\
             * GskGpu bind-group binding alignment: FIXED — the driver binds each texture/sampler at the HOST \
                binding index the compiled shader declares. GskGpu declares each texture as a samplerExternalOES \
                OR a sampler2D triple in two #if branches; the host numbers layout(binding=) across ALL branches \
                (counting the inactive external decls), so the active sampler2D GSK_TEXTURE0 is binding k=1 (tex \
                3 / sampler 4), not k=0. The driver skipped the external decls + deduped (k=0), so its bind group \
                overlapped the shader's live bindings only on the UBO and the executor's used-binding filter cut \
                it to 1 entry vs the layout's 3 ('bindings (1) does not match (3)'). hl_wip-gl now recovers the \
                host k per bound sampler (adapter/glsl.rs verbatim_sampler_bindings → Program::samp_bindings) and \
                emits tex/sampler at 1+2k / 2+2k; ES2 programs keep the identity k == declaration index.\n\
             * GskGpu per-frame shader recompile: FIXED — GskGL reuses each of its ~11 programs across every \
                draw of every frame, but the GL driver re-emitted CreateShader (+ CreateRenderPipeline) for the \
                SAME program on EVERY draw, so the host re-ran naga's glsl→wgsl compile ~549 times per run (0 \
                failures) and burned the whole frame budget (~2 submits, 0 presents). hl_wip-gl now caches a \
                linked program's shader modules + pipeline by GL program name + link generation \
                (context.rs program_shader_ir / program_pipeline_ir, wired through service/frame.rs lower_draw), \
                so a reused program compiles ONCE and costs nothing per later draw/frame — recompiles drop to \
                ~30 (once per program/link) and gpu submits progress past the shader wall.\n\
             * GskGpu bind-group COMPLETENESS: FIXED — Device::create_bind_group now succeeds. A GskGpu \
                fragment program declares + samples THREE textures (auto layout = UBO@0 + three tex/sampler \
                pairs at 1+2k/2+2k = 7 bindings), but a given draw populates only some of those samplers; the \
                driver used to emit an entry only for a sampler with uploaded data (3 entries) and the 7-binding \
                layout NACKed ('bindings (3) does not match (7)'). hl_wip-gl's lower_draw now NEVER skips a \
                declared sampler — it binds the real texture when present, else a shared 1x1 transparent-black \
                PLACEHOLDER texture + default sampler (created once, cached via GlContext::default_placeholder) \
                at that sampler's host binding — so the bind group covers every declared sampler and matches the \
                layout; the executor's used-binding filter trims to the sampled subset. gpu submits now progress \
                past the bind-group wall (~10).\n\
             * compositor presented frames: {presented} (best luminance spread {best_spread})\n\
               (the bind group is complete, so GTK now SUBMITS and the compositor PRESENTS frames — but the \
                presented pixels are a UNIFORM fill (spread {best_spread}), not GTK's chrome. There are NO host \
                validation errors (bind group / pipelines / shaders all clean): the frame reaches the GPU and \
                composites, but no GskGpu geometry is VISIBLE. The next stop is the frame CONTENT / DATA routing \
                — most likely the GskGpu std140 push-constant UBO (layout(std140,binding=0) uniform \
                GskPushConstants {{ mat4 mvp; mat3x4 clip; vec2 scale; }}) + the per-instance vertex data: if \
                the driver's flat glUniform*-style uniform bytes or the instance attribute routing do not match \
                that std140 layout, every primitive transforms offscreen/degenerate and the target keeps only \
                its clear. Distinct from — and downstream of — the now-fixed bind-group completeness.)\n\
             --- decisive app stderr lines ---\n{}\n",
            decisive_lines(&stderr),
            presented = frames.len(),
        );
        let _ = std::fs::remove_file(&socket_path);
        panic!(
            "GTK4 composited {presented} frame(s) through the full stack but did NOT render PROPER content \
             (best luminance spread {best_spread}) — the frame is a uniform clear / degenerate geometry, not \
             GTK's real widgets. This milestone is ACHIEVED in-tree (the GskGpu PushConstants MVP is \
             reconstructed in hl_wip-gl service::frame), so a blank frame is a REGRESSION, not a diagnosed \
             gap. See the stage evidence above.",
            presented = frames.len(),
        );
    }

    // ---- 8. ASSERT the captured pixels are the REAL GTK4 app's rendered window -------------------------
    assert!(
        submit_count > 0,
        "GTK produced composited frames but the host executor saw 0 GPU submits — the pixels did not come \
         from our GL lowering path"
    );

    // Pick the frame with the most visual structure (widest luminance spread) — GTK draws a populated window
    // (chrome + widgets), so at least one captured frame carries real content.
    let frame = frames
        .iter()
        .max_by_key(|f| luminance_spread(f))
        .expect("at least one captured frame")
        .clone();
    let (w, h) = (frame.width, frame.height);
    assert!(w > 0 && h > 0, "captured frame has a real size, got {w}x{h}");
    eprintln!("asserting on captured frame: {w}x{h}, serial {}", frame.serial);

    // The window is NOT a uniform fill: GTK's chrome (light headerbar/background) over darker widgets/text
    // yields a wide luminance spread; a blank/clear buffer is near-zero.
    let spread = luminance_spread(&frame);
    assert!(
        spread > 40,
        "the composited frame must carry GTK's window content (non-uniform), but its luminance spread is \
         only {spread} — a blank/flat buffer, not a rendered GTK window"
    );

    // GTK's default (Adwaita light) chrome is a light gray (~0xf6f5f4): some pixels must be bright/light,
    // proving actual GTK chrome composited — not a dim uniform fill or the clear color.
    assert!(
        has_light_chrome(&frame),
        "expected GTK's light-gray chrome (bright light pixels) somewhere in the frame"
    );

    // ---- PROPER CONTENT, not just "non-blank": confront the actual GskGpu geometry -------------------
    // "GTK composites" is NOT "GTK renders correctly". A collapsed GskGpu MVP (`push.mvp == 0`) STILL
    // composites a frame and still carries the light-gray chrome clear — every primitive just transforms
    // onto the origin, so the window is a uniform fill that sneaks past a bare spread/chrome check. So we
    // assert GTK's REAL widgets are present AND land at their expected screen positions, which only holds
    // when the std140 PushConstants MVP actually transforms the gl_VertexID-pulled vertices:
    //   * across the whole toplevel, thousands of Adwaita ACCENT-BLUE pixels (sliders / the color button /
    //     the selected row) and thousands of DARK text-glyph pixels — a rich, varied widget surface, not
    //     the handful a near-uniform clear yields; AND
    //   * for gtk4-widget-factory (which `which_gtk4_app` picks first) at its 1378x774 toplevel, two
    //     distinctive, layout-stable regions carry their real colors: the color-button swatch (~x505,y393)
    //     is solid accent BLUE, and the "Sunset" photo thumbnail (bottom-left, ~x185,y690) is a WARM orange
    //     sunrise — a real sampled TEXTURE, so it also proves textured-node placement.
    let (blue_px, warm_px, dark_px) = adwaita_color_counts(&frame);
    eprintln!("proper-content pixels: accent-blue={blue_px} warm={warm_px} dark-text={dark_px}");
    assert!(
        blue_px > 3000,
        "expected GTK's accent-blue widgets (sliders / color button / selection) spread across the frame, \
         but only {blue_px} accent-blue px — a collapsed GskGpu MVP left the window as its clear"
    );
    assert!(
        dark_px > 1000,
        "expected GTK's dark text glyphs across the frame, but only {dark_px} dark px — no real widget \
         content, just chrome"
    );
    let is_widget_factory = app_bin
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.contains("widget-factory"))
        .unwrap_or(false);
    if is_widget_factory && w == 1378 && h == 774 {
        let swatch = region_avg(&frame, 490, 388, 520, 398);
        assert!(
            swatch.2 > 130 && swatch.2 as i32 > swatch.0 as i32 + 40 && swatch.0 < 120,
            "the color-button swatch at (~505,393) must be Adwaita accent BLUE, got RGB {swatch:?} — the \
             GskGpu geometry is mis-placed/collapsed (this region should be a solid blue rectangle)"
        );
        let sunset = region_avg(&frame, 150, 675, 220, 710);
        assert!(
            sunset.0 > 150 && sunset.0 as i32 > sunset.2 as i32 + 60 && sunset.2 < 130,
            "the Sunset photo thumbnail at (~185,690) must be WARM orange, got RGB {sunset:?} — the textured \
             node is mis-placed/collapsed (this region should be a sampled sunrise image)"
        );
        assert!(
            warm_px > 3000,
            "expected widget-factory's warm Sunset photo, but only {warm_px} warm px"
        );
    }

    // A real, viewable PNG of the composited app frame was written.
    let png = png_dir.join(format!("frame-{}.png", frame.serial));
    assert!(png.exists(), "a real PNG of the composited GTK frame was written at {png:?}");
    let mut surfaces: Vec<u32> = frames.iter().map(|f| f.surface.0).collect();
    surfaces.sort_unstable();
    surfaces.dedup();
    eprintln!(
        "MILESTONE PASSED: real GTK4 app composited through the full stack.\n\
         PNG: {}\n  frames captured: {}, toplevels: {}, gpu submits: {}, adapter: {adapter}, spread: {spread}",
        png.display(),
        frames.len(),
        surfaces.len(),
        submit_count,
    );

    let _ = std::fs::remove_file(&socket_path);
}

/// The luminance spread (max minus min per-pixel R+G+B) across the frame — near-zero for a flat/blank
/// buffer, wide for a real rendered GTK window (light chrome over darker widgets/text).
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

/// Whether some pixel carries GTK's light-gray chrome (all channels high — a light, near-white region),
/// proving real GTK window chrome rather than a dark/dim uniform fill.
fn has_light_chrome(f: &CapturedFrame) -> bool {
    f.rgba
        .chunks_exact(4)
        .any(|p| p[0] >= 200 && p[1] >= 200 && p[2] >= 200)
}

/// Count the distinctive Adwaita colors across the frame: `(accent_blue, warm, dark)`. Accent blue is GTK's
/// selection/slider/button highlight (blue dominant over red+green); warm is the reddish-orange of a photo
/// (red dominant over green+blue, e.g. widget-factory's Sunset image); dark is near-black text glyphs. A
/// properly MVP-transformed GTK window has thousands of each; a collapsed-MVP uniform clear has ~none.
fn adwaita_color_counts(f: &CapturedFrame) -> (u32, u32, u32) {
    let (mut blue, mut warm, mut dark) = (0u32, 0u32, 0u32);
    for p in f.rgba.chunks_exact(4) {
        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
        if b > 120 && b - r > 40 && b - g > 25 {
            blue += 1;
        }
        if r > 120 && r - b > 40 && r - g > 20 {
            warm += 1;
        }
        if r + g + b < 90 {
            dark += 1;
        }
    }
    (blue, warm, dark)
}

/// The average `(r, g, b)` over the pixel box `[x0,x1) × [y0,y1)` of the frame (clamped to its extent) — used
/// to assert a KNOWN widget lands at a KNOWN screen position with its real color (only true when the GskGpu
/// MVP transforms the geometry correctly).
fn region_avg(f: &CapturedFrame, x0: i32, y0: i32, x1: i32, y1: i32) -> (u32, u32, u32) {
    let (w, h) = (f.width, f.height);
    let (mut r, mut g, mut b, mut n) = (0u64, 0u64, 0u64, 0u64);
    for y in y0..y1.min(h) {
        for x in x0..x1.min(w) {
            let o = ((y * w + x) * 4) as usize;
            if o + 2 < f.rgba.len() {
                r += f.rgba[o] as u64;
                g += f.rgba[o + 1] as u64;
                b += f.rgba[o + 2] as u64;
                n += 1;
            }
        }
    }
    if n == 0 {
        return (0, 0, 0);
    }
    ((r / n) as u32, (g / n) as u32, (b / n) as u32)
}

/// Extract the lines from the app's stderr most likely to name the decisive stop (EGL/GL/GDK/GSK/epoxy
/// errors), so the diagnosis report leads with signal rather than the full log.
fn decisive_lines(stderr: &str) -> String {
    let mut hits: Vec<&str> = stderr
        .lines()
        .filter(|l| {
            let s = l.to_lowercase();
            s.contains("egl")
                || s.contains("gl error")
                || s.contains("glerror")
                || s.contains("opengl")
                || s.contains("gsk")
                || s.contains("gdk")
                || s.contains("epoxy")
                || s.contains("renderer")
                || s.contains("fail")
                || s.contains("error")
                || s.contains("unimpl")
                || s.contains("not implemented")
                || s.contains("no provider")
                || s.contains("context")
        })
        .collect();
    if hits.is_empty() {
        hits = stderr.lines().rev().take(20).collect();
        hits.reverse();
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

/// Locate a real GTK4 example app. Prefer `gtk4-widget-factory` (maps a densely-populated window with lots
/// of chrome immediately), then `gtk4-demo` (sidebar + headerbar), then `gtk4-demo-application`.
fn which_gtk4_app() -> Option<PathBuf> {
    let dirs: Vec<PathBuf> =
        std::env::var("PATH").unwrap_or_default().split(':').map(PathBuf::from).collect();
    for name in ["gtk4-widget-factory", "gtk4-demo", "gtk4-demo-application"] {
        for dir in &dirs {
            let p = dir.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}
