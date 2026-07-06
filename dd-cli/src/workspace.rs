//! `ddcli workspace` — configure and launch terminal workspaces (a named image+arch you develop in).
//!
//! The model + persistence live in `dd_term_core::workspace`; this is the CLI surface the dd-gui invokes
//! (`ddcli workspace launch <name>`). `launch` runs the workspace as an interactive terminal in the
//! current window via a raw-mode PTY passthrough. The launcher is `LocalShellLauncher` today (a real
//! shell, so this works + is exercisable everywhere); the macOS build swaps in a dd-jit launcher that
//! enters the image's container with a persistent writable upper.

use crate::cli::WorkspaceCmd;
use crate::paths;
use dd_term_core::pty::local::LocalPty;
use dd_term_core::workspace::{Arch, LocalShellLauncher, Launcher, Workspace, WorkspaceStore};
use dd_term_core::PtyBackend;
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

    // Enter the workspace's IMAGE via the dd daemon when it's available (a real container with a
    // persistent named layer); otherwise fall back to a plain host shell so `launch` still works in dev.
    let launched = if paths::socket().exists() {
        DockerLauncher::new().launch(&ws, cols, rows)
    } else {
        eprintln!("[dd] (no daemon socket — running a local shell; start the daemon to enter the image)");
        LocalShellLauncher::default().launch(&ws, cols, rows)
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

/// A launcher that enters the workspace's image as a real dd container, via the stock `docker` CLI
/// pointed at the dd daemon socket (the same proven path `ddcli run` + the GUI terminal use). The
/// workspace persists as a stable named container: relaunch reuses it (with all installed packages /
/// files), so it is a dev environment you return to.
struct DockerLauncher {
    docker_host: String,
}

impl DockerLauncher {
    fn new() -> DockerLauncher {
        DockerLauncher { docker_host: paths::docker_host() }
    }
}

impl Launcher for DockerLauncher {
    fn launch(&self, ws: &Workspace, cols: u16, rows: u16) -> std::io::Result<Box<dyn PtyBackend>> {
        let cname = format!("dd-ws-{}", ws_container_name(&ws.name));
        let platform = ws.arch.platform().map(|p| format!("--platform {p} ")).unwrap_or_default();
        // Prefer a plain interactive `bash`, falling back to `sh` inside the container.
        let shell = "$( command -v bash >/dev/null 2>&1 && echo bash || echo sh )";
        // Reattach to the persistent container if it exists (start a stopped one, or exec into a running
        // one); otherwise create it fresh (named, NOT --rm, so its writable layer survives).
        let script = format!(
            "if docker inspect {c} >/dev/null 2>&1; then \
                 docker start {c} >/dev/null 2>&1; \
                 exec docker exec -it {c} {sh}; \
             else \
                 exec docker run -it --name {c} {plat}{img} {sh}; \
             fi",
            c = cname,
            plat = platform,
            img = sh_quote(&ws.image),
            sh = shell,
        );
        let pty = LocalPty::spawn(
            &["/bin/sh", "-c", &script],
            cols,
            rows,
            &[("DOCKER_HOST", &self.docker_host), ("TERM", "xterm-256color")],
        )?;
        Ok(Box::new(pty))
    }
}

/// Sanitize a workspace name into a docker-safe container-name component.
fn ws_container_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Raw-mode passthrough between the real terminal and the workspace PTY until the child exits.
fn run_inline(pty: &mut dyn dd_term_core::PtyBackend) -> i32 {
    let raw = RawMode::enter(libc::STDIN_FILENO);
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
    }
    let master = pty.master_fd().unwrap_or(-1);
    let mut buf = [0u8; 8192];
    let mut exit_code = 0;
    loop {
        if WINCH.swap(false, Ordering::SeqCst) {
            if let Some((c, r)) = term_size() {
                pty.resize(c, r);
            }
        }
        let mut fds = [
            libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 },
        ];
        let pr = unsafe { libc::poll(fds.as_mut_ptr(), 2, 100) };
        if pr < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue; // interrupted by SIGWINCH
            }
            break;
        }
        // Terminal → workspace stdin.
        if fds[0].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n > 0 {
                let _ = pty.write(&buf[..n as usize]);
            } else if n == 0 {
                let _ = pty.write(&[]); // EOF → close stdin
            }
        }
        // Workspace output → terminal.
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let n = pty.read(&mut buf).unwrap_or(0);
            if n > 0 {
                let mut out = std::io::stdout();
                let _ = out.write_all(&buf[..n]);
                let _ = out.flush();
                continue;
            }
        }
        if let Some(code) = pty.try_wait() {
            // Drain any final output before exiting.
            loop {
                let n = pty.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                let _ = std::io::stdout().write_all(&buf[..n]);
            }
            let _ = std::io::stdout().flush();
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
