//! Row 3 live bridge regression (`compositor_validates_dmabuf_planes_flags_and_backing_metadata_
//! before_success`): drive the REAL C guest probe (`gui_dmabuf_stale_generation_guest.c`) through a
//! full `zwp_linux_dmabuf_v1` import handshake against a live `dd-compositor`, and prove the compositor
//! REJECTS an import whose modifier carries a stale allocation generation — while accepting the
//! matching (and the legacy, unversioned) generation. This is the stale-id protocol regression the
//! ledger asks for, run over a real socket on the macOS host via the `mac` bridge.
//!
//! The id's live allocation generation is seeded by a test presenter (`iosurface_metadata` returns
//! generation 5 for the imported id); the executor-health gate is forced up so the rejection is
//! attributable to the generation check, not to a missing GPU backend. Skips (does not fail) if no C
//! toolchain is present.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dd_compositor::{ClientState, DdState};
use dd_display::present::{
    IOSurfaceMetadata, PresentError, PresentOutcome, Presenter, SurfaceBuffer,
};
use smithay::reexports::wayland_server::Display;

const IOSURFACE_ID: u32 = 7;
const LIVE_GENERATION: u32 = 5;

/// A presenter that authenticates `IOSURFACE_ID` as a live allocation at `LIVE_GENERATION`, matching
/// the probe's 16x8 / stride-64 / BGRA dmabuf so only the generation distinguishes accept from reject.
struct GenPresenter;
impl Presenter for GenPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        Ok(PresentOutcome::Delivered { serial: 0, timing: None })
    }
    fn frame_count(&self) -> u32 {
        0
    }
    fn iosurface_metadata(&self, id: u32) -> Option<IOSurfaceMetadata> {
        (id == IOSURFACE_ID).then_some(IOSurfaceMetadata {
            width: 16,
            height: 8,
            bytes_per_row: 64,
            pixel_format: 0x4247_5241, // 'BGRA'
            generation: LIVE_GENERATION,
        })
    }
}

fn probe_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../dd-tests/guests/gui_matrix/gui_dmabuf_stale_generation_guest.c")
}

fn build_probe(out: &Path) -> Option<PathBuf> {
    let src = probe_src();
    if !src.exists() {
        return None;
    }
    let bin = out.join("gui_dmabuf_stale_generation_guest");
    let ok = Command::new("cc")
        .arg(&src)
        .arg("-O1")
        .arg("-o")
        .arg(&bin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok.then_some(bin)
}

#[test]
fn dmabuf_import_rejects_a_stale_allocation_generation_over_the_wire() {
    std::env::set_var("DD_DISPLAY_DMABUF", "1");
    // The accelerated-import readiness gate must be up so a rejection is attributable to the generation
    // check rather than to a missing executor.
    dd_compositor::gpu::set_executor_health(true);

    let tmp = std::env::temp_dir().join(format!("dd-stale-gen-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let Some(probe) = build_probe(&tmp) else {
        return; // no toolchain: skip
    };

    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = DdState::new(dh.clone(), Box::new(GenPresenter));

    let sock_name = "wayland-stale-gen";
    let sock_path = tmp.join(sock_name);
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).expect("bind wayland socket");
    listener.set_nonblocking(true).unwrap();

    // Run the probe once for a given generation; return its printed "result=created|failed".
    let mut run = |generation: u32| -> String {
        let mut child = Command::new(&probe)
            .arg(generation.to_string())
            .env("XDG_RUNTIME_DIR", &tmp)
            .env("WAYLAND_DISPLAY", sock_name)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn probe");

        let mut accepted = false;
        let deadline = Instant::now() + Duration::from_secs(20);
        let status = loop {
            if !accepted {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(true).unwrap();
                        dh.insert_client(stream, Arc::new(ClientState::default()))
                            .expect("insert client");
                        accepted = true;
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => panic!("accept: {e}"),
                }
            }
            let _ = display.dispatch_clients(&mut state);
            let _ = display.flush_clients();
            if let Some(s) = child.try_wait().expect("try_wait") {
                break s;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("probe (gen={generation}) did not finish");
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let mut out = String::new();
        if let Some(mut so) = child.stdout.take() {
            use std::io::Read;
            let _ = so.read_to_string(&mut out);
        }
        assert!(status.success(), "probe (gen={generation}) errored: exit {:?}", status.code());
        out
    };

    // Stale generation (id's live allocation moved on to 5) -> rejected.
    let stale = run(LIVE_GENERATION - 1);
    assert!(
        stale.contains("result=failed"),
        "a stale allocation generation must be rejected at import, got: {stale:?}"
    );
    // Matching generation -> accepted (proves the rejection is generation-specific, not a blanket fail).
    let matching = run(LIVE_GENERATION);
    assert!(
        matching.contains("result=created"),
        "the live allocation generation must import, got: {matching:?}"
    );
    // Unversioned (generation 0) -> accepted (legacy producers unaffected).
    let legacy = run(0);
    assert!(
        legacy.contains("result=created"),
        "an unversioned (generation 0) buffer must import, got: {legacy:?}"
    );

    let _ = std::fs::remove_file(&sock_path);
    dd_compositor::gpu::set_executor_health(false);
}
