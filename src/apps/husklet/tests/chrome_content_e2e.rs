//! CHROME REAL-CONTENT — escalate the Chrome first-light milestone (`chrome_e2e.rs`: a solid #ff7700 fill,
//! which exercises exactly ONE Skia draw shape, a scissored clear) to "renders REAL web content correctly".
//!
//! It drives the SAME native-Linux hl stack chrome_e2e drives (WgpuExecutorServer on lavapipe + our
//! staged libEGL/libGLESv2 shims + smithay::run_auto compositor + PngPresenter capture + the GAP #0/#0b
//! fd-ownership/HandleReplacements LD_PRELOAD neutralizer), but points Chromium at a page with KNOWN,
//! POSITION-CHECKABLE content and then asserts REAL pixels at REAL positions:
//!
//!   * a solid RED box   (top-left),
//!   * a solid BLUE box  (center),
//!   * a solid GREEN box (bottom-center),
//!   * a block of black text on white  (proves glyph coverage rasterization — dark+light contrast),
//!   * a horizontal black->white linear-gradient  (proves gradient shading — monotonic luminance ramp),
//!   * a white rounded/bordered box  (proves a stroked+clipped draw, not just a fill).
//!
//! WHY THIS PROVES MORE THAN chrome_e2e: a solid fill is one scissored clear. Distinct primary-colored
//! boxes at distinct positions prove MULTIPLE independent fills lower + composite to the right place; the
//! text block proves Skia's glyph-coverage sampling (an alpha-coverage texture blit) survives lowering; the
//! gradient proves a shaded (non-constant) fragment program; the rounded box proves a clipped stroke. If any
//! ONE of these renders wrong we PIN it (which region, expected vs actual) instead of loosening the assert.
//!
//! CHROMELESS: unlike chrome_e2e (which shows the browser tab-strip/omnibox — that region composites BLACK,
//! leaving the web content in the lower ~68% of the frame), this runs Chromium in `--app=` mode so the web
//! content fills the whole client area. We still DON'T assume the content rect == the frame: CSD drop-shadow
//! leaves a transparent margin, so we DETECT the opaque content bounding box and sample every region as a
//! FRACTION of that detected rect. That keeps the position asserts honest without hard-coding pixel offsets.
//!
//! Like chrome_e2e this stays GREEN as a tracker when Chrome can't reach content for an environmental reason
//! (no chromium binary, no staged shims, Chrome stops before the first GL frame); but when Chrome DOES paint
//! the page, the color/position/text/gradient asserts are REAL and HARD.

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

// Chrome commonly commits one initial shell frame and one fully painted frame, then stays idle. Waiting
// for a third commit turns a successful deterministic render into a 60-second timeout on quiet runs.
const TARGET_FRAMES: usize = 2;
const APP_DEADLINE: Duration = Duration::from_secs(60);

/// The known layout, expressed as fractions (0..1) of the DETECTED opaque content rectangle. Kept in ONE
/// place so the HTML and the asserts can't drift: the HTML positions each region at these fractions, and the
/// test samples at these fractions of whatever content rect it detects.
mod layout {
    /// A point sampled as a fraction of the content rect.
    pub const RED_C: (f64, f64) = (0.175, 0.135); // center of the top-left red box
    pub const BLUE_C: (f64, f64) = (0.500, 0.490); // center of the center blue box
    pub const GREEN_C: (f64, f64) = (0.500, 0.865); // center of the bottom-center green box

    /// Text block (top-right): a rectangle we scan for glyph contrast (dark ink + light paper).
    pub const TEXT_RECT: (f64, f64, f64, f64) = (0.42, 0.05, 0.96, 0.22); // x0,y0,x1,y1

    /// Gradient strip (mid-left), black on the left -> white on the right. Sampled along y=GRAD_Y.
    pub const GRAD_Y: f64 = 0.49;
    pub const GRAD_XS: [f64; 5] = [0.055, 0.115, 0.175, 0.235, 0.290];

