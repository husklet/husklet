//! The `zwp_linux_dmabuf_v1` global must advertise **version 4+** (feedback) on the Smithay
//! compositor — the accelerated-Chromium gap (Gap #1 in
//! `docs/rendering/SMITHAY_DEFAULT_READINESS.md`).
//!
//! Chromium's ozone/GPU derives its DRM render-node path from the dmabuf-**feedback** `main_device`
//! (protocol version 4). `dd-compositor` used to advertise the v3 global only, because Smithay builds
//! the v4 feedback format-table in a `shm_open`ed file whose name overflows macOS `PSHMNAMLEN` (31) →
//! `ENAMETOOLONG`, so the v4 global could not stand up on the macOS host. With the offline-vendored
//! smithay fix (`vendor/smithay-0.7.0/src/utils/sealed_file.rs` shortens that object name), the
//! feedback format-table now builds and `new_dmabuf_state` creates the feedback-carrying global.
//!
//! This test connects a real wire client, binds feedback, receives its SCM_RIGHTS format-table fd,
//! mmaps/parses it, and verifies the explicit 8-byte Linux device id and truthful modifier pairs.

use std::collections::HashMap;
use std::sync::Arc;

use hl_compositor::{ClientState, DdState};
use hl_display::present::{PresentError, PresentOutcome, Presenter, SurfaceBuffer};
use hl_display::wire::{Conn, Message};
use smithay::reexports::wayland_server::Display;

const WL_DISPLAY: u32 = 1;

/// A presenter that records nothing — this test only inspects the registry, never commits a frame.
struct NullPresenter;
impl Presenter for NullPresenter {
    fn present(&mut self, _surf: &SurfaceBuffer) -> Result<PresentOutcome, PresentError> {
        Ok(PresentOutcome::Delivered { serial: 0, timing: None })
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
    std::env::set_var("HL_DISPLAY_DMABUF", "1");
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
    let mut globals: HashMap<String, (u32, u32)> = HashMap::new();
    while let Some(m) = conn.next_message() {
        if m.object == registry && m.opcode == 0 {
            let mut r = m.reader();
            let name = r.u32();
            let iface = r.string();
            let ver = r.u32();
            globals.insert(iface, (name, ver));
        }
    }

    let (name, ver) = globals.get("zwp_linux_dmabuf_v1").copied().unwrap_or_else(|| {
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

    // Bind v4 and request default feedback.
    let dmabuf = 3u32;
    let feedback = 4u32;
    conn.send(
        &Message::new(registry, 0)
            .u32(name)
            .string("zwp_linux_dmabuf_v1")
            .u32(4)
            .u32(dmabuf),
    );
    conn.send(&Message::new(dmabuf, 2).u32(feedback));
    conn.flush().unwrap();
    display.dispatch_clients(&mut state).unwrap();
    display.flush_clients().unwrap();
    loop {
        match conn.fill().unwrap() {
            0 | -1 => break,
            _ => {}
        }
    }

    let mut table_fd = None;
    let mut table_size = None;
    let mut main_device = None;
    while let Some(m) = conn.next_message() {
        if m.object != feedback {
            continue;
        }
        match m.opcode {
            1 => {
                table_size = Some(m.reader().u32() as usize);
                table_fd = conn.take_fd();
            }
            2 => main_device = Some(m.reader().array()),
            _ => {}
        }
    }

    let device = main_device.expect("feedback.main_device missing");
    assert_eq!(device.len(), 8, "Linux dev_t feedback must always be u64");
    assert_eq!(u64::from_le_bytes(device.try_into().unwrap()), (226u64 << 8) | 128);

    let fd = table_fd.expect("feedback.format_table SCM_RIGHTS fd missing");
    let size = table_size.expect("feedback.format_table size missing");
    assert!(size >= 16 && size % 16 == 0);
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            fd,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "format-table fd must be guest-mappable");
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), size) };
    let entries: Vec<(u32, u64)> = bytes
        .chunks_exact(16)
        .map(|entry| {
            (
                u32::from_le_bytes(entry[0..4].try_into().unwrap()),
                u64::from_le_bytes(entry[8..16].try_into().unwrap()),
            )
        })
        .collect();
    unsafe {
        libc::munmap(ptr, size);
        libc::close(fd);
    }
    let modifier = 0x6464u64 << 32;
    assert!(entries.contains(&(0x3432_5241, modifier)));
    assert!(entries.contains(&(0x3432_5258, modifier)));
    assert!(
        entries.iter().all(|(_, advertised)| *advertised == modifier),
        "feedback must not advertise LINEAR or other pairs rejected by the importer: {entries:?}"
    );
}
