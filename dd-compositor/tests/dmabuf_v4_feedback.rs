//! The `zwp_linux_dmabuf_v1` global must advertise **version 4+** (feedback) on the Smithay
//! compositor — the accelerated-Chromium gap (Gap #1 in
//! `docs/rendering/SMITHAY_DEFAULT_READINESS.md`).
//!
//! Chromium's ozone/GPU derives its DRM render-node path from the dmabuf-**feedback** `main_device`
//! (protocol version 4). `dd-compositor` used to advertise the v3 global only, because Smithay builds
//! the v4 feedback format-table in a `shm_open`ed file whose name overflows macOS `PSHMNAMLEN` (31) →
//! `ENAMETOOLONG`, so the v4 global could not stand up on the macOS host. With the offline-vendored
//! smithay fix (`third_party/smithay-0.7.0/src/utils/sealed_file.rs` shortens that object name), the
//! feedback format-table now builds and `new_dmabuf_state` creates the feedback-carrying global.
//!
//! This test connects an in-process client, enumerates the registry, and asserts the advertised
//! `zwp_linux_dmabuf_v1` version is >= 4. If the format-table build regressed (PSHMNAMLEN), the
//! compositor's fallback advertises v3 and this assertion fails — the exact regression signal.

use std::collections::HashMap;
use std::sync::Arc;

use dd_compositor::{ClientState, DdState};
use dd_display::present::{Presenter, SurfaceBuffer};
use dd_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;

const WL_DISPLAY: u32 = 1;

/// A presenter that records nothing — this test only inspects the registry, never commits a frame.
struct NullPresenter;
impl Presenter for NullPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> bool {
        true
    }
    fn frame_count(&self) -> u32 {
        0
    }
}

fn socketpair_nonblocking() -> (i32, i32) {
    let mut sv = [0i32; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
        0
    );
    for fd in sv {
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }
    (sv[0], sv[1])
}

#[test]
fn dmabuf_global_advertises_v4_feedback() {
    let mut display: Display<DdState> = Display::new().unwrap();
    let mut dh = display.handle();
    let mut state = DdState::new(dh.clone(), Box::new(NullPresenter));

    use std::os::unix::io::FromRawFd;
    let (client_fd, server_fd) = socketpair_nonblocking();
    dh.insert_client(
        unsafe { std::os::unix::net::UnixStream::from_raw_fd(server_fd) },
        Arc::new(ClientState::default()),
    )
    .unwrap();

    let mut conn = Conn::new(client_fd);
    // wl_display.get_registry(registry=2)
    let registry = 2u32;
    conn.send(&Message::new(WL_DISPLAY, 1).u32(registry));
    conn.flush().unwrap();
    display.dispatch_clients(&mut state).unwrap();
    display.flush_clients().unwrap();

    // Drain and index wl_registry.global(name, iface, version) events.
    loop {
        match conn.fill().unwrap() {
            0 | -1 => break,
            _ => {}
        }
    }
    let mut globals: HashMap<String, u32> = HashMap::new();
    while let Some(m) = conn.next_message() {
        if m.object == registry && m.opcode == 0 {
            let mut r = m.reader();
            let _name = r.u32();
            let iface = r.string();
            let ver = r.u32();
            globals.insert(iface, ver);
        }
    }

    let ver = globals.get("zwp_linux_dmabuf_v1").copied().unwrap_or_else(|| {
        panic!(
            "zwp_linux_dmabuf_v1 not advertised; globals = {:?}",
            globals.keys().collect::<Vec<_>>()
        )
    });
    assert!(
        ver >= 4,
        "zwp_linux_dmabuf_v1 must advertise version >= 4 (feedback) for the accelerated Chromium \
         path; got v{ver}. A v3 advertisement means the feedback format-table failed to build \
         (macOS PSHMNAMLEN regression?)."
    );
}
