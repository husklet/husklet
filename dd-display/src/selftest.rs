//! `dd-display selftest <out.png>` — a REAL-socket end-to-end proof of the CPU path, portable to macOS.
//!
//! Unlike the in-crate unit test (which uses a `socketpair`), this binds a real `AF_UNIX` listening
//! socket, `fork`s a Wayland client that backs a `wl_shm` pool with an anonymous shared file
//! (`memfd_create` on Linux, `shm_open`+`shm_unlink` on macOS — the exact fallback the engine's
//! `memfd_create` uses), draws `weston-simple-shm`'s `(x^y)` pattern, and passes the pool fd over
//! `SCM_RIGHTS`. The parent accepts, composites, and dumps a PNG. Running this on the Mac retires
//! validation risk #2 (cross-process shm-fd `mmap` on macOS) on the actual target OS.

use crate::present::PngPresenter;
use crate::server::Server;
use crate::wire::{Conn, Message};
use std::os::unix::io::RawFd;

const WL_DISPLAY: u32 = 1;

/// Create an anonymous, shared, ftruncatable fd — the wl_shm-pool backing. Portable across Linux/macOS.
fn anon_shared_fd(size: usize) -> RawFd {
    #[cfg(target_os = "linux")]
    let fd = {
        let name = std::ffi::CString::new("dd-shm-selftest").unwrap();
        unsafe { libc::memfd_create(name.as_ptr(), 0) }
    };
    #[cfg(not(target_os = "linux"))]
    let fd = {
        // macOS: shm_open a uniquely-named object, then immediately unlink it → anonymous, lives with the fd.
        let nm = format!("/dd{}.{}", unsafe { libc::getpid() }, std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos());
        let c = std::ffi::CString::new(nm).unwrap();
        unsafe {
            let fd = libc::shm_open(c.as_ptr(), libc::O_RDWR | libc::O_CREAT | libc::O_EXCL, 0o600);
            if fd >= 0 {
                libc::shm_unlink(c.as_ptr());
            }
            fd
        }
    };
    assert!(fd >= 0, "anon shared fd failed");
    assert_eq!(unsafe { libc::ftruncate(fd, size as libc::off_t) }, 0, "ftruncate failed");
    fd
}

fn nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

/// Connect a client to `sock`, draw a frame, pass the pool fd via SCM_RIGHTS. Runs in the forked child.
pub fn client(sock: &str) {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0);
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as _;
    for (i, b) in sock.as_bytes().iter().enumerate() {
        addr.sun_path[i] = *b as _;
    }
    let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    // Retry connect until the parent has bound/listened.
    for _ in 0..200 {
        if unsafe { libc::connect(fd, &addr as *const _ as *const _, len) } == 0 {
            break;
        }
        unsafe { libc::usleep(5000) };
    }
    nonblock(fd);
    let mut c = Conn::new(fd);

    let (w, h): (i32, i32) = (256, 160);
    let stride = w * 4;
    let size = (stride * h) as usize;

    let (reg, comp, shm, wm, surface, xdg, toplevel, pool, buffer) = (2u32, 3, 4, 5, 6, 7, 8, 9, 10);
    c.send(&Message::new(WL_DISPLAY, 1).u32(reg)); // get_registry
    drain(&mut c);
    c.send(&Message::new(reg, 0).u32(1).string("wl_compositor").u32(4).u32(comp));
    c.send(&Message::new(reg, 0).u32(2).string("wl_shm").u32(1).u32(shm));
    c.send(&Message::new(reg, 0).u32(3).string("xdg_wm_base").u32(1).u32(wm));
    c.send(&Message::new(comp, 0).u32(surface));
    c.send(&Message::new(wm, 2).u32(xdg).u32(surface));
    c.send(&Message::new(xdg, 1).u32(toplevel));
    c.send(&Message::new(toplevel, 2).string("weston-simple-shm (dd real-socket)"));
    c.send(&Message::new(surface, 6)); // initial commit
    drain(&mut c);
    c.send(&Message::new(xdg, 4).u32(1)); // ack_configure

    let mfd = anon_shared_fd(size);
    let map = unsafe {
        libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, mfd, 0)
    };
    assert_ne!(map, libc::MAP_FAILED);
    let px = map as *mut u8;
    for y in 0..h {
        for x in 0..w {
            let o = (y * stride + x * 4) as usize;
            let (b, g, r);
            if x < 8 || x >= w - 8 || y < 8 || y >= h - 8 {
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
    c.flush().unwrap();
    unsafe { libc::close(mfd) };
    c.send(&Message::new(pool, 0).u32(buffer).i32(0).i32(w).i32(h).i32(stride).u32(1)); // XRGB8888
    c.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0));
    c.send(&Message::new(surface, 2).i32(0).i32(0).i32(w).i32(h));
    c.send(&Message::new(surface, 6)); // commit
    c.flush().unwrap();
    // Give the server time to read the commit before we exit.
    unsafe {
        libc::usleep(200_000);
        libc::munmap(map, size);
    }
}

