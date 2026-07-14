#![cfg(target_os = "macos")] // drives the real macOS GPU mach bridge (hl_display::metal::*); no-op on Linux
//! End-to-end proof with a REAL (engine-format, not test-forced) allocation generation. Row 3 wants a
//! genuinely-recycled id rejected; this wires the real pieces together on macOS:
//!
//!   1. start the real GPU mach bridge;
//!   2. a C helper (`gui_dmabuf_gen_sender.c`, the engine's `hl_gpu_send_port` in miniature) creates a
//!      real IOSurface and sends its send-right + id + a real generation over the mach ABI;
//!   3. the compositor's authenticated metadata for that id is resolved from the REAL IOSurface via
//!      `hl_display::metal::iosurface_metadata` (no mock generation);
//!   4. the real C guest probe drives the `zwp_linux_dmabuf_v1` import handshake against that id: a
//!      modifier carrying a STALE generation is rejected (`params.failed`), while the id's real live
//!      generation imports (`params.created`).
//!
//! So the generation the compositor authenticates against is the one that actually crossed the mach
//! ABI, not a value a test presenter fabricated. macOS-only; skips without a C toolchain.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_compositor::{ClientState, HlState};
use hl_display::present::{IOSurfaceMetadata, PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use smithay::reexports::wayland_server::Display;

/// Resolves IOSurface-backed imports from the REAL host cache (the mach-bridge-populated registry), so
/// the authenticated generation is the engine-supplied one — not a test constant.
struct MetalDelegatingPresenter;
impl Presenter for MetalDelegatingPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        Ok(PresentOutcome::Delivered { serial: 0, timing: None })
    }
    fn frame_count(&self) -> u32 {
        0
    }
    fn iosurface_metadata(&self, id: u32) -> Option<IOSurfaceMetadata> {
        hl_display::metal::iosurface_metadata(id)
    }
}

fn guest(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../hl-jit-darwin/testdata/guests/gui_matrix").join(name)
}

fn cc(src: &Path, out: &Path, extra: &[&str]) -> Option<PathBuf> {
    if !src.exists() {
        return None;
    }
    let mut c = Command::new("cc");
    c.arg(src).args(extra).arg("-O1").arg("-o").arg(out);
    c.status().map(|s| s.success()).unwrap_or(false).then(|| out.to_path_buf())
}

#[test]
fn stale_engine_generation_is_rejected_and_the_live_one_imports() {
    let tmp = std::env::temp_dir().join(format!("hl-stale-engine-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let sender = cc(
        &guest("gui_dmabuf_gen_sender.c"),
        &tmp.join("sender"),
        &["-framework", "CoreFoundation", "-framework", "IOSurface"],
    );
    let probe = cc(&guest("gui_dmabuf_stale_generation_guest.c"), &tmp.join("probe"), &[]);
    let (Some(sender), Some(probe)) = (sender, probe) else {
        return; // no toolchain: skip
    };

    std::env::set_var("HL_DISPLAY_DMABUF", "1");
    hl_compositor::gpu::set_executor_health(true);
    let bridge = format!("com.hl.display.gpu.test.{}", std::process::id());
    std::env::set_var("HL_GPU_BRIDGE_NAME", &bridge);
    assert!(hl_display::metal::start_gpu_bridge(), "mach bridge register");

    // (2) Send a real IOSurface + a real generation over the mach ABI; learn the id it minted.
    const GEN: u32 = 9; // the id's live allocation generation
    let out = Command::new(&sender).arg(GEN.to_string()).env("HL_GPU_BRIDGE_NAME", &bridge).output().unwrap();
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "sender failed: {so:?}");
    let id: u32 = so
        .split_whitespace()
        .find_map(|t| t.strip_prefix("id="))
        .and_then(|v| v.parse().ok())
        .expect("sender id");

    // (3) Wait for the real generation to land in the compositor's authenticated metadata.
    let deadline = Instant::now() + Duration::from_secs(5);
    while hl_display::metal::iosurface_generation(id) != GEN {
        assert!(Instant::now() < deadline, "engine generation never reached the compositor");
        std::thread::sleep(Duration::from_millis(20));
    }

    // (4) Drive the import handshake for this real id.
    let mut display: Display<HlState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = HlState::new(dh.clone(), Box::new(MetalDelegatingPresenter));
    let sock_name = "wayland-stale-engine";
    let sock_path = tmp.join(sock_name);
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).unwrap();
    listener.set_nonblocking(true).unwrap();

    let mut run = |generation: u32| -> String {
        let mut child = Command::new(&probe)
            .arg(generation.to_string())
            .arg(id.to_string())
            .env("XDG_RUNTIME_DIR", &tmp)
            .env("WAYLAND_DISPLAY", sock_name)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut accepted = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if !accepted {
                if let Ok((stream, _)) = listener.accept() {
                    stream.set_nonblocking(true).unwrap();
                    dh.insert_client(stream, Arc::new(ClientState::default())).unwrap();
                    accepted = true;
                }
            }
            let _ = display.dispatch_clients(&mut state);
            let _ = display.flush_clients();
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            assert!(Instant::now() < deadline, "probe (gen={generation}) hung");
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut o = String::new();
        if let Some(mut s) = child.stdout.take() {
            use std::io::Read;
            let _ = s.read_to_string(&mut o);
        }
        assert!(status.success(), "probe (gen={generation}) exit {:?}", status.code());
        o
    };

    let stale = run(GEN - 1);
    assert!(stale.contains("result=failed"), "stale engine generation must be rejected: {stale:?}");
    let live = run(GEN);
    assert!(live.contains("result=created"), "the live engine generation must import: {live:?}");

    let _ = std::fs::remove_file(&sock_path);
    hl_compositor::gpu::set_executor_health(false);
}
