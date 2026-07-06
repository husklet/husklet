//! `ddcli workspace` — configure and launch terminal workspaces (a named image+arch you develop in).
//!
//! The model + persistence live in `dd_term_core::workspace`; this is the CLI surface the dd-gui invokes
//! (`ddcli workspace launch <name>`). `launch` runs the workspace as an interactive terminal in the
//! current window via a raw-mode PTY passthrough. The launcher is `LocalShellLauncher` today (a real
//! shell, so this works + is exercisable everywhere); the macOS build swaps in a dd-jit launcher that
//! enters the image's container with a persistent writable upper.

use crate::cli::WorkspaceCmd;
use crate::paths;
use dd_term_core::workspace::{Arch, LocalShellLauncher, Launcher, Workspace, WorkspaceStore};
use std::io::Write;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

fn store_path() -> std::path::PathBuf {
    paths::dd_root().join("workspaces.conf")
}

pub(crate) fn run(action: WorkspaceCmd) {
    match action {
        WorkspaceCmd::List => list(),
        WorkspaceCmd::Create { name, image, arch } => create(name, image, arch),
        WorkspaceCmd::Rm { name } => rm(name),
        WorkspaceCmd::Launch { name } => launch(name),
    }
}

fn list() {
    let store = WorkspaceStore::load(store_path());
    if store.all().is_empty() {
        println!("no workspaces yet — create one with:  ddcli workspace create <name> --image <img>");
        return;
    }
    println!("{:<20} {:<14} {}", "NAME", "ARCH", "IMAGE");
    for w in store.all() {
        println!("{:<20} {:<14} {}", w.name, w.arch.as_str(), w.image);
    }
}

fn create(name: String, image: String, arch: String) {
    let Some(arch) = Arch::parse(&arch) else {
        eprintln!("unknown arch {arch:?} (use arm64 | amd64 | darwin-arm64)");
        std::process::exit(2);
    };
    let mut store = WorkspaceStore::load(store_path());
    if let Err(e) = store.upsert(Workspace::new(&name, &image, arch)) {
        eprintln!("failed to save workspace: {e}");
        std::process::exit(1);
    }
    println!("workspace {name:?} → {image} ({})  saved. launch it:  ddcli workspace launch {name}", arch.as_str());
}

fn rm(name: String) {
    let mut store = WorkspaceStore::load(store_path());
    match store.remove(&name) {
        Ok(true) => println!("removed workspace {name:?}"),
        Ok(false) => eprintln!("no workspace named {name:?}"),
        Err(e) => {
            eprintln!("failed: {e}");
            std::process::exit(1);
        }
    }
}

static WINCH: AtomicBool = AtomicBool::new(false);
extern "C" fn on_winch(_sig: libc::c_int) {
    WINCH.store(true, Ordering::SeqCst);
}

fn launch(name: String) {
    let store = WorkspaceStore::load(store_path());
    let Some(ws) = store.get(&name).cloned() else {
        eprintln!("no workspace named {name:?} — see:  ddcli workspace list");
        std::process::exit(2);
    };
    let (cols, rows) = term_size().unwrap_or((80, 24));
    eprintln!("[dd] launching workspace {:?} ({} · {})", ws.name, ws.image, ws.arch.as_str());

    // Enter the workspace's IMAGE as a real container IN-PROCESS via dd-jit — no daemon, no docker, no
    // socket. When this host's engine can't run the workspace's arch (e.g. a non-macOS dev box), fall
    // back to a plain host shell so `launch` still works for iterating on the terminal itself.
    let launched = match crate::ddjit_launcher::launch(&ws, cols, rows) {
        Ok(pty) => Ok(pty),
        Err(e) => {
            eprintln!("[dd] (dd-jit unavailable here — {e}; running a local shell instead)");
            LocalShellLauncher::default().launch(&ws, cols, rows)
        }
    };
    let mut pty = match launched {
        Ok(p) => p,
        Err(e) => {
            eprintln!("failed to launch: {e}");
            std::process::exit(1);
        }
    };
    let code = run_inline(&mut *pty);
    std::process::exit(code);
}

/// Raw-mode passthrough between the real terminal and the workspace PTY until the child exits.
/// Backend-agnostic: it only polls the terminal's stdin and drains `pty.read()` each tick, so it works
/// for a `LocalPty` (a real master fd) and for `DdJitPty` (output drained from dd-jit's broadcast) alike.
fn run_inline(pty: &mut dyn dd_term_core::PtyBackend) -> i32 {
    let raw = RawMode::enter(libc::STDIN_FILENO);
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
    }
    let mut buf = [0u8; 8192];
    let mut out = std::io::stdout();
    let exit_code;
    loop {
        if WINCH.swap(false, Ordering::SeqCst) {
            if let Some((c, r)) = term_size() {
                pty.resize(c, r);
            }
        }
        // Wait briefly for terminal input; the short timeout also paces output draining.
        let mut pfd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
        let pr = unsafe { libc::poll(&mut pfd, 1, 10) };
        if pr < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            exit_code = 1;
            break;
        }
        // Terminal → workspace stdin.
        if pr > 0 && pfd.revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                let _ = pty.write(&buf[..n as usize]);
            } else if n == 0 {
                let _ = pty.write(&[]); // EOF → close stdin
            }
        }
        // Drain workspace output → terminal.
        let mut wrote = false;
        loop {
            let n = pty.read(&mut buf).unwrap_or(0);
            if n == 0 {
                break;
            }
            let _ = out.write_all(&buf[..n]);
            wrote = true;
        }
        if wrote {
            let _ = out.flush();
        }
        if let Some(code) = pty.try_wait() {
            // Final drain after exit (buffered tail), then stop.
            loop {
                let n = pty.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                let _ = out.write_all(&buf[..n]);
            }
            let _ = out.flush();
            exit_code = code;
            break;
        }
    }
    drop(raw);
    exit_code
}

/// Query the controlling terminal's size (cols, rows).
fn term_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

/// RAII raw-mode for a tty fd; restores the saved termios on drop.
struct RawMode {
    fd: RawFd,
    saved: Option<libc::termios>,
}
impl RawMode {
    fn enter(fd: RawFd) -> RawMode {
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut t) != 0 {
                return RawMode { fd, saved: None }; // not a tty (e.g. piped) — leave as-is
            }
            let saved = t;
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(fd, libc::TCSANOW, &t);
            RawMode { fd, saved: Some(saved) }
        }
    }
}
impl Drop for RawMode {
    fn drop(&mut self) {
        if let Some(saved) = self.saved {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &saved);
            }
        }
    }
}