/// An input-round-trip test client: maps a toplevel (so a window opens + gains focus), binds
/// `wl_seat`→`wl_pointer`/`wl_keyboard`, then reads seat events for `run_ms` and appends a decoded line
/// per event to `results` (also echoed to stderr). The parent (`selftest_input`) synthesizes NSEvents into
/// the live window and asserts the expected lines appear — proving NSEvent → seat → wire → client delivery.
/// Runs in the forked child.
pub fn input_client(sock: &str, results: &str, run_ms: u64) {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(fd >= 0);
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as _;
    for (i, b) in sock.as_bytes().iter().enumerate() {
        addr.sun_path[i] = *b as _;
    }
    let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    for _ in 0..200 {
        if unsafe { libc::connect(fd, &addr as *const _ as *const _, len) } == 0 {
            break;
        }
        unsafe { libc::usleep(5000) };
    }
    nonblock(fd);
    let mut c = Conn::new(fd);

    // ids: reg=2 comp=3 shm=4 wm=5 seat=6 surface=7 xdg=8 toplevel=9 pool=10 buffer=11 ptr=12 kbd=13
    let (reg, comp, shm, wm, seat, surface, xdg, toplevel, pool, buffer, ptr, kbd) =
        (2u32, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13);
    c.send(&Message::new(WL_DISPLAY, 1).u32(reg)); // get_registry
    drain(&mut c);
    c.send(&Message::new(reg, 0).u32(1).string("wl_compositor").u32(4).u32(comp));
    c.send(&Message::new(reg, 0).u32(2).string("wl_shm").u32(1).u32(shm));
    c.send(&Message::new(reg, 0).u32(3).string("xdg_wm_base").u32(1).u32(wm));
    c.send(&Message::new(reg, 0).u32(4).string("wl_seat").u32(5).u32(seat));
    c.send(&Message::new(comp, 0).u32(surface));
    c.send(&Message::new(wm, 2).u32(xdg).u32(surface));
    c.send(&Message::new(xdg, 1).u32(toplevel));
    c.send(&Message::new(toplevel, 2).string("dd input round-trip"));
    c.send(&Message::new(seat, 0).u32(ptr)); // get_pointer
    c.send(&Message::new(seat, 1).u32(kbd)); // get_keyboard
    c.send(&Message::new(surface, 6)); // initial commit → configure handshake
    drain(&mut c);
    c.send(&Message::new(xdg, 4).u32(1)); // ack_configure

    // Back the surface with a small frame so a window opens (and gains input focus).
    let (w, h): (i32, i32) = (200, 120);
    let stride = w * 4;
    let size = (stride * h) as usize;
    let mfd = anon_shared_fd(size);
    let map = unsafe {
        libc::mmap(std::ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, mfd, 0)
    };
    assert_ne!(map, libc::MAP_FAILED);
    let px = map as *mut u8;
    for i in 0..size {
        unsafe { *px.add(i) = if i % 4 == 2 { 0x40 } else { 0x20 } }; // dim blue-ish fill
    }
    c.send(&Message::new(shm, 0).u32(pool).u32(size as u32)); // create_pool (fd OOB)
    c.queue_fd(mfd);
    c.flush().unwrap();
    unsafe { libc::close(mfd) };
    c.send(&Message::new(pool, 0).u32(buffer).i32(0).i32(w).i32(h).i32(stride).u32(1)); // XRGB8888
    c.send(&Message::new(surface, 1).u32(buffer).i32(0).i32(0));
    c.send(&Message::new(surface, 6)); // commit → window appears, focus granted
    c.flush().unwrap();

    let mut log = std::fs::OpenOptions::new().create(true).append(true).open(results).ok();
    let mut emit = |line: String| {
        eprintln!("CLIENT-INPUT: {line}");
        if let Some(f) = log.as_mut() {
            use std::io::Write;
            let _ = writeln!(f, "{line}");
            let _ = f.flush();
        }
    };

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(run_ms);
    while std::time::Instant::now() < deadline {
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 30) };
        match c.fill().unwrap_or(0) {
            0 => break, // server closed
            _ => {}
        }
        while let Some(m) = c.next_message() {
            let mut r = m.reader();
            if m.object == ptr {
                match m.opcode {
                    0 => { let s = r.u32(); let surf = r.u32(); let x = r.i32(); let y = r.i32();
                           emit(format!("PTR_ENTER serial={s} surface={surf} x={} y={}", x / 256, y / 256)); }
                    1 => emit("PTR_LEAVE".into()),
                    2 => { let _t = r.u32(); let x = r.i32(); let y = r.i32();
                           emit(format!("PTR_MOTION x={} y={}", x / 256, y / 256)); }
                    3 => { let _s = r.u32(); let _t = r.u32(); let b = r.u32(); let st = r.u32();
                           emit(format!("PTR_BUTTON button={b} state={st}")); }
                    4 => { let _t = r.u32(); let ax = r.u32(); let v = r.i32();
                           emit(format!("PTR_AXIS axis={ax} value={}", v / 256)); }
                    _ => {}
                }
            } else if m.object == kbd {
                match m.opcode {
                    0 => { // keymap(format, size) — the fd rides OOB; consume + close it.
                        if let Some(kfd) = c.take_fd() { unsafe { libc::close(kfd) }; }
                        emit("KBD_KEYMAP".into());
                    }
                    1 => { let s = r.u32(); let surf = r.u32(); emit(format!("KBD_ENTER serial={s} surface={surf}")); }
                    2 => emit("KBD_LEAVE".into()),
                    3 => { let _s = r.u32(); let _t = r.u32(); let k = r.u32(); let st = r.u32();
                           emit(format!("KBD_KEY key={k} state={st}")); }
                    4 => { let _s = r.u32(); let dep = r.u32(); emit(format!("KBD_MOD depressed={dep}")); }
                    _ => {}
                }
            }
        }
    }
    unsafe { libc::munmap(map, size) };
}