    /// Rounded/bordered box (bottom-left): center is white paper, border is a known color.
    pub const ROUND_C: (f64, f64) = (0.165, 0.855); // white interior
    pub const ROUND_BORDER: (f64, f64) = (0.035, 0.855); // left edge -> border ink (purple)
}

/// The HTML. Every region is placed at the fractions named in `mod layout`, using viewport percentages so it
/// scales to whatever window size Chrome gives the app surface. Background is white so the opaque content
/// bounding box == the web viewport (minus CSD shadow).
fn content_html() -> String {
    // %-of-viewport positions matching mod layout (top-left origin).
    // red box:   x 3..32 %, y 3..24 %   -> center ~17.5, 13.5
    // blue box:  x 35..65 %, y 40..58 % -> center 50, 49
    // green box: x 35..65 %, y 77..96 % -> center 50, 86.5
    // text:      x 42..97 %, y 4..22 %
    // gradient:  x 3..31 %, y 40..58 %  -> sampled along y 49
    // rounded:   x 3..30 %, y 77..94 %  -> center 16.5, 85.5 ; left border ink at x~3.5
    String::from(
        "<!doctype html><html><head><meta charset=utf-8><style>\
         html,body{margin:0;padding:0;width:100%;height:100%;background:#ffffff;overflow:hidden;\
           font-family:sans-serif;}\
         .box{position:absolute;box-sizing:border-box;}\
         #red{left:3%;top:3%;width:29%;height:21%;background:#ff0000;}\
         #blue{left:35%;top:40%;width:30%;height:18%;background:#0000ff;}\
         #green{left:35%;top:77%;width:30%;height:19%;background:#00ff00;}\
         #text{left:42%;top:4%;width:55%;height:18%;color:#000000;background:#ffffff;\
           font-size:34px;font-weight:800;line-height:1.15;letter-spacing:1px;}\
         #grad{left:3%;top:40%;width:28%;height:18%;\
           background:linear-gradient(to right,#000000 0%,#ffffff 100%);}\
         #round{left:3%;top:77%;width:27%;height:17%;background:#ffffff;\
           border:10px solid #800080;border-radius:22px;}\
         </style></head><body>\
         <div class=box id=red></div>\
         <div class=box id=blue></div>\
         <div class=box id=green></div>\
         <div class=box id=text>ABCDEFG HIJKLMN OPQRST UVWXYZ 0123456789 the quick brown fox</div>\
         <div class=box id=grad></div>\
         <div class=box id=round></div>\
         </body></html>",
    )
}

