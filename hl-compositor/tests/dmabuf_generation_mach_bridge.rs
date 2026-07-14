#![cfg(target_os = "macos")] // drives the real macOS GPU mach bridge (hl_display::metal::*); no-op on Linux
//! Engine→compositor allocation-generation channel proof. The generation the engine stamps on an
//! IOSurface (dd-jit-darwin vfs.c `hl_gpu_alloc`/`hl_gpu_send_port`) must reach the compositor over the
//! GPU mach bridge and become the id's authenticated `IOSurfaceMetadata::generation`. This starts the
//! REAL bridge (`hl_display::metal::start_gpu_bridge`) and runs a C helper that sends the SAME
//! `hl_gpu_msg_t` the engine sends — a real IOSurface send-right + id + generation — then asserts
//! `hl_display::metal::iosurface_generation(id)` reports exactly that generation. Together with
//! `dmabuf_stale_generation_bridge` (which proves the compositor rejects a mismatched generation over
//! the wire), this closes the loop: real engine generation → real mach → real compositor authentication.
//!
//! macOS-only (IOSurface + mach); skips if no C toolchain.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn sender_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hl-jit-darwin/testdata/guests/gui_matrix/gui_dmabuf_gen_sender.c")
}

fn build_sender(out: &Path) -> Option<PathBuf> {
    let src = sender_src();
    if !src.exists() {
        return None;
    }
    let bin = out.join("gui_dmabuf_gen_sender");
    let ok = Command::new("cc")
        .arg(&src)
        .args(["-framework", "CoreFoundation", "-framework", "IOSurface", "-O1", "-o"])
        .arg(&bin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then_some(bin)
}

#[test]
fn engine_supplied_generation_reaches_the_compositor_over_the_mach_bridge() {
    let tmp = std::env::temp_dir().join(format!("dd-gen-mach-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let Some(sender) = build_sender(&tmp) else {
        return; // no toolchain: skip
    };

    // A per-process bridge name so concurrent test binaries don't collide on the bootstrap service.
    let bridge = format!("com.dd.display.gpu.test.{}", std::process::id());
    std::env::set_var("DD_GPU_BRIDGE_NAME", &bridge);
    assert!(
        hl_display::metal::start_gpu_bridge(),
        "GPU mach bridge failed to register under {bridge}"
    );

    const GENERATION: u32 = 0x1234; // a distinctive non-trivial value (within the 15-bit field)
    let out = Command::new(&sender)
        .arg(GENERATION.to_string())
        .env("DD_GPU_BRIDGE_NAME", &bridge)
        .output()
        .expect("run gen sender");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "gen sender failed (exit {:?}): {stdout:?} / {:?}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    // Parse "id=<id> gen=<gen>".
    let id: u32 = stdout
        .split_whitespace()
        .find_map(|t| t.strip_prefix("id="))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("sender did not print an id: {stdout:?}"));

    // The receive thread caches (id → surface, generation) asynchronously; poll briefly.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if hl_display::metal::iosurface_generation(id) == GENERATION {
            break;
        }
        if Instant::now() > deadline {
            panic!(
                "engine-supplied generation {GENERATION:#x} for id={id} never reached the compositor \
                 (got {:#x})",
                hl_display::metal::iosurface_generation(id)
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    // And it is exposed as the id's authenticated allocation metadata (the value dmabuf import checks).
    let meta = hl_display::metal::iosurface_metadata(id).expect("metadata for a cached IOSurface");
    assert_eq!(meta.generation, GENERATION, "authenticated metadata carries the engine generation");
    assert_eq!((meta.width, meta.height), (16, 8), "metadata reflects the real IOSurface dimensions");
}