fn drain(c: &mut Conn) {
    unsafe { libc::usleep(20_000) };
    loop {
        match c.fill().unwrap_or(0) {
            0 | -1 => break,
            _ => {}
        }
    }
    while c.next_message().is_some() {}
}

/// Run the real-socket self-test: fork a client, serve it, dump a PNG. Returns the PNG path.
pub fn run(out: &str) -> std::io::Result<()> {
    let sock = format!("/tmp/dd-display-selftest-{}.sock", unsafe { libc::getpid() });
    let _ = std::fs::remove_file(&sock);
    let lfd = crate::listen_unix(&sock)?;

    let pid = unsafe { libc::fork() };
    if pid == 0 {
        client(&sock);
        unsafe { libc::_exit(0) };
    }

    let cfd = loop {
        let fd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
        if fd >= 0 {
            break fd;
        }
        if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            return Err(std::io::Error::last_os_error());
        }
    };
    nonblock(cfd);
    let dir = std::path::Path::new(out).parent().map(|p| p.to_path_buf()).unwrap_or_else(|| ".".into());
    let mut server = Server::new(cfd, PngPresenter::new(&dir));

    // Pump until the client has committed a frame (or a short deadline elapses).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while server.presenter().frames == 0 && std::time::Instant::now() < deadline {
        let mut pfd = libc::pollfd { fd: cfd, events: libc::POLLIN, revents: 0 };
        unsafe { libc::poll(&mut pfd, 1, 50) };
        if !server.pump()? {
            break;
        }
    }
    unsafe {
        libc::waitpid(pid, std::ptr::null_mut(), 0);
        libc::close(cfd);
        libc::close(lfd);
    }
    let _ = std::fs::remove_file(&sock);

    let produced = dir.join("surface-6.png");
    if produced != std::path::Path::new(out) {
        let _ = std::fs::rename(&produced, out);
    }
    if server.presenter().frames == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "no frame composited"));
    }
    println!("dd-display selftest: composited 1 frame over a real AF_UNIX socket -> {out}");
    Ok(())
}