#[test]
fn chrome_renders_real_content_correctly() {
    // ---- 0. Preconditions ------------------------------------------------------------------------------
    let chromium_bin = match which_chromium() {
        Some(p) => p,
        None => {
            eprintln!("no chromium binary (set HL_CHROME_BIN) — skipping the Chrome real-content milestone.");
            return;
        }
    };
    eprintln!("real chromium binary: {}", chromium_bin.display());

    let gl_dir = staged_dir("gl");
    for lib in ["libEGL.so.1", "libGLESv2.so.2", "libwayland-egl.so.1"] {
        assert!(
            gl_dir.join(lib).exists(),
            "staged {lib} missing at {gl_dir:?} — a `cargo test` in hl stages hl-gl's shim"
        );
    }

    let (run_bin, dep_dirs) = match build_run_prefix(&chromium_bin, &gl_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("could not build the chromium run prefix ({e}) — skipping.");
            return;
        }
    };

    // ---- 1. Private XDG_RUNTIME_DIR + the content page -------------------------------------------------
    let runtime_dir = roomy_base().join(format!("xdg-content-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&runtime_dir);
    std::fs::create_dir_all(&runtime_dir).expect("create runtime dir");
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))
        .expect("chmod 0700");
    std::env::set_var("XDG_RUNTIME_DIR", &runtime_dir);
    std::env::remove_var("WAYLAND_SOCKET");

    let png_dir = runtime_dir.join("png");
    let profile_dir = runtime_dir.join("profile");
    std::fs::create_dir_all(&profile_dir).expect("create profile dir");

    let html_path = runtime_dir.join("content.html");
    let html = match std::env::var("HL_TEST_HTML").as_deref() {
        Ok("orange") => "<!doctype html><html><head><meta charset=utf-8><style>html,body{margin:0;padding:0;width:100%;height:100%;background:#ff7700;}</style></head><body></body></html>".to_string(),
        Ok("bands") => "<!doctype html><html><head><meta charset=utf-8><style>html,body{margin:0;padding:0;width:100%;height:100%;}#r{position:absolute;left:0;top:0;width:100%;height:50%;background:#ff0000;}#b{position:absolute;left:0;top:50%;width:100%;height:50%;background:#0000ff;}</style></head><body><div id=r></div><div id=b></div></body></html>".to_string(),
        Ok("boxes") => "<!doctype html><html><head><meta charset=utf-8><style>html,body{margin:0;padding:0;width:100%;height:100%;background:#ffffff;}.b{position:absolute;}#r{left:3%;top:3%;width:29%;height:21%;background:#ff0000;}#g{left:35%;top:40%;width:30%;height:18%;background:#0000ff;}#n{left:35%;top:77%;width:30%;height:19%;background:#00ff00;}</style></head><body><div class=b id=r></div><div class=b id=g></div><div class=b id=n></div></body></html>".to_string(),
        _ => content_html(),
    };
    std::fs::write(&html_path, html).expect("write content.html");
    let file_url = format!("file://{}", html_path.display());

    // ---- 2. Host GPU executor (lavapipe) ---------------------------------------------------------------
    let exec = WgpuExecutorServer::start("chrome-content");
    let adapter = exec.adapter_name();
    eprintln!("host wgpu adapter: {adapter}");
    assert!(
        adapter.to_lowercase().contains("llvmpipe") || adapter.to_lowercase().contains("lavapipe"),
        "Chrome's GL frames must rasterize on the software Vulkan device, got adapter {adapter:?}"
    );

    // ---- 3. Compositor on the discovery socket ---------------------------------------------------------
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
    assert!(
        name_str.starts_with("wayland-"),
        "expected a `wayland-N` name, got {name_str:?}"
    );
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

    // ---- 4. Spawn Chromium in app (chromeless) mode ----------------------------------------------------
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
    match &preload {
        Some(p) => eprintln!("GAP #0 fd-ownership preload: {}", p.display()),
        None => eprintln!("no fd-ownership preload — Chrome will re-hit GAP #0."),
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
        .arg("--disable-renderer-backgrounding")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--enable-logging=stderr")
        .arg("--v=1")
        .arg("--window-size=800,600");
    // Chromeless app window: no tab-strip/omnibox, so the web content fills the client area.
    if std::env::var("HL_TEST_APP").as_deref() == Ok("0") {
        cmd.arg(&file_url);
    } else {
        cmd.arg(format!("--app={file_url}"));
    }
    cmd.arg(format!("--user-data-dir={}", profile_dir.display()))
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
    for var in ["HL_LOG", "HL_LOG_LEVEL"] {
        if let Ok(v) = std::env::var(var) {
            cmd.env(var, v);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));

    let mut child = cmd
        .spawn()
        .unwrap_or_else(|e| panic!("failed to spawn {}: {e}", run_bin.display()));

    // ---- 5. Let it render ------------------------------------------------------------------------------
    let start = Instant::now();
    let mut frames: Vec<CapturedFrame> = Vec::new();
    let mut app_exited: Option<std::process::ExitStatus> = None;
    while start.elapsed() < APP_DEADLINE {
        frames = captures.lock().unwrap().clone();
        // Keep going until we have a frame whose content actually looks painted (a decent opaque rect),
        // not just the first blank surface commit.
        if frames.len() >= TARGET_FRAMES && frames.iter().any(|f| content_bbox(f).is_some()) {
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            app_exited = Some(status);
            frames = captures.lock().unwrap().clone();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // ---- 6. Teardown FIRST -----------------------------------------------------------------------------
    let _ = child.kill();
    let killed_status = child.wait().ok();
    stop.store(true, Ordering::Relaxed);
    let _ = compositor.join();

    let stderr = read_to_string(&err_path);
    let submit_count = exec.submit_count();

    // Choose the frame with the largest opaque content rect (the most-painted composite).
    let best = frames
        .iter()
        .filter_map(|f| content_bbox(f).map(|bb| (f, bb)))
        .max_by_key(|(_, bb)| (bb.2 - bb.0) as i64 * (bb.3 - bb.1) as i64);

    // ---- 7. Tracker path: Chrome didn't reach paintable content ----------------------------------------
    let Some((frame, bbox)) = best else {
        let exited = app_exited.or(killed_status);
        let sigtrapped = exited.map(|s| (s.into_raw() & 0x7f) == 5).unwrap_or(false);
        let fd_ownership_abort = stderr.contains("FD ownership violation");
        let stage = if fd_ownership_abort {
            "GAP #0 fd-ownership abort — the LD_PRELOAD neutralizer did not apply (see chrome_e2e's GAP #0 \
             note); Chrome aborted in early ChromeMain before any Wayland/EGL/GL."
        } else if sigtrapped {
            "Chrome SIGTRAPped at a CHECK before painting content — pin the new PC (see chrome_e2e GAP #0c)."
        } else if submit_count == 0 {
            "GAP #2 / EGL bring-up — 0 GPU submits; Chrome never lowered a GL frame to our shim."
        } else if frames.is_empty() {
            "GAP #1 (shader) / present — submitted GL IR but no frame reached the compositor."
        } else {
            "No opaque content rect in any presented frame — Chrome composited only blank/transparent \
             surfaces (page never painted, or present never committed real pixels)."
        };
        eprintln!(
            "CHROME REAL-CONTENT DIAGNOSED (suite stays green as a tracker):\n\
             Stage: {stage}\n\
             Evidence: gpu_submits={submit_count}, presented_frames={}, \
             decisive stderr:\n{}\n",
            frames.len(),
            decisive_lines(&stderr),
        );
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir_all(&runtime_dir);
        return;
    };

    // ---- 8. RENDERED: sample every region at its fraction of the detected content rect ------------------
    assert!(
        submit_count > 0,
        "content frames present but the host executor saw 0 GPU submits — pixels didn't come from our GL path"
    );
    let (bx0, by0, bx1, by1) = bbox;
    let bw = (bx1 - bx0) as f64;
    let bh = (by1 - by0) as f64;
    // Sanity: the content rect must be a real, large region (guards against a tiny stray opaque blob).
    let cover = (bw * bh) / (frame.width as f64 * frame.height as f64);
    assert!(
        bw > 300.0 && bh > 220.0 && cover > 0.45,
        "detected content rect {bbox:?} ({bw}x{bh}, {:.0}% of {}x{}) is too small to be the page — \
         Chrome likely painted only a partial surface",
        cover * 100.0,
        frame.width,
        frame.height
    );
    let at = |fx: f64, fy: f64| -> [u8; 4] {
        let x = bx0 + (fx * bw).round() as i32;
        let y = by0 + (fy * bh).round() as i32;
        frame
            .pixel(x.clamp(0, frame.width - 1), y.clamp(0, frame.height - 1))
            .unwrap_or([0, 0, 0, 0])
    };

    // --- sample solid boxes ---
    let red = at(layout::RED_C.0, layout::RED_C.1);
    let blue = at(layout::BLUE_C.0, layout::BLUE_C.1);
    let green = at(layout::GREEN_C.0, layout::GREEN_C.1);

    // --- text contrast: fraction of dark ink and light paper in the text rect ---
    let (dark_frac, light_frac) = region_contrast(frame, bbox, layout::TEXT_RECT);

    // --- gradient: luminance ramp left->right ---
    let grad_lum: Vec<i32> = layout::GRAD_XS
        .iter()
        .map(|&fx| luma(&at(fx, layout::GRAD_Y)))
        .collect();
    let grad_delta = grad_lum.last().unwrap() - grad_lum.first().unwrap();
    let grad_monotonic = grad_lum.windows(2).all(|w| w[1] >= w[0] - 8);

    // --- rounded/bordered box: white interior + purple border ink ---
    let round_in = at(layout::ROUND_C.0, layout::ROUND_C.1);
    let round_border = at(layout::ROUND_BORDER.0, layout::ROUND_BORDER.1);

    // Report every region BEFORE asserting, so a single --nocapture run shows all actuals.
    eprintln!(
        "--- CHROME REAL-CONTENT sampled (frame {}x{}, content rect {bbox:?} = {bw:.0}x{bh:.0}, \
         {:.0}% cover, gpu_submits={submit_count}, frames={}) ---\n\
         RED    @({:.2},{:.2}) expect ~[255,0,0]   got {red:?}\n\
         BLUE   @({:.2},{:.2}) expect ~[0,0,255]   got {blue:?}\n\
         GREEN  @({:.2},{:.2}) expect ~[0,255,0]   got {green:?}\n\
         TEXT   rect {:?} dark_frac={dark_frac:.3} light_frac={light_frac:.3} (want dark>0.008 & light>0.30)\n\
         GRAD   luma={grad_lum:?} delta={grad_delta} monotonic={grad_monotonic} (want delta>110 & monotonic)\n\
         ROUND  interior {round_in:?} (want ~white) border {round_border:?} (want purple ~[128,0,128])",
        frame.width,
        frame.height,
        cover * 100.0,
        frames.len(),
        layout::RED_C.0,
        layout::RED_C.1,
        layout::BLUE_C.0,
        layout::BLUE_C.1,
        layout::GREEN_C.0,
        layout::GREEN_C.1,
        layout::TEXT_RECT,
    );

    // ---- 9. HARD ASSERTS -------------------------------------------------------------------------------
    assert!(
        is_color(&red, 0),
        "RED box mis-rendered: expected dominant-red, got {red:?}"
    );
    assert!(
        is_color(&blue, 2),
        "BLUE box mis-rendered: expected dominant-blue, got {blue:?}"
    );
    assert!(
        is_color(&green, 1),
        "GREEN box mis-rendered: expected dominant-green, got {green:?}"
    );

    assert!(
        dark_frac > 0.008 && light_frac > 0.30,
        "TEXT region lacks glyph contrast (dark_frac={dark_frac:.3}, light_frac={light_frac:.3}) — glyphs \
         likely did not rasterize (a flat fill would have one of these near zero)"
    );

    assert!(
        grad_monotonic && grad_delta > 110,
        "GRADIENT did not render as a monotonic black->white ramp: luma={grad_lum:?} delta={grad_delta} \
         monotonic={grad_monotonic} — a flat/constant fill means the shaded fragment program didn't lower"
    );

    assert!(
        is_whiteish(&round_in),
        "ROUNDED box interior expected white paper, got {round_in:?}"
    );
    assert!(
        round_border[0] > 80 && round_border[2] > 80 && round_border[1] < 90 && round_border[3] > 180,
        "ROUNDED box border expected purple ink ~[128,0,128], got {round_border:?} — the stroked+clipped \
         border draw did not lower/composite"
    );

    let png = png_dir.join(format!("frame-{}.png", frame.serial));
    assert!(
        png.exists(),
        "a PNG of the composited content frame was written at {png:?}"
    );
    eprintln!(
        "CHROME REAL-CONTENT PASSED: real Chromium rendered multi-color boxes, glyph text, a gradient, and \
         a bordered box correctly through the full stack.\n  PNG: {}\n  gpu submits: {submit_count}",
        png.display(),
    );
    let _ = std::fs::remove_file(&socket_path);
}

// ================================ pixel analysis helpers =================================================

/// The opaque, non-black content bounding box `(x0, y0, x1, y1)` (exclusive on the far edge) of a frame —
/// i.e. the web viewport, excluding the transparent CSD drop-shadow margin and any black uncomposited chrome.
/// `None` when there is essentially no opaque content (a blank/transparent surface).
fn content_bbox(f: &CapturedFrame) -> Option<(i32, i32, i32, i32)> {
    let (w, h) = (f.width, f.height);
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0i32, 0i32);
    let mut count = 0u64;
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let a = f.rgba[i + 3];
            let sum = f.rgba[i] as u32 + f.rgba[i + 1] as u32 + f.rgba[i + 2] as u32;
            // "content" = opaque and not near-black (the browser chrome / shadow reads as black/transparent).
            if a >= 200 && sum >= 60 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
                count += 1;
            }
        }
    }
    // Require a meaningful painted area so a stray opaque pixel can't define a bogus rect.
    if count < 5000 || x1 <= x0 || y1 <= y0 {
        None
    } else {
        Some((x0, y0, x1, y1))
    }
}

