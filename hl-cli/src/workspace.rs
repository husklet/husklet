//! `ddcli workspace` — configure and launch terminal workspaces (a named image+arch you develop in).
//!
//! The model + persistence live in `hl_term::workspace`; this is the CLI surface the dd-gui invokes
//! (`ddcli workspace launch <name>`). `launch` runs the workspace as an interactive terminal in the
//! current window via a raw-mode PTY passthrough. The launcher is `LocalShellLauncher` today (a real
//! shell, so this works + is exercisable everywhere); the macOS build swaps in a dd-jit launcher that
//! enters the image's container with a persistent writable upper.

use crate::cli::WorkspaceCmd;
use crate::paths;
use hl_term::workspace::{Arch, CudaDevice, LocalShellLauncher, Launcher, VpnConfig, Workspace, WorkspaceStore};
use std::io::Write;
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};

fn store_path() -> std::path::PathBuf {
    paths::dd_root().join("workspaces.conf")
}

pub(crate) fn run(action: WorkspaceCmd) {
    match action {
        WorkspaceCmd::List => list(),
        WorkspaceCmd::Create { name, image, arch, vpn, cuda, gui } => create(name, image, arch, vpn, cuda, gui),
        WorkspaceCmd::Rm { name } => rm(name),
        WorkspaceCmd::Launch { name, restore, cwd, slot } => launch(name, restore, cwd, slot),
        WorkspaceCmd::Restore { name } => launch(name, true, None, None),
        WorkspaceCmd::Checkpoint { name, slot } => checkpoint(name, slot),
        WorkspaceCmd::Daemon { name } => match crate::wsdaemon::ensure(&name) {
            Ok(sock) => println!("{}", sock.display()),
            Err(e) => {
                eprintln!("failed to start workspace daemon: {e}");
                std::process::exit(1);
            }
        },
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

/// Build the workspace to persist for `workspace create`, merging the create-flags over any PRIOR
/// workspace of the same name. This is the pure core of [`create`] (no IO, no `exit`) so it is unit-tested.
///
/// Re-creating an existing workspace is an UPDATE, not a wipe: every field the CLI does not expose
/// (`env`, `mounts`, `cpus`, `memory_mb`, `storage`, `shell`, `scrollback`, `docker_sock`) is carried over
/// from `prior`, and `--arch` when omitted keeps the workspace's current arch (a *fresh* workspace defaults
/// to arm64). Only identity (image, and arch when given) and the CLI-exposed flags (`vpn`/`cuda`/`gui`) are
/// (re)written, each with three-way semantics: absent → preserve prior, `off`/`none`/`""` → clear, else set.
/// Returns `Err(message)` on any invalid input (bad arch / vpn / cuda spec).
fn build_workspace(
    prior: Option<&Workspace>,
    name: &str,
    image: &str,
    arch: Option<&str>,
    vpn: Option<&str>,
    cuda: Option<&str>,
    gui: Option<&str>,
) -> Result<Workspace, String> {
    // arch: explicit `--arch` always wins; omitted keeps the prior workspace's arch, else defaults arm64.
    let arch = match arch {
        Some(s) => Arch::parse(s).ok_or_else(|| format!("unknown arch {s:?} (use arm64 | amd64 | darwin-arm64)"))?,
        None => prior.map(|w| w.arch).unwrap_or(Arch::Arm64),
    };
    let vpn_cfg = match vpn {
        None => prior.and_then(|w| w.vpn.clone()),
        Some(s) if s.trim().is_empty() || s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") => None,
        Some(s) => Some(
            VpnConfig::parse(s)
                .ok_or_else(|| format!("invalid --vpn {s:?} (use e.g. `socks5:127.30.0.1:1080` or a bare `host:port`)"))?,
        ),
    };
    // Same three-way semantics as `--vpn`, plus a bare `--cuda` (default_missing_value "default") = the
    // default simulated device: absent → preserve prior; `off`/`none`/`""` → clear; `default` → default
    // device; anything else → parse a `Name|cc|VRAM-MB` spec.
    let cuda_cfg = match cuda {
        None => prior.and_then(|w| w.cuda.clone()),
        Some(s) if s.trim().is_empty() || s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") => None,
        Some(s) if s.eq_ignore_ascii_case("default") => Some(CudaDevice::default_device()),
        Some(s) => Some(
            CudaDevice::parse(s)
                .ok_or_else(|| format!("invalid --cuda {s:?} (use `--cuda`, `--cuda \"Name|8.6|8192\"`, or `--cuda off`)"))?,
        ),
    };
    // Same three-way semantics: absent → preserve prior; `off`/`none`/`false`/`""` → clear; anything else → on.
    let gui_on = match gui {
        None => prior.map(|w| w.gui).unwrap_or(false),
        Some(s) if s.trim().is_empty() || s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") || s.eq_ignore_ascii_case("false") => false,
        Some(_) => true,
    };
    // Start from the prior workspace so its non-CLI-exposed config survives the update; only re-create from
    // scratch (with the safe defaults) when this is a brand-new workspace.
    let mut ws = prior.cloned().unwrap_or_else(|| Workspace::new(name, image, arch));
    ws.name = name.to_string();
    ws.image = image.to_string();
    ws.arch = arch;
    ws.vpn = vpn_cfg;
    ws.cuda = cuda_cfg;
    ws.gui = gui_on;
    Ok(ws)
}

fn create(name: String, image: String, arch: Option<String>, vpn: Option<String>, cuda: Option<String>, gui: Option<String>) {
    let mut store = WorkspaceStore::load(store_path());
    let ws = match build_workspace(store.get(&name), &name, &image, arch.as_deref(), vpn.as_deref(), cuda.as_deref(), gui.as_deref()) {
        Ok(w) => w,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let arch = ws.arch;
    let vpn_note = ws.vpn.as_ref().map(|c| format!("  vpn={}", c.to_spec())).unwrap_or_default();
    let cuda_note = ws.cuda.as_ref().map(|c| format!("  cuda={}", c.to_spec())).unwrap_or_default();
    let gui_note = if ws.gui { "  gui=on" } else { "" };
    if let Err(e) = store.upsert(ws) {
        eprintln!("failed to save workspace: {e}");
        std::process::exit(1);
    }
    println!("workspace {name:?} → {image} ({}){vpn_note}{cuda_note}{gui_note}  saved. launch it:  ddcli workspace launch {name}", arch.as_str());
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

/// Checkpoint a running workspace's whole process tree to disk. Reads the init pid recorded at launch,
/// sends the engine's checkpoint control signal, and waits for the complete on-disk checkpoint.
fn checkpoint(name: String, slot: Option<String>) {
    let store = WorkspaceStore::load(store_path());
    let Some(ws) = store.get(&name).cloned() else {
        eprintln!("no workspace named {name:?} — see:  ddcli workspace list");
        std::process::exit(2);
    };
    // A per-pane SLOT freezes that pane's own engine into `<storage>/checkpoint/<slot>`; None uses the
    // single shared slot (back-compat).
    let (pid_file, ckpt_dir) = match slot.as_deref() {
        Some(s) => (ws.checkpoint_slot_pid_file(&paths::dd_root(), s), ws.checkpoint_slot_dir(&paths::dd_root(), s)),
        None => (ws.checkpoint_pid_file(&paths::dd_root()), ws.checkpoint_dir(&paths::dd_root())),
    };
    let pid: u32 = match std::fs::read_to_string(&pid_file).ok().and_then(|s| s.trim().parse().ok()) {
        Some(p) => p,
        None => {
            eprintln!("workspace {name:?} is not running (no checkpoint pid recorded)");
            std::process::exit(1);
        }
    };
    let rt = match hl_jit::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("checkpoint failed: {e}");
            std::process::exit(1);
        }
    };
    match rt.checkpoint(pid, &ckpt_dir.to_string_lossy(), std::time::Duration::from_secs(30)) {
        Ok(()) => {
            let _ = std::fs::remove_file(&pid_file);
            println!("workspace {name:?} checkpointed → {}", ckpt_dir.display());
            println!("resume it with:  ddcli workspace launch {name} --restore");
        }
        Err(e) => {
            eprintln!("checkpoint of workspace {name:?} failed: {e}");
            std::process::exit(1);
        }
    }
}

fn launch(name: String, restore: bool, cwd: Option<String>, slot: Option<String>) {
    let store = WorkspaceStore::load(store_path());
    let Some(ws) = store.get(&name).cloned() else {
        eprintln!("no workspace named {name:?} — see:  ddcli workspace list");
        std::process::exit(2);
    };
    let (cols, rows) = term_size().unwrap_or((80, 24));

    // Enter the workspace's IMAGE as a real container IN-PROCESS via dd-jit — no daemon, no docker, no
    // socket. When `restore`, resume the last whole-workspace checkpoint (process tree) instead of a fresh
    // shell. When this host's engine can't run the workspace's arch, fall back to a plain host shell.
    let launched = match crate::ddjit_launcher::launch_ex(&ws, cols, rows, restore, cwd.as_deref(), slot.as_deref()) {
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
fn run_inline(pty: &mut dyn hl_term::PtyBackend) -> i32 {
    let raw = RawMode::enter(libc::STDIN_FILENO);
    unsafe {
        libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t);
    }
    let mut buf = [0u8; 8192];
    let mut out = std::io::stdout();
    let exit_code;
    // Track the last size we pushed to the guest so we can POLL for changes, not just react to
    // SIGWINCH. The GUI resizes our controlling PTY via VTE's `set_size`, which (unlike a plain
    // ioctl) does not reliably deliver SIGWINCH to us — and the very first resize can land while
    // we're still booting the engine (before the handler is armed). Polling every tick makes the
    // guest's window size track the terminal regardless, so full-screen apps (htop) fill the window.
    let mut last_size = term_size();
    if let Some((c, r)) = last_size {
        pty.resize(c, r);
    }
    let mut ticks: u32 = 0;
    // Once stdin reaches EOF (or is a non-pollable source we've drained), drop it from the poll set.
    // Rationale: when stdin is a *redirected regular file* (`ddcli … < cmds.sh`), `poll()` on a regular
    // file returns POLLIN *immediately* every iteration and never blocks — after we've read the file to
    // EOF, re-polling+re-reading it would free-run the loop at 100% CPU (read→EOF→write(&[])→drain→
    // try_wait, forever). So after EOF we stop polling stdin and pace the loop with a plain 10ms sleep,
    // which keeps output draining without pinning a core. Interactive-tty stdin never hits EOF here, so
    // its blocking-poll behavior is unchanged.
    let mut stdin_open = true;
    loop {
        let winched = WINCH.swap(false, Ordering::SeqCst);
        ticks = ticks.wrapping_add(1);
        // On SIGWINCH, or every ~300ms as a fallback, re-read the terminal size and push any change.
        if winched || ticks % 30 == 0 {
            let now = term_size();
            if now != last_size {
                if let Some((c, r)) = now {
                    pty.resize(c, r);
                }
                last_size = now;
            }
        }
        // Wait briefly for terminal input; the short timeout also paces output draining. Once stdin is
        // EOF/closed we no longer poll it (a redirected regular file would otherwise always report POLLIN
        // and defeat the timeout) — just sleep the same 10ms to pace the output drain below.
        if stdin_open {
            let mut pfd = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
            let pr = unsafe { libc::poll(&mut pfd, 1, 10) };
            if pr < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                exit_code = 1;
                break;
            }
            // Terminal → workspace stdin.
            if pr > 0 && pfd.revents & libc::POLLIN != 0 {
                let n =
                    unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut _, buf.len()) };
                if n > 0 {
                    let _ = pty.write(&buf[..n as usize]);
                } else if n == 0 {
                    let _ = pty.write(&[]); // EOF → close stdin
                    stdin_open = false; // stop polling a drained/EOF'd stdin (kills the busy-spin)
                } else {
                    // read error on a stdin poll'd readable: don't spin on it, treat as closed.
                    stdin_open = false;
                }
            }
        } else {
            // stdin drained: pace the loop without busy-spinning.
            let ts = libc::timespec { tv_sec: 0, tv_nsec: 10_000_000 };
            unsafe { libc::nanosleep(&ts, std::ptr::null_mut()) };
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

#[cfg(test)]
mod tests {
    use super::*;
    use hl_term::workspace::{Mount, VpnKind};

    // A prior workspace carrying config that the `create` CLI does NOT expose as flags — exactly the
    // state a user builds via `~/.dd/workspaces.conf` or the GUI. Re-running `create` must preserve it.
    fn prior_rich() -> Workspace {
        let mut w = Workspace::new("api", "node:20", Arch::Amd64);
        w.cpus = Some(4);
        w.memory_mb = Some(2048);
        w.docker_sock = false;
        w.scrollback = Some(5000);
        w.shell = Some("/bin/zsh".into());
        w.storage = Some(std::path::PathBuf::from("/data/api"));
        w.env = vec![("FOO".into(), "bar".into())];
        w.mounts = vec![Mount { host: "/h".into(), container: "/c".into(), ro: true }];
        w
    }

    // REGRESSION: re-creating an existing workspace (e.g. to change its image or toggle a flag) must NOT
    // wipe the fields the CLI can't set. The old `create` rebuilt from `Workspace::new`, silently dropping
    // env/mounts/cpus/memory/storage/shell/scrollback and resetting docker_sock back to `true`.
    #[test]
    fn recreate_preserves_non_cli_config() {
        let prior = prior_rich();
        // Change only the image; leave arch/vpn/cuda/gui unspecified.
        let got = build_workspace(Some(&prior), "api", "node:22", None, None, None, None).unwrap();
        assert_eq!(got.image, "node:22", "image is updated");
        assert_eq!(got.cpus, Some(4), "cpus preserved");
        assert_eq!(got.memory_mb, Some(2048), "memory preserved");
        assert!(!got.docker_sock, "docker_sock must NOT reset to true");
        assert_eq!(got.scrollback, Some(5000), "scrollback preserved");
        assert_eq!(got.shell.as_deref(), Some("/bin/zsh"), "shell preserved");
        assert_eq!(got.storage, prior.storage, "storage preserved");
        assert_eq!(got.env, prior.env, "env preserved");
        assert_eq!(got.mounts, prior.mounts, "mounts preserved");
    }

    // REGRESSION: `--arch` had a clap `default_value = "arm64"`, so re-creating an amd64 workspace WITHOUT
    // `--arch` silently flipped it to arm64. With `arch: Option`, an omitted `--arch` keeps the prior arch.
    #[test]
    fn recreate_without_arch_keeps_prior_arch() {
        let prior = prior_rich(); // Amd64
        let got = build_workspace(Some(&prior), "api", "node:20", None, None, None, Some("on")).unwrap();
        assert_eq!(got.arch, Arch::Amd64, "omitted --arch must not reset an amd64 workspace to arm64");
        assert!(got.gui, "the flag the user DID pass still applies");
        // Passing --arch explicitly still changes it.
        let got2 = build_workspace(Some(&prior), "api", "node:20", Some("arm64"), None, None, None).unwrap();
        assert_eq!(got2.arch, Arch::Arm64);
    }

    #[test]
    fn fresh_workspace_defaults_arm64() {
        let got = build_workspace(None, "ws", "alpine", None, None, None, None).unwrap();
        assert_eq!(got.arch, Arch::Arm64);
        assert!(got.docker_sock, "fresh workspace defaults docker_sock on");
        assert!(got.vpn.is_none() && got.cuda.is_none() && !got.gui);
    }

    #[test]
    fn vpn_three_way_semantics() {
        let mut prior = Workspace::new("w", "img", Arch::Arm64);
        prior.vpn = Some(VpnConfig::socks5("127.0.0.1:1080"));
        // absent → preserve
        let keep = build_workspace(Some(&prior), "w", "img", None, None, None, None).unwrap();
        assert_eq!(keep.vpn, prior.vpn);
        // off → clear
        let cleared = build_workspace(Some(&prior), "w", "img", None, Some("off"), None, None).unwrap();
        assert!(cleared.vpn.is_none());
        // explicit kind → set
        let set = build_workspace(None, "w", "img", None, Some("socks5:10.0.0.1:9050"), None, None).unwrap();
        assert_eq!(set.vpn.as_ref().unwrap().kind, VpnKind::Socks5);
        assert_eq!(set.vpn.as_ref().unwrap().endpoint, "10.0.0.1:9050");
    }

    #[test]
    fn cuda_three_way_semantics() {
        let mut prior = Workspace::new("w", "img", Arch::Arm64);
        prior.cuda = Some(CudaDevice::default_device());
        let keep = build_workspace(Some(&prior), "w", "img", None, None, None, None).unwrap();
        assert_eq!(keep.cuda, prior.cuda);
        let cleared = build_workspace(Some(&prior), "w", "img", None, None, Some("none"), None).unwrap();
        assert!(cleared.cuda.is_none());
        let dfl = build_workspace(None, "w", "img", None, None, Some("default"), None).unwrap();
        assert_eq!(dfl.cuda, Some(CudaDevice::default_device()));
        let spec = build_workspace(None, "w", "img", None, None, Some("My GPU|7.5|8192"), None).unwrap();
        assert_eq!(spec.cuda.as_ref().unwrap().vram_mb, 8192);
    }

    #[test]
    fn gui_three_way_semantics() {
        let mut prior = Workspace::new("w", "img", Arch::Arm64);
        prior.gui = true;
        assert!(build_workspace(Some(&prior), "w", "img", None, None, None, None).unwrap().gui, "absent preserves on");
        assert!(!build_workspace(Some(&prior), "w", "img", None, None, None, Some("off")).unwrap().gui, "off clears");
        assert!(build_workspace(None, "w", "img", None, None, None, Some("on")).unwrap().gui, "on sets");
        assert!(!build_workspace(None, "w", "img", None, None, None, Some("false")).unwrap().gui, "false clears");
    }

    #[test]
    fn bad_input_is_err_not_panic() {
        assert!(build_workspace(None, "w", "img", Some("sparc"), None, None, None).is_err(), "bad arch");
        // A `--vpn` that VpnConfig::parse rejects. An empty host:port (`socks5:`) has no endpoint.
        assert!(build_workspace(None, "w", "img", None, Some("socks5:"), None, None).is_err(), "bad vpn");
    }
}
