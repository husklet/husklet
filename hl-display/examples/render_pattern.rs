//! Headless "first pixels" proof, viewable on the Linux dev host: drive the `hl-display` compositor with
//! an in-process minimal Wayland client that draws `weston-simple-shm`'s exact signature pattern (the
//! `(x ^ y)` XOR texture with a moving-color border) into a real `memfd`-backed `wl_shm` pool, pass the fd
//! via `SCM_RIGHTS`, and have the server composite + dump a PNG. Since it renders the SAME pattern the real
//! `weston-simple-shm` produces, the PNG is directly comparable to the eventual on-Mac window.
//!
//! Run: `cargo run -p hl-display --example render_pattern -- /path/out.png`

#![allow(unused_imports, dead_code)] // imports/consts are used only by the Linux `main` below

use hl_display::present::PngPresenter;
use hl_display::server::Server;
use hl_display::wire::{Conn, Message};
use std::os::unix::io::RawFd;

const WL_DISPLAY: u32 = 1;

// This "first pixels" demo builds a `wl_shm` pool from a Linux `memfd`; `memfd_create` is Linux-only, so
// the demo is gated to Linux and stubbed elsewhere (keeps the macOS build of the crate's examples green).
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("render_pattern is a Linux-only memfd/shm demo; not built on this platform");
}

#[cfg(target_os = "linux")]
fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/hl-display-pattern.png".into());
    let dir = std::path::Path::new(&out)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| ".".into());

    let mut sv = [0i32; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
        0
    );
    let (client_fd, server_fd) = (sv[0], sv[1]);
    for fd in [client_fd, server_fd] {
        nonblock(fd);
    }
    let mut server = Server::new(server_fd, PngPresenter::new(&dir));
    let mut c = Conn::new(client_fd);

    let (w, h): (i32, i32) = (256, 160);
    let stride = w * 4;
    let size = (stride * h) as usize;

    // Minimal handshake (we know our own server assigns fixed global names 1..5; a real client would parse
    // the registry, which the unit self-test exercises — here we keep it terse for the visual demo).
    let (reg, comp, shm, wm, surface, xdg, toplevel, pool, buffer) = (2, 3, 4, 5, 6, 7, 8, 9, 10);
    c.send(&Message::new(WL_DISPLAY, 1).u32(reg)); // get_registry
    flush(&mut c, &mut server);
    c.send(
        &Message::new(reg, 0)
            .u32(1)
            .string("wl_compositor")
            .u32(4)
            .u32(comp),
    );
    c.send(&Message::new(reg, 0).u32(2).string("wl_shm").u32(1).u32(shm));
    c.send(
        &Message::new(reg, 0)
            .u32(3)
            .string("xdg_wm_base")
            .u32(1)
            .u32(wm),
    );
    c.send(&Message::new(comp, 0).u32(surface)); // create_surface
    c.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
    c.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
    c.send(&Message::new(toplevel, 2).string("weston-simple-shm (hl)")); // set_title
    c.send(&Message::new(surface, 6)); // initial commit
    flush(&mut c, &mut server);
    c.send(&Message::new(xdg, 4).u32(1)); // ack_configure

    // memfd-backed pool + the XOR pattern.
    let name = std::ffi::CString::new("hl-shm-pattern").unwrap();
    let mfd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
    assert!(mfd >= 0);
    assert_eq!(unsafe { libc::ftruncate(mfd, size as libc::off_t) }, 0);
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            mfd,
            0,
        )
    };
    assert_ne!(map, libc::MAP_FAILED);
    let px = map as *mut u8;
    for y in 0..h {
        for x in 0..w {
            let o = (y * stride + x * 4) as usize;
            // weston-simple-shm's paint: border ring + XOR interior. BGRA memory order (little-endian ARGB).
            let (b, g, r);
            if x < 8 || x >= w - 8 || y < 8 || y >= h - 8 {
                // colored border
                r = 0xff;
                g = ((x * 255 / w) as u8) & 0xff;
                b = ((y * 255 / h) as u8) & 0xff;
            } else {
                let v = ((x ^ y) & 0xff) as u8;
                r = v;
                g = v;
                b = v;
            }
            unsafe {
                *px.add(o) = b;
                *px.add(o + 1) = g;
                *px.add(o + 2) = r;
                *px.add(o + 3) = 0;
            }
        }
    }

    c.send(&Message::new(shm, 0).u32(pool).u32(size as u32)); // create_pool (fd OOB)
    c.queue_fd(mfd);
    flush(&mut c, &mut server);
    c.send(
        &Message::new(pool, 0)
            .u32(buffer)
            .i32(0)
            .i32(w)
            .i32(h)
            .i32(stride)
            .u32(1),
    ); // XRGB8888
    c.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
    c.send(&Message::new(surface, 2).i32(0).i32(0).i32(w).i32(h)); // damage
    c.send(&Message::new(surface, 6)); // commit
    flush(&mut c, &mut server);

    unsafe {
        libc::munmap(map, size);
        libc::close(mfd);
    }
    // The PngPresenter wrote `<dir>/surface-6.png`; move it to the requested name.
    let produced = dir.join("surface-6.png");
    if produced != std::path::Path::new(&out) {
        let _ = std::fs::rename(&produced, &out);
    }
    println!(
        "wrote {out} ({}x{}), frames={}",
        w,
        h,
        server.presenter().frames
    );
}

fn nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

fn flush(c: &mut Conn, s: &mut Server<PngPresenter>) {
    c.flush().unwrap();
    s.pump().unwrap();
    // drain any server replies so the client socket doesn't back up
    loop {
        match c.fill().unwrap() {
            0 | -1 => break,
            _ => {}
        }
    }
    while c.next_message().is_some() {}
}