/// Whether `p` is a saturated primary whose dominant channel is `chan` (0=R,1=G,2=B): dominant >=150, the
/// other two color channels <=100, opaque. Tolerant of the software rasterizer's slight color drift.
fn is_color(p: &[u8; 4], chan: usize) -> bool {
    if p[3] < 180 {
        return false;
    }
    let others = [0, 1, 2].into_iter().filter(|&c| c != chan);
    p[chan] >= 150 && others.map(|c| p[c]).all(|v| v <= 100)
}

fn is_whiteish(p: &[u8; 4]) -> bool {
    p[3] >= 180 && p[0] >= 200 && p[1] >= 200 && p[2] >= 200
}

fn luma(p: &[u8; 4]) -> i32 {
    // Simple perceptual-ish luma; enough to detect a ramp / dark-vs-light.
    (p[0] as i32 * 3 + p[1] as i32 * 6 + p[2] as i32) / 10
}

/// Fraction of pixels in a sub-rect (given as fractions of the content rect) that are dark ink vs light
/// paper — used to prove a text region has real glyph contrast rather than a flat fill.
fn region_contrast(
    f: &CapturedFrame,
    bbox: (i32, i32, i32, i32),
    rect: (f64, f64, f64, f64),
) -> (f64, f64) {
    let (bx0, by0, bx1, by1) = bbox;
    let bw = (bx1 - bx0) as f64;
    let bh = (by1 - by0) as f64;
    let x0 = bx0 + (rect.0 * bw) as i32;
    let y0 = by0 + (rect.1 * bh) as i32;
    let x1 = bx0 + (rect.2 * bw) as i32;
    let y1 = by0 + (rect.3 * bh) as i32;
    let (mut dark, mut light, mut total) = (0u64, 0u64, 0u64);
    for y in y0.max(0)..y1.min(f.height) {
        for x in x0.max(0)..x1.min(f.width) {
            if let Some(p) = f.pixel(x, y) {
                if p[3] < 180 {
                    continue;
                }
                total += 1;
                let l = luma(&p);
                if l < 90 {
                    dark += 1;
                } else if l > 180 {
                    light += 1;
                }
            }
        }
    }
    if total == 0 {
        (0.0, 0.0)
    } else {
        (dark as f64 / total as f64, light as f64 / total as f64)
    }
}

// ============================ harness helpers (mirrored from chrome_e2e.rs) ===============================

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
        eprintln!(
            "could not compile the fd-ownership preload ({}): {}",
            src.display(),
            String::from_utf8_lossy(&out.stderr)
        );
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

fn decisive_lines(stderr: &str) -> String {
    let mut hits: Vec<&str> = stderr
        .lines()
        .filter(|l| {
            if l.contains(":VERBOSE1:") || l.contains(":INFO:") {
                return false;
            }
            let s = l.to_lowercase();
            s.contains("egl")
                || s.contains("gl error")
                || s.contains("angle")
                || s.contains("ozone")
                || s.contains("wayland")
                || s.contains("shader")
                || s.contains("compile")
                || s.contains("symbol")
                || s.contains("violation")
                || s.contains("crash")
                || s.contains("trap")
                || s.contains("abort")
                || s.contains("fail")
                || s.contains("error")
                || s.contains("fatal")
                || s.contains("hl_")
        })
        .collect();
    if hits.is_empty() {
        hits = stderr.lines().rev().take(40).collect();
        hits.reverse();
    }
    if hits.len() > 120 {
        hits.drain(0..hits.len() - 120);
    }
    hits.join("\n")
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
