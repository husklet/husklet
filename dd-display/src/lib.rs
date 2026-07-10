//! `dd-display` — the shared host renderer for dd containers (see `docs/ideas/RENDERING.md`).
//!
//! This library is the **portable compositor core**: a minimal Wayland endpoint ([`server::Server`]) that
//! composites a guest's `wl_shm` buffers into tight BGRA framebuffers and hands them to a
//! [`present::Presenter`]. The core has no GPU/window/display dependency, so it builds and is fully
//! end-to-end self-tested on the Linux dev host (see the `headless` test below). The native macOS window
//! backend lives behind `present_cocoa` (compiled only on macOS) and reuses the same [`present::Presenter`]
//! seam.

pub mod keymap;
pub mod present;
pub mod selftest;
pub mod server;
pub mod wire;

#[cfg(target_os = "macos")]
pub mod present_cocoa;

#[cfg(target_os = "macos")]
pub mod metal;

#[cfg(target_os = "macos")]
pub mod metal_backend;

use std::os::unix::io::RawFd;

/// Bind + listen on an `AF_UNIX` `SOCK_STREAM` socket at `path` (removing any stale inode first). Shared
/// by the server binary and the real-socket self-test.
pub fn listen_unix(path: &str) -> std::io::Result<RawFd> {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as _;
    let bytes = path.as_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket path too long",
        ));
    }
    for (i, b) in bytes.iter().enumerate() {
        addr.sun_path[i] = *b as _;
    }
    let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    if unsafe { libc::bind(fd, &addr as *const _ as *const _, len) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::listen(fd, 16) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(fd)
}

#[cfg(all(test, target_os = "linux"))]
mod headless {
    //! End-to-end proof of the CPU pipeline with NO GPU/display: an in-process minimal Wayland client
    //! backs an shm pool with a real `memfd`, draws a test pattern, passes the fd via `SCM_RIGHTS`, and
    //! drives the full `xdg_shell` handshake against [`super::server::Server`]. The server composites and
    //! the [`super::present::PngPresenter`] dumps a PNG; we assert the round-tripped pixels match what the
    //! client drew. This is milestone M0+M1's software path, verifiable headlessly.

    use crate::present::PngPresenter;
    use crate::server::Server;
    use crate::wire::{Conn, Message};
    use std::os::unix::io::RawFd;

    const WL_DISPLAY: u32 = 1;

    struct Client {
        conn: Conn,
        next_id: u32,
        globals: std::collections::HashMap<String, (u32, u32)>, // iface -> (name, version)
    }

    impl Client {
        fn new(fd: RawFd) -> Client {
            Client {
                conn: Conn::new(fd),
                next_id: 2,
                globals: Default::default(),
            }
        }
        fn alloc(&mut self) -> u32 {
            let id = self.next_id;
            self.next_id += 1;
            id
        }
        fn flush(&mut self) {
            self.conn.flush().unwrap();
        }
        /// Drain everything the server has sent so far (the socket is nonblocking; -1 == would-block).
        fn drain(&mut self) {
            loop {
                match self.conn.fill().unwrap() {
                    0 | -1 => break,
                    _ => {}
                }
            }
            while let Some(m) = self.conn.next_message() {
                // registry.global(name, iface, ver)
                let mut r = m.reader();
                if m.opcode == 0 && self.is_registry(m.object) {
                    let name = r.u32();
                    let iface = r.string();
                    let ver = r.u32();
                    self.globals.insert(iface, (name, ver));
                }
            }
        }
        fn is_registry(&self, _o: u32) -> bool {
            // In this tiny client the only object that emits opcode-0 events with a string arg is the
            // registry (id 2). Good enough for the self-test.
            _o == 2
        }
        /// Read + return every message the server has sent so far (raw, unparsed).
        fn poll_messages(&mut self) -> Vec<Message> {
            loop {
                match self.conn.fill().unwrap() {
                    0 | -1 => break,
                    _ => {}
                }
            }
            let mut out = Vec::new();
            while let Some(m) = self.conn.next_message() {
                out.push(m);
            }
            out
        }
    }

