//! Live validation client for the input-offset + popup-position fixes. Connects to a RUNNING hl-display
//! (`--window --metal`) over its Wayland socket and drives the two code paths under test with the raw wire
//! protocol (no libwayland, no GTK):
//!   1. a toplevel whose `xdg_surface.set_window_geometry` insets a CSD-style shadow margin — so the native
//!      window is CROPPED to the inner rect and on-screen clicks must be offset back by (gx,gy);
//!   2. an `xdg_popup` anchored near the top-left of that toplevel via a positioner — so the popup window
//!      must open AT the anchor, not at a default cascade / screen bottom.
//! It then holds the connection open (draining events) so a driver can SIGUSR1-dump, inspect the hl-display
//! logs, and synthesize clicks. This is the on-Mac analogue of the headless `render_pattern` example.
//!
//! Run:  live_geometry_popup <socket-path> [hold-seconds]

use hl_display::wire::{Conn, Message};
use std::os::unix::io::{AsRawFd, RawFd};

const WL_DISPLAY: u32 = 1;

// Toplevel: a 480x360 surface whose window geometry insets a (gx=20, gy=34) shadow margin, leaving a
// 400x280 visible window. These are the numbers the driver checks against the pointer-motion debug log.
const FULL_W: i32 = 480;
const FULL_H: i32 = 360;
const GEO_X: i32 = 20;
const GEO_Y: i32 = 34;
const GEO_W: i32 = 400;
const GEO_H: i32 = 280;

