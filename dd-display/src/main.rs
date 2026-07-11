//! `dd-display` server binary: listen on a Wayland socket, accept guest clients, composite their
//! `wl_shm` surfaces. The presenter is chosen at runtime:
//!   - default on macOS: the native Cocoa window backend (one NSWindow per surface);
//!   - `--png <dir>` (any platform): dump each committed surface to a PNG — the headless proof path.
//!
//! Usage:
//!   dd-display [--socket <path>] [--png <dir>]
//! Env:
//!   WAYLAND_DISPLAY / XDG_RUNTIME_DIR — if `--socket` is absent, the socket path is
//!   `$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY` (default `$XDG_RUNTIME_DIR/wayland-0`).

use dd_display::present::{PngPresenter, Presenter};
use dd_display::server::Server;
use std::os::unix::io::RawFd;

fn main() {
    // Flag-gated Smithay-native compositor path. When `DD_DISPLAY_SMITHAY=1`, replace the hand-written
    // protocol machine below with the Smithay-based `dd-compositor` binary (same CLI + socket), execing
    // it in place so the daemon's launch semantics are preserved. When the flag is unset, everything
    // below runs unchanged — the legacy `server.rs` path is the untouched default, and this crate never
    // links smithay (keeping the Linux headless `cargo build -p dd-display` core build green + offline).
    maybe_exec_smithay();

    let mut args = std::env::args().skip(1);
    let mut socket: Option<String> = None;
    let mut png_dir: Option<String> = None;
    let mut metal = false;
    let mut window = false;
    let mut no_metal = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = args.next(),
            "--png" => png_dir = args.next(),
            "--metal" => metal = true,
            "--window" => window = true,
            "--no-metal" => no_metal = true,
            "selftest-input" => {
                // macOS-only: headless proof of the full live input round-trip (synthesized NSEvent →
                // wl_seat → guest client), dumping the live window to a PNG.
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-input.png".into());
                    dd_display::present_cocoa::selftest_input(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-input is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest" => {
                // Real-socket end-to-end proof (portable; run on the Mac to validate the macOS shm path).
                let out = args
                    .next()
                    .unwrap_or_else(|| "/tmp/dd-display-selftest.png".into());
                match dd_display::selftest::run(&out) {
                    Ok(()) => return,
                    Err(e) => {
                        eprintln!("dd-display selftest FAILED: {e}");
                        std::process::exit(1);
                    }
                }
            }
            "selftest-metal" => {
                // macOS-only: prove the Metal upload+blit+readback present path, dumping a PNG.
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-metal.png".into());
                    dd_display::metal::selftest_metal(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-metal is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-shader" => {
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-shader.png".into());
                    dd_display::metal_backend::selftest_shader(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-shader is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-shim-ir" => {
                #[cfg(target_os = "macos")]
                {
                    let irf = args.next().unwrap_or_default();
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-shim-ir.png".into());
                    let w = args.next().and_then(|s| s.parse().ok()).unwrap_or(256);
                    let h = args.next().and_then(|s| s.parse().ok()).unwrap_or(256);
                    let target = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
                    dd_display::metal_backend::selftest_shim_ir(&irf, &out, w, h, target);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-shim-ir is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-msl" => {
                #[cfg(target_os = "macos")]
                {
                    let f = args.next().unwrap_or_default();
                    dd_display::metal_backend::selftest_msl(&f);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-msl is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-texture" => {
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-texture.png".into());
                    dd_display::metal_backend::selftest_texture(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-texture is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-indexed" => {
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-indexed.png".into());
                    dd_display::metal_backend::selftest_indexed(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-indexed is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-replay" => {
                // macOS-only: GPU rung 3 — replay a streamed dd-gpu IR quad through the Metal executor.
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-replay.png".into());
                    dd_display::metal_backend::selftest_replay(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-replay is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-render" => {
                // macOS-only: GPU rung 3 — render a triangle into an IOSurface via a Metal render pass.
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-render.png".into());
                    dd_display::metal::selftest_render(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-render is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-iosurface" => {
                // macOS-only: prove the zero-copy IOSurface→MTLTexture composite path (GPU rung 2).
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-iosurface.png".into());
                    dd_display::metal::selftest_iosurface(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-iosurface is macOS-only");
                    std::process::exit(2);
                }
            }
            "selftest-cocoa" => {
                // macOS-only: prove the live on-screen NSView renders, dumping it to a PNG.
                #[cfg(target_os = "macos")]
                {
                    let out = args
                        .next()
                        .unwrap_or_else(|| "/tmp/dd-display-cocoa.png".into());
                    dd_display::present_cocoa::selftest_cocoa(&out);
                }
                #[cfg(not(target_os = "macos"))]
                {
                    eprintln!("selftest-cocoa is macOS-only");
                    std::process::exit(2);
                }
            }
            "-h" | "--help" => {
                eprintln!("usage: dd-display [--socket <path>] [--png <dir>]\n       dd-display selftest [out.png]\n       dd-display selftest-cocoa [out.png]  (macOS)");
                return;
            }
            _ => {}
        }
    }
    let socket = socket.unwrap_or_else(default_socket);

    let lfd = dd_display::listen_unix(&socket).unwrap_or_else(|e| {
        eprintln!("dd-display: cannot bind {socket}: {e}");
        std::process::exit(1);
    });
    eprintln!("dd-display: listening on {socket}");

    // Presenter selection.
    //   --window            → first-class LIVE mode: a real interactive NSWindow per surface, Metal-
    //                         accelerated (CAMetalLayer) by default, multi-client, NSEvent input routed
    //                         into each guest's wl_seat. `--no-metal` forces the CPU NSImageView blit.
    //   (no --png, no --window on macOS) → legacy single-client Cocoa window (the proven default).
    //   --png <dir>         → headless: dump each committed frame to a PNG (portable proof path).
    #[cfg(target_os = "macos")]
    {
        if window {
            let win_metal = !no_metal;
            return dd_display::present_cocoa::run_window(lfd, socket, win_metal);
        }
        if png_dir.is_none() {
            return dd_display::present_cocoa::run(lfd, socket, metal);
        }
    }
    let _ = (window, no_metal);
    let dir = png_dir.unwrap_or_else(|| "/tmp/dd-display".into());
    // `--png --metal`: dump each committed frame to a PNG, but composite via the Metal path — this is
    // what resolves an IOSurface-backed dmabuf buffer (GPU rung 2 proof). Plain `--png` uses the CPU path.
    #[cfg(target_os = "macos")]
    {
        if metal {
            serve_loop_metal(lfd, &dir);
            let _ = socket;
            return;
        }
    }
    let _ = metal;
    serve_loop(lfd, &dir);
    let _ = socket;
}

/// Like `serve_loop` but composites via `MetalPngPresenter` (real MTLDevice) so IOSurface-backed dmabuf
/// buffers resolve + composite zero-copy, dumping the GPU-rendered frame to a PNG.
#[cfg(target_os = "macos")]
fn serve_loop_metal(lfd: RawFd, dir: &str) {
    // Start the mach-port IOSurface handle bridge so guest dmabuf buffers resolve cross-process.
    dd_display::metal::start_gpu_bridge();
    // Start the dd-gpu IR executor (rung 3): the guest streams GPU commands here; we replay them on Metal
    // into the resolved IOSurface. Socket path: DD_GPU_EXEC_SOCK, else alongside the display socket.
    if let Some(p) = gpu_exec_sock() {
        std::thread::spawn(move || dd_display::metal_backend::run_executor(p));
    }
    let dir = dir.to_string();
    serve_multiplex(lfd, "metal-png", move || {
        dd_display::metal::MetalPngPresenter::new(&dir)
    });
}

/// Accept clients and pump each with a fresh PNG presenter. The Cocoa backend has its own run loop; this
/// drives the headless/PNG path.
fn serve_loop(lfd: RawFd, dir: &str) {
    let dir = dir.to_string();
    serve_multiplex(lfd, "png", move || Some(PngPresenter::new(&dir)));
}

/// Accept + service MANY concurrent clients on one thread via `poll()`. A single Wayland-native app can
/// hold several connections open at once — e.g. glmark2 keeps its own libwayland-client connection
/// (wl_compositor/output/seat + wl_egl_window) live for its whole run while the GL shim opens a SECOND
/// connection to commit the rendered IOSurface. The old one-client-at-a-time loop deadlocked on the first
/// connection and never serviced the second, so nothing composited. Here every client fd is polled and
/// pumped independently; a client is dropped when its pump reports EOF/error.
fn serve_multiplex<P: Presenter>(
    lfd: RawFd,
    tag: &str,
    mut make_presenter: impl FnMut() -> Option<P>,
) {
    set_nonblock(lfd);
    let mut clients: Vec<Server<P>> = Vec::new();
    loop {
        let mut pfds: Vec<libc::pollfd> = Vec::with_capacity(clients.len() + 1);
        pfds.push(libc::pollfd {
            fd: lfd,
            events: libc::POLLIN,
            revents: 0,
        });
        for c in &clients {
            pfds.push(libc::pollfd {
                fd: c.raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, -1) };
        if n < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        // Snapshot which client fds are ready BEFORE we mutate `clients` (accept grows it; a hang-up
        // shrinks it via swap_remove which also reorders). Servicing by fd — not by pfd index — keeps
        // this correct regardless of those mutations. `pfds[1..]` map 1:1 to the clients as they were.
        let ready_fds: Vec<RawFd> = pfds[1..]
            .iter()
            .filter(|p| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
            .map(|p| p.fd)
            .collect();
        // New connection(s) on the listen fd. New clients are serviced on the next poll iteration.
        if pfds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            loop {
                let cfd = unsafe { libc::accept(lfd, std::ptr::null_mut(), std::ptr::null_mut()) };
                if cfd < 0 {
                    break; // EAGAIN: no more pending connections
                }
                set_nonblock(cfd);
                match make_presenter() {
                    Some(p) => {
                        eprintln!(
                            "dd-display[{tag}]: client connected (fd {cfd}, {} live)",
                            clients.len() + 1
                        );
                        clients.push(Server::new(cfd, p));
                    }
                    None => {
                        eprintln!("dd-display[{tag}]: no presenter (no Metal device?)");
                        unsafe { libc::close(cfd) };
                    }
                }
            }
        }
        // Service each ready client (looked up by fd, so swap_remove reordering can't misindex it).
        for fd in ready_fds {
            let Some(idx) = clients.iter().position(|c| c.raw_fd() == fd) else {
                continue;
            };
            let pr = clients[idx].pump();
            if !matches!(pr, Ok(true)) {
                let why = match &pr {
                    Ok(false) => "EOF (peer closed)".to_string(),
                    Err(e) => format!("io error: {e}"),
                    _ => String::new(),
                };
                eprintln!(
                    "dd-display[{tag}]: client disconnected ({} frame(s)) [{why}]",
                    clients[idx].presenter().frame_count()
                );
                unsafe { libc::close(fd) };
                clients.swap_remove(idx);
            }
        }
    }
}

/// The dd-gpu IR executor socket path: `DD_GPU_EXEC_SOCK` if set, else `dd-gpu.sock` beside the display
/// socket. `None` only if no runtime dir is resolvable.
#[cfg(target_os = "macos")]
fn gpu_exec_sock() -> Option<String> {
    if let Ok(p) = std::env::var("DD_GPU_EXEC_SOCK") {
        return Some(p);
    }
    let disp = default_socket();
    let dir = std::path::Path::new(&disp).parent()?;
    Some(dir.join("dd-gpu.sock").to_string_lossy().into_owned())
}

fn default_socket() -> String {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let disp = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    if disp.starts_with('/') {
        disp
    } else {
        format!("{dir}/{disp}")
    }
}

fn set_nonblock(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

/// If `DD_DISPLAY_SMITHAY=1`, exec the `dd-compositor` binary (the Smithay-native path) in place of
/// this process, forwarding all CLI args. `dd-compositor` is expected next to this executable (both
/// land in the same `dd.app` bundle / `target/<profile>` dir). If it cannot be found or exec fails,
/// we log and fall through to the legacy `server.rs` path so the display never silently dies. This is
/// an exec, not a link dependency, so `dd-display` itself never pulls in smithay/libxkbcommon.
fn maybe_exec_smithay() {
    match std::env::var("DD_DISPLAY_SMITHAY").as_deref() {
        Ok("1") | Ok("true") | Ok("on") => {}
        _ => return,
    }
    use std::os::unix::process::CommandExt;
    let self_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("dd-display: DD_DISPLAY_SMITHAY set but current_exe failed: {e}; using legacy path");
            return;
        }
    };
    let bin = self_exe
        .parent()
        .map(|d| d.join("dd-compositor"))
        .unwrap_or_else(|| std::path::PathBuf::from("dd-compositor"));
    if !bin.exists() {
        eprintln!(
            "dd-display: DD_DISPLAY_SMITHAY set but {} not found; using legacy path",
            bin.display()
        );
        return;
    }
    eprintln!("dd-display: DD_DISPLAY_SMITHAY=1 -> exec {}", bin.display());
    let err = std::process::Command::new(&bin)
        .args(std::env::args_os().skip(1))
        .exec(); // only returns on failure
    eprintln!("dd-display: exec {} failed: {err}; using legacy path", bin.display());
}