    #[test]
    fn shm_client_draws_a_frame_the_server_composites() {
        // Socketpair: one end is the client, the other is the compositor.
        let mut sv = [0i32; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
            0
        );
        let (client_fd, server_fd) = (sv[0], sv[1]);
        for fd in [client_fd, server_fd] {
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }

        let dir =
            std::env::var("DD_DISPLAY_DUMP").unwrap_or_else(|_| "/tmp/dd-display-selftest".into());
        let mut server = Server::new(server_fd, PngPresenter::new(&dir));

        let mut c = Client::new(client_fd);
        // get_registry(new_id) + sync roundtrip.
        let reg = c.alloc(); // = 2
        c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg)); // get_registry
        c.flush();
        server.pump().unwrap(); // server advertises globals + shm formats
        c.drain();
        assert!(
            c.globals.contains_key("wl_compositor"),
            "registry advertised: {:?}",
            c.globals.keys().collect::<Vec<_>>()
        );
        assert!(c.globals.contains_key("wl_shm"));
        assert!(c.globals.contains_key("xdg_wm_base"));
        assert!(c.globals.contains_key("wp_viewporter"));

        // bind the three globals we need.
        let comp = c.alloc();
        c.conn.send(
            &Message::new(reg, 0)
                .u32(c.globals["wl_compositor"].0)
                .string("wl_compositor")
                .u32(4)
                .u32(comp),
        );
        let shm = c.alloc();
        c.conn.send(
            &Message::new(reg, 0)
                .u32(c.globals["wl_shm"].0)
                .string("wl_shm")
                .u32(1)
                .u32(shm),
        );
        let wm = c.alloc();
        c.conn.send(
            &Message::new(reg, 0)
                .u32(c.globals["xdg_wm_base"].0)
                .string("xdg_wm_base")
                .u32(1)
                .u32(wm),
        );
        let viewporter = c.alloc();
        c.conn.send(
            &Message::new(reg, 0)
                .u32(c.globals["wp_viewporter"].0)
                .string("wp_viewporter")
                .u32(1)
                .u32(viewporter),
        );

        // surface + xdg toplevel.
        let surface = c.alloc();
        c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
        let xdg = c.alloc();
        c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
        let toplevel = c.alloc();
        c.conn.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel
        c.conn
            .send(&Message::new(toplevel, 2).string("dd-selftest")); // set_title
        c.conn.send(&Message::new(surface, 6)); // initial commit (no buffer)
        c.flush();
        server.pump().unwrap();
        c.drain(); // configure events arrive; the tiny client doesn't need to parse them, just ack a serial
        c.conn.send(&Message::new(xdg, 4).u32(1)); // ack_configure(serial=1)

        // Back the pool with a real memfd, draw a recognizable pattern.
        let (w, h): (i32, i32) = (4, 3);
        let stride = w * 4;
        let size = (stride * h) as usize;
        let name = std::ffi::CString::new("dd-shm").unwrap();
        let mfd = unsafe { libc::memfd_create(name.as_ptr(), 0) };
        assert!(mfd >= 0, "memfd_create failed");
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
        // Fill BGRA: a per-pixel gradient (B=col*40, G=row*60, R=200, X=0).
        let px = map as *mut u8;
        for row in 0..h {
            for col in 0..w {
                let o = (row * stride + col * 4) as usize;
                unsafe {
                    *px.add(o) = (col * 40) as u8; // B
                    *px.add(o + 1) = (row * 60) as u8; // G
                    *px.add(o + 2) = 200; // R
                    *px.add(o + 3) = 0; // X
                }
            }
        }

        // create_pool(fd, size) — the fd rides SCM_RIGHTS with this flush.
        let pool = c.alloc();
        c.conn
            .send(&Message::new(shm, 0).u32(pool).u32(size as u32)); // note: fd is OOB
        c.conn.queue_fd(mfd);
        c.flush();
        server.pump().unwrap();
        unsafe { libc::close(mfd) };

        // create_buffer, attach, damage, commit → the server composites this frame.
        let buffer = c.alloc();
        c.conn.send(
            &Message::new(pool, 0)
                .u32(buffer)
                .i32(0)
                .i32(w)
                .i32(h)
                .i32(stride)
                .u32(1),
        ); // XRGB8888
        c.conn
            .send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
        c.conn
            .send(&Message::new(surface, 2).i32(0).i32(0).i32(w).i32(h)); // damage
        c.conn.send(&Message::new(surface, 6)); // commit
        c.flush();
        server.pump().unwrap();

        // Assert the presenter received exactly the pixels the client drew (BGRA→RGBA, XRGB→opaque).
        let last = server_last(&server);
        let (sid, gw, gh, rgba) = last.expect("server did not composite a frame");
        assert_eq!((gw, gh), (w, h));
        assert_eq!(sid, surface);
        // pixel (col=2,row=1): R=200,G=60,B=80,A=255
        let o = (1 * gw + 2) as usize * 4;
        assert_eq!(
            &rgba[o..o + 4],
            &[200, 60, 80, 255],
            "composited pixel mismatch"
        );

        // Chromium uses wp_viewport destination/source state for logical surface sizing. Crop the 4x3
        // buffer to a 2x2 surface and verify both the size and top-left pixel come from the crop origin.
        let viewport = c.alloc();
        c.conn
            .send(&Message::new(viewporter, 1).u32(viewport).u32(surface)); // get_viewport
        c.conn.send(
            &Message::new(viewport, 1)
                .i32(256)
                .i32(256)
                .i32(2 * 256)
                .i32(2 * 256),
        ); // set_source
        c.conn.send(&Message::new(viewport, 2).i32(2).i32(2)); // set_destination
        c.conn
            .send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0)); // attach
        c.conn.send(&Message::new(surface, 6)); // commit
        c.flush();
        server.pump().unwrap();

        let (sid, gw, gh, rgba) =
            server_last(&server).expect("server did not composite viewport frame");
        assert_eq!((sid, gw, gh), (surface, 2, 2));
        assert_eq!(
            &rgba[0..4],
            &[200, 60, 40, 255],
            "viewport crop origin pixel mismatch"
        );

        // Destroying a wp_viewport clears the viewport state; buffer scale below should now drive mapping.
        c.conn.send(&Message::new(viewport, 0)); // destroy wp_viewport; buffer scale now drives mapping

        // Buffer scale turns backing pixels into logical surface units. With a full 4x3 buffer at scale 2
        // the logical output is 2x1, sampled from the full backing texture.
        c.conn.send(&Message::new(surface, 8).i32(2)); // wl_surface.set_buffer_scale
        c.conn
            .send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0));
        c.conn.send(&Message::new(surface, 6));
        c.flush();
        server.pump().unwrap();

        let (sid, gw, gh, _rgba) =
            server_last(&server).expect("server did not composite buffer scale frame");
        assert_eq!((sid, gw, gh), (surface, w / 2, h / 2));

        // Chrome can request a 512x384 xdg window geometry while committing a wider 532x384 backing
        // surface. The compositor must crop to the logical window bounds instead of presenting 532 wide.
        let (wide_w, wide_h): (i32, i32) = (532, 384);
        let wide_stride = wide_w * 4;
        let wide_size = (wide_stride * wide_h) as usize;
        let wide_name = std::ffi::CString::new("dd-wide-shm").unwrap();
        let wide_mfd = unsafe { libc::memfd_create(wide_name.as_ptr(), 0) };
        assert!(wide_mfd >= 0, "wide memfd_create failed");
        assert_eq!(unsafe { libc::ftruncate(wide_mfd, wide_size as libc::off_t) }, 0);
        let wide_map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                wide_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                wide_mfd,
                0,
            )
        };
        assert_ne!(wide_map, libc::MAP_FAILED);
        let wide_px = wide_map as *mut u8;
        for row in 0..wide_h {
            for col in 0..wide_w {
                let (r, g, b) = if col < 10 {
                    (255, 0, 0)
                } else if col >= 522 {
                    (0, 0, 255)
                } else {
                    (((col - 10) % 251) as u8, (row % 251) as u8, 40)
                };
                let o = (row * wide_stride + col * 4) as usize;
                unsafe {
                    *wide_px.add(o) = b; // B
                    *wide_px.add(o + 1) = g; // G
                    *wide_px.add(o + 2) = r; // R
                    *wide_px.add(o + 3) = 0; // X
                }
            }
        }

        let wide_pool = c.alloc();
        c.conn
            .send(&Message::new(shm, 0).u32(wide_pool).u32(wide_size as u32));
        c.conn.queue_fd(wide_mfd);
        c.flush();
        server.pump().unwrap();
        unsafe { libc::close(wide_mfd) };

        let wide_buffer = c.alloc();
        c.conn.send(
            &Message::new(wide_pool, 0)
                .u32(wide_buffer)
                .i32(0)
                .i32(wide_w)
                .i32(wide_h)
                .i32(wide_stride)
                .u32(1),
        );
        c.conn.send(&Message::new(surface, 8).i32(1)); // reset buffer scale
        c.conn
            .send(&Message::new(xdg, 3).i32(10).i32(0).i32(512).i32(384));
        c.conn
            .send(&Message::new(surface, 1).u32(wide_buffer).i32(0).i32(0));
        c.conn.send(&Message::new(surface, 6));
        c.flush();
        server.pump().unwrap();

        let (sid, gw, gh, rgba) =
            server_last(&server).expect("server did not composite xdg geometry frame");
        assert_eq!((sid, gw, gh), (surface, 512, 384));
        assert_eq!(rgba.len(), 512 * 384 * 4);
        assert_eq!(
            &rgba[0..4],
            &[0, 0, 40, 255],
            "xdg geometry crop must start at source x=10, not the left sentinel"
        );
        let right = (gw - 1) as usize * 4;
        assert_eq!(
            &rgba[right..right + 4],
            &[9, 0, 40, 255],
            "xdg geometry crop must end at source x=521, before the right sentinel"
        );
        eprintln!("[headless] PNG written under {dir}");

        unsafe { libc::munmap(wide_map, wide_size) };
        unsafe { libc::munmap(map, size) };
    }

    // Reach into the server's presenter to read the last composited frame. The presenter is owned by the
    // server; expose it via a tiny accessor to keep the test honest (no global state).
    fn server_last(s: &Server<PngPresenter>) -> Option<(u32, i32, i32, Vec<u8>)> {
        s.presenter().last.clone()
    }

    /// M2 input: a client binds `wl_seat`, gets `wl_pointer`+`wl_keyboard`, and (after the compositor
    /// injects synthetic input) receives the correct pointer enter/motion/button and keyboard keymap/
    /// enter/key events, routed to its focused surface. Asserts the wire contents.
    #[test]
    fn input_events_route_to_the_focused_surface() {
        let mut sv = [0i32; 2];
        assert_eq!(
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, sv.as_mut_ptr()) },
            0
        );
        let (client_fd, server_fd) = (sv[0], sv[1]);
        for fd in [client_fd, server_fd] {
            unsafe {
                let fl = libc::fcntl(fd, libc::F_GETFL);
                libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
            }
        }
        let mut server = Server::new(server_fd, PngPresenter::new("/tmp/dd-display-input"));
        let mut c = Client::new(client_fd);

        // Registry → bind seat/compositor/xdg → make a toplevel (so it becomes the focus).
        let reg = c.alloc();
        c.conn.send(&Message::new(WL_DISPLAY, 1).u32(reg));
        c.flush();
        server.pump().unwrap();
        c.drain();
        assert!(c.globals.contains_key("wl_seat"), "seat advertised");

        let seat = c.alloc();
        c.conn.send(
            &Message::new(reg, 0)
                .u32(c.globals["wl_seat"].0)
                .string("wl_seat")
                .u32(5)
                .u32(seat),
        );
        let comp = c.alloc();
        c.conn.send(
            &Message::new(reg, 0)
                .u32(c.globals["wl_compositor"].0)
                .string("wl_compositor")
                .u32(4)
                .u32(comp),
        );
        let wm = c.alloc();
        c.conn.send(
            &Message::new(reg, 0)
                .u32(c.globals["xdg_wm_base"].0)
                .string("xdg_wm_base")
                .u32(1)
                .u32(wm),
        );
        let pointer = c.alloc();
        c.conn.send(&Message::new(seat, 0).u32(pointer)); // get_pointer
        let keyboard = c.alloc();
        c.conn.send(&Message::new(seat, 1).u32(keyboard)); // get_keyboard
        let surface = c.alloc();
        c.conn.send(&Message::new(comp, 0).u32(surface)); // create_surface
        let xdg = c.alloc();
        c.conn.send(&Message::new(wm, 2).u32(xdg).u32(surface)); // get_xdg_surface
        let toplevel = c.alloc();
        c.conn.send(&Message::new(xdg, 1).u32(toplevel)); // get_toplevel → sets focus
        c.flush();
        server.pump().unwrap();

        // The keymap event (with an fd) is sent on get_keyboard; drain + verify the fd maps to our keymap.
        let msgs = c.poll_messages();
        let keymap = msgs
            .iter()
            .find(|m| m.object == keyboard && m.opcode == 0)
            .expect("wl_keyboard.keymap");
        let mut kr = keymap.reader();
        assert_eq!(kr.u32(), 1, "keymap format = XKB_V1");
        let km_size = kr.u32();
        let kfd = c.conn.take_fd().expect("keymap fd via SCM_RIGHTS");
        unsafe {
            let map = libc::mmap(
                std::ptr::null_mut(),
                km_size as usize,
                libc::PROT_READ,
                libc::MAP_SHARED,
                kfd,
                0,
            );
            assert_ne!(map, libc::MAP_FAILED, "keymap fd mmaps");
            let head = std::slice::from_raw_parts(map as *const u8, 11);
            assert_eq!(&head[..11], b"xkb_keymap ", "keymap content over the fd");
            libc::munmap(map, km_size as usize);
            libc::close(kfd);
        }

        // Inject synthetic input; assert the client receives the right events on the right objects.
        server.pointer_motion(10, 20);
        server.pointer_button(0x110, true); // BTN_LEFT down
        server.key(30, true); // KEY_A down
        server.modifiers(0, 0, 0, 0);
        let ev = c.poll_messages();

        // wl_pointer.enter(serial, surface, x, y) then motion(time, x, y).
        let enter = ev
            .iter()
            .find(|m| m.object == pointer && m.opcode == 0)
            .expect("pointer.enter");
        let mut r = enter.reader();
        let _serial = r.u32();
        assert_eq!(r.u32(), surface, "pointer entered the focused surface");
        assert_eq!(r.i32(), 10 * 256, "enter x (wl_fixed)");
        assert_eq!(r.i32(), 20 * 256, "enter y (wl_fixed)");
        let motion = ev
            .iter()
            .find(|m| m.object == pointer && m.opcode == 2)
            .expect("pointer.motion");
        let mut r = motion.reader();
        let _t = r.u32();
        assert_eq!(r.i32(), 10 * 256);
        assert_eq!(r.i32(), 20 * 256);
        let button = ev
            .iter()
            .find(|m| m.object == pointer && m.opcode == 3)
            .expect("pointer.button");
        let mut r = button.reader();
        let (_s, _t2, btn, state) = (r.u32(), r.u32(), r.u32(), r.u32());
        assert_eq!((btn, state), (0x110, 1), "BTN_LEFT pressed");

        // wl_keyboard.enter then key(serial, time, key, state).
        assert!(
            ev.iter().any(|m| m.object == keyboard && m.opcode == 1),
            "keyboard.enter"
        );
        let key = ev
            .iter()
            .find(|m| m.object == keyboard && m.opcode == 3)
            .expect("keyboard.key");
        let mut r = key.reader();
        let (_s, _t, code, kstate) = (r.u32(), r.u32(), r.u32(), r.u32());
        assert_eq!((code, kstate), (30, 1), "KEY_A pressed (raw evdev code)");
        assert!(
            ev.iter().any(|m| m.object == keyboard && m.opcode == 4),
            "keyboard.modifiers"
        );
        eprintln!("[headless] input events routed + asserted OK");
    }
}