fn main() {
    let sock = std::env::args().nth(1).expect("usage: live_geometry_popup <socket> [secs]");
    let hold: u64 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(60);

    let fd = connect(&sock);
    nonblock(fd);
    let mut c = Conn::new(fd);

    // ---- object ids (client-allocated) ----
    let (reg, comp, shm, wm, seat, pointer) = (2u32, 3, 4, 5, 6, 7);
    let (surface, xdg, toplevel) = (10u32, 11, 12);
    let (pool, buffer) = (20u32, 21);
    let (pos, psurface, pxdg, popup) = (30u32, 31, 32, 33);
    let (ppool, pbuffer) = (40u32, 41);

    // ---- registry + binds (hl-display advertises fixed global names 1..) ----
    c.send(&Message::new(WL_DISPLAY, 1).u32(reg)); // get_registry
    c.send(&Message::new(reg, 0).u32(1).string("wl_compositor").u32(4).u32(comp));
    c.send(&Message::new(reg, 0).u32(2).string("wl_shm").u32(1).u32(shm));
    c.send(&Message::new(reg, 0).u32(3).string("xdg_wm_base").u32(1).u32(wm));
    c.send(&Message::new(reg, 0).u32(4).string("wl_seat").u32(5).u32(seat));
    c.send(&Message::new(seat, 0).u32(pointer)); // wl_seat.get_pointer
    pump(&mut c);

    // ---- toplevel with a shadow-margin window geometry ----
    c.send(&Message::new(comp, 0).u32(surface)); // create_surface
    c.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
    c.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
    c.send(&Message::new(toplevel, 2).string("hl live: geometry+popup")); // set_title
    c.send(&Message::new(xdg, 3).i32(GEO_X).i32(GEO_Y).i32(GEO_W).i32(GEO_H)); // set_window_geometry
    c.send(&Message::new(surface, 6)); // initial commit
    pump(&mut c);
    c.send(&Message::new(xdg, 4).u32(1)); // ack_configure (server ignores the serial value)

    // full-surface shm buffer, painted so the geometry crop is visible (checkerboard + bright margin).
    let (map, mfd, size) = make_pool(FULL_W, FULL_H, 0xFF3060A0u32, 0xFF102040u32, true);
    c.send(&Message::new(shm, 0).u32(pool).u32(size as u32)); // create_pool (fd OOB)
    c.queue_fd(mfd);
    pump(&mut c);
    c.send(&Message::new(pool, 0).u32(buffer).i32(0).i32(FULL_W).i32(FULL_H).i32(FULL_W * 4).u32(1)); // XRGB
    c.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.send(&Message::new(surface, 2).i32(0).i32(0).i32(FULL_W).i32(FULL_H)); // damage
    c.send(&Message::new(surface, 6)); // commit
    pump(&mut c);
    unsafe { libc::munmap(map, size); }

    // ---- popup anchored near the toplevel's top-left ----
    // Positioner: size 220x160, anchor rect (60,50,10,10) in the PARENT's window-geometry space, anchor
    // bottom_left, gravity bottom_right, no offset ⇒ geometry() = (60, 60). The popup window's top-left must
    // therefore land at parent-content-top-left + (60,60) — right at the "widget", not at screen bottom.
    let (pw, ph) = (220i32, 160i32);
    c.send(&Message::new(wm, 1).u32(pos)); // create_positioner
    c.send(&Message::new(pos, 1).i32(pw).i32(ph)); // set_size
    c.send(&Message::new(pos, 2).i32(60).i32(50).i32(10).i32(10)); // set_anchor_rect
    c.send(&Message::new(pos, 3).u32(6)); // set_anchor bottom_left
    c.send(&Message::new(pos, 4).u32(8)); // set_gravity bottom_right
    c.send(&Message::new(comp, 0).u32(psurface)); // create_surface (popup)
    c.send(&Message::new(wm, 2).u32(pxdg).u32(psurface)); // get_xdg_surface (popup)
    c.send(&Message::new(pxdg, 2).u32(popup).u32(xdg).u32(pos)); // get_popup(id, parent_xdg, positioner)
    c.send(&Message::new(pxdg, 3).i32(0).i32(0).i32(pw).i32(ph)); // set_window_geometry (no popup margin)
    c.send(&Message::new(psurface, 6)); // initial commit
    pump(&mut c);
    c.send(&Message::new(pxdg, 4).u32(2)); // ack_configure

    let (pmap, pmfd, psize) = make_pool(pw, ph, 0xFFE0E0E0u32, 0xFFB00020u32, false);
    c.send(&Message::new(shm, 0).u32(ppool).u32(psize as u32));
    c.queue_fd(pmfd);
    pump(&mut c);
    c.send(&Message::new(ppool, 0).u32(pbuffer).i32(0).i32(pw).i32(ph).i32(pw * 4).u32(1));
    c.send(&Message::new(psurface, 1).u32(pbuffer).i32(0).i32(0));
    c.send(&Message::new(psurface, 2).i32(0).i32(0).i32(pw).i32(ph));
    c.send(&Message::new(psurface, 6));
    pump(&mut c);
    unsafe { libc::munmap(pmap, psize); }

    eprintln!(
        "live_geometry_popup: toplevel geometry=({GEO_X},{GEO_Y} {GEO_W}x{GEO_H}) popup {pw}x{ph} at parent-offset (60,60); holding {hold}s"
    );
    // Hold open, draining events, so the driver can dump / click.
    let start = std::time::Instant::now();
    while start.elapsed().as_secs() < hold {
        pump(&mut c);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Connect an AF_UNIX SOCK_STREAM to `path` and return the fd.
fn connect(path: &str) -> RawFd {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0, "socket() failed");
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as _;
    let bytes = path.as_bytes();
    assert!(bytes.len() < addr.sun_path.len(), "socket path too long");
    for (i, b) in bytes.iter().enumerate() {
        addr.sun_path[i] = *b as _;
    }
    let r = unsafe {
        libc::connect(
            fd,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    assert_eq!(r, 0, "connect({path}) failed: {}", std::io::Error::last_os_error());
    fd
}

fn nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

/// Flush outbound, then read + discard any inbound events (keeps the socket from backing up).
fn pump(c: &mut Conn) {
    c.flush().ok();
    loop {
        match c.fill() {
            Ok(n) if n > 0 => {}
            _ => break,
        }
    }
    while c.next_message().is_some() {}
}

/// A temp-file-backed shm pool of `w`x`h` XRGB pixels, painted as a checkerboard between `a` and `b`
/// (0xAARRGGBB). When `margin`, the outer 1-cell ring is drawn bright so the window-geometry crop is
/// visibly distinct from the full surface. Returns (mmap ptr, fd, byte size). The file is unlinked so it
/// vanishes on close; the fd stays valid for SCM_RIGHTS passing + the server's own mmap.
fn make_pool(w: i32, h: i32, a: u32, b: u32, margin: bool) -> (*mut libc::c_void, RawFd, usize) {
    let size = (w * h * 4) as usize;
    let path = format!("/tmp/hl-live-shm-{}-{}", std::process::id(), w * 100003 + h);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("open shm temp");
    file.set_len(size as u64).expect("ftruncate");
    let _ = std::fs::remove_file(&path); // unlink; fd keeps it alive
    let fd = file.as_raw_fd();
    std::mem::forget(file); // keep the fd open for the server; process exit reaps it
    let map = unsafe {
        libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0)
    };
    assert_ne!(map, libc::MAP_FAILED, "mmap");
    let px = map as *mut u8;
    let put = |o: usize, argb: u32| unsafe {
        *px.add(o) = (argb & 0xff) as u8; // B
        *px.add(o + 1) = ((argb >> 8) & 0xff) as u8; // G
        *px.add(o + 2) = ((argb >> 16) & 0xff) as u8; // R
        *px.add(o + 3) = 0; // X
    };
    for y in 0..h {
        for x in 0..w {
            let o = (y * w * 4 + x * 4) as usize;
            let ring = margin && (x < 12 || x >= w - 12 || y < 12 || y >= h - 12);
            let c = if ring {
                0xFF10FF40 // bright green margin = the shadow area cropped OUT by window geometry
            } else if ((x / 24) + (y / 24)) % 2 == 0 {
                a
            } else {
                b
            };
            put(o, c);
        }
    }
    (map, fd, size)
}
