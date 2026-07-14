//! Row 1 (`dmabuf_feedback_serializes_an_explicit_linux_u64_device_id`): drive the REAL C guest probe
//! (`dd-tests/guests/gui_matrix/gui_dmabuf_feedback_guest.c`) against a REAL `dd-compositor` Wayland
//! socket, end to end. The in-process `dmabuf_v4_feedback.rs` test proves the Rust wire side; the
//! ledger's residual is that the macOS *guest bridge* run — a separate process doing `recvmsg` +
//! `SCM_RIGHTS` fd receipt + `mmap` of the format table over a real `AF_UNIX` connection — was absent.
//!
//! This harness closes that: it binds a genuine listening socket, `insert_client`s the probe's
//! connection, pumps the compositor dispatch loop, and asserts the probe (which maps the v4 format
//! table and reads the 8-byte little-endian Linux `dev_t`) exits 0. It runs on the macOS host through
//! the `mac` bridge, where `recvmsg`/`SCM_RIGHTS`/`mmap`/`AF_UNIX` are all real — so it is the missing
//! bridge run, now a committed regression rather than a manual step. Skips (does not fail) if no C
//! toolchain is present, mirroring the `pixel_parity` harness convention.

use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hl_compositor::{ClientState, DdState};
use hl_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use smithay::reexports::wayland_server::Display;

struct NullPresenter;
impl Presenter for NullPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        Ok(PresentOutcome::Delivered { serial: 0, timing: None })
    }
    fn frame_count(&self) -> u32 {
        0
    }
}

fn guest_probe_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../hl-jit-darwin/testdata/guests/gui_matrix/gui_dmabuf_feedback_guest.c")
}

/// Compile the guest probe to a native host binary. Returns None (skip) on any toolchain failure.
fn build_probe(out: &Path) -> Option<PathBuf> {
    let src = guest_probe_src();
    if !src.exists() {
        eprintln!("[dmabuf-bridge] guest probe source missing ({src:?}); skipping");
        return None;
    }
    let bin = out.join("gui_dmabuf_feedback_guest");
    let ok = Command::new("cc")
        .arg(&src)
        .arg("-O1")
        .arg("-o")
        .arg(&bin)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("[dmabuf-bridge] guest probe failed to compile; skipping");
        return None;
    }
    Some(bin)
}

#[test]
fn dmabuf_feedback_guest_reads_device_id_and_format_table_over_a_real_socket() {
    // The v4/v5 feedback global is opt-in behind HL_DISPLAY_DMABUF (see new_dmabuf_state).
    std::env::set_var("HL_DISPLAY_DMABUF", "1");

    let tmp = std::env::temp_dir().join(format!("dd-dmabuf-bridge-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let Some(probe) = build_probe(&tmp) else {
        return; // no toolchain: skip (not a failure)
    };

    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = DdState::new(dh.clone(), Box::new(NullPresenter));

    // A real listening AF_UNIX socket at $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY (what the probe connects to).
    let sock_name = "wayland-dmabuf-bridge";
    let sock_path = tmp.join(sock_name);
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path).expect("bind wayland socket");
    listener.set_nonblocking(true).unwrap();

    let mut child = Command::new(&probe)
        .env("XDG_RUNTIME_DIR", &tmp)
        .env("WAYLAND_DISPLAY", sock_name)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn guest probe");

    // Accept the probe's connection and pump the compositor dispatch loop until the probe exits.
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
                Err(e) => panic!("accept failed: {e}"),
            }
        }
        // Service client requests and flush replies (registry, feedback, format-table fd, device).
        let _ = display.dispatch_clients(&mut state);
        let _ = display.flush_clients();

        if let Some(status) = child.try_wait().expect("try_wait") {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("guest probe did not finish within the deadline (accepted={accepted})");
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    let mut out = String::new();
    if let Some(mut so) = child.stdout.take() {
        use std::io::Read;
        let _ = so.read_to_string(&mut out);
    }
    let _ = std::fs::remove_file(&sock_path);

    // The probe prints e.g. `... device=57984 ar=1 xr=1 truthful=1` and returns 0 only when it mapped
    // the v4 format table, saw both ARGB/XRGB dd-modifier pairs, and read the exact Linux dev_t.
    assert!(
        status.success(),
        "guest dmabuf-feedback probe failed (exit {:?}); output: {out:?}. \
         A non-zero exit maps to the probe's stage: 4=no v4 global, 7=bad table size, \
         8=wrong dev_t, 9=mmap failed, 10=missing/again untruthful pairs.",
        status.code()
    );
    assert!(
        out.contains("device=57984") && out.contains("truthful=1"),
        "probe did not confirm the (226<<8)|128 dev_t and truthful modifier table: {out:?}"
    );
}
