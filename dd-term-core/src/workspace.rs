//! Workspaces: a named, persistent dev environment = a specific image (distro) + architecture, launched
//! as a terminal. This is the model + on-disk persistence + the launch seam.
//!
//! The MODEL and persistence are pure (no container engine), so they build + test on any host. The
//! actual container launch is a [`Launcher`] trait: on macOS the GPU shell provides a dd-jit launcher
//! that starts a shell inside the image's container (with a persistent writable upper); here + in tests
//! a [`LocalShellLauncher`] runs a host shell, so the whole "configure a workspace → get a working
//! terminal" flow is exercised headlessly.

use crate::pty::local::LocalPty;
use crate::pty::PtyBackend;
use std::io;
use std::path::{Path, PathBuf};

/// Target architecture for a workspace's image. Maps to dd's `Guest` / `--platform` (an x86_64 image
/// runs on an arm64 mac through the jit86 translator).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arch {
    /// Native ARM64 Linux.
    Arm64,
    /// x86-64 Linux (translated to ARM64 by the jit86 engine on Apple silicon).
    Amd64,
    /// Native macOS ARM64 (a darwin rootfs, run in the darwinjail).
    DarwinArm64,
}

impl Arch {
    /// The stable on-disk / config token.
    pub fn as_str(self) -> &'static str {
        match self {
            Arch::Arm64 => "arm64",
            Arch::Amd64 => "amd64",
            Arch::DarwinArm64 => "darwin-arm64",
        }
    }
    /// Parse the config token (also accepts common aliases).
    pub fn parse(s: &str) -> Option<Arch> {
        match s.trim() {
            "arm64" | "aarch64" | "linux/arm64" => Some(Arch::Arm64),
            "amd64" | "x86_64" | "x86-64" | "linux/amd64" => Some(Arch::Amd64),
            "darwin-arm64" | "darwin" | "macos" => Some(Arch::DarwinArm64),
            _ => None,
        }
    }
    /// The Docker `--platform` string dd-images uses to pick which arch to pull, or `None` for darwin.
    pub fn platform(self) -> Option<&'static str> {
        match self {
            Arch::Arm64 => Some("linux/arm64"),
            Arch::Amd64 => Some("linux/amd64"),
            Arch::DarwinArm64 => None,
        }
    }
}

/// A configured workspace: a name, the image it runs, and the target arch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Workspace {
    pub name: String,
    pub image: String,
    pub arch: Arch,
}

impl Workspace {
    pub fn new(name: impl Into<String>, image: impl Into<String>, arch: Arch) -> Workspace {
        Workspace { name: name.into(), image: image.into(), arch }
    }
    /// The persistent writable-upper directory for this workspace under `base` (e.g. `~/.dd`). The dd-jit
    /// launcher passes this to `Image::overlay(upper, [rootfs])` so state survives app restarts.
    pub fn upper_dir(&self, base: &Path) -> PathBuf {
        base.join("workspaces").join(sanitize(&self.name)).join("upper")
    }
    /// The default shell command to run in the workspace.
    pub fn default_shell() -> Vec<String> {
        vec!["/bin/bash".to_string(), "-l".to_string()]
    }
}

/// A file-backed set of workspaces (`~/.dd/workspaces.conf`). Persisted in a tiny tab-separated format
/// (`name<TAB>arch<TAB>image`) rather than serde/toml so the core stays dependency-free.
pub struct WorkspaceStore {
    path: PathBuf,
    items: Vec<Workspace>,
}

impl WorkspaceStore {
    /// Load the store at `path` (an absent/empty file yields an empty store; malformed lines are skipped).
    pub fn load(path: impl Into<PathBuf>) -> WorkspaceStore {
        let path = path.into();
        let mut items = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let mut f = line.splitn(3, '\t');
                if let (Some(name), Some(arch), Some(image)) = (f.next(), f.next(), f.next()) {
                    if let Some(arch) = Arch::parse(arch) {
                        items.push(Workspace::new(name, image, arch));
                    }
                }
            }
        }
        WorkspaceStore { path, items }
    }

    pub fn all(&self) -> &[Workspace] {
        &self.items
    }
    pub fn get(&self, name: &str) -> Option<&Workspace> {
        self.items.iter().find(|w| w.name == name)
    }

    /// Add or replace a workspace by name, then persist.
    pub fn upsert(&mut self, ws: Workspace) -> io::Result<()> {
        self.items.retain(|w| w.name != ws.name);
        self.items.push(ws);
        self.save()
    }

    /// Remove a workspace by name; returns whether one was removed, then persists.
    pub fn remove(&mut self, name: &str) -> io::Result<bool> {
        let before = self.items.len();
        self.items.retain(|w| w.name != name);
        let removed = self.items.len() != before;
        self.save()?;
        Ok(removed)
    }

    fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = String::from("# dd-term workspaces: name<TAB>arch<TAB>image\n");
        for w in &self.items {
            // Names/images with tabs or newlines would corrupt the line format — reject at write time by
            // stripping them (config is written by the app, not hand-edited, so this is belt-and-braces).
            out.push_str(&clean(&w.name));
            out.push('\t');
            out.push_str(w.arch.as_str());
            out.push('\t');
            out.push_str(&clean(&w.image));
            out.push('\n');
        }
        std::fs::write(&self.path, out)
    }
}

/// The launch seam: turn a [`Workspace`] into a live terminal ([`PtyBackend`]).
pub trait Launcher {
    fn launch(&self, ws: &Workspace, cols: u16, rows: u16) -> io::Result<Box<dyn PtyBackend>>;
}

/// A launcher that runs a plain host shell (ignoring the image) — used on hosts without the container
/// engine and in tests, so the workspace→terminal flow is exercisable everywhere. The dd-jit launcher
/// (macOS) is the real one that enters the image's container.
pub struct LocalShellLauncher {
    pub shell: Vec<String>,
}

impl Default for LocalShellLauncher {
    fn default() -> Self {
        let sh = if Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" };
        LocalShellLauncher { shell: vec![sh.to_string()] }
    }
}

impl Launcher for LocalShellLauncher {
    fn launch(&self, ws: &Workspace, cols: u16, rows: u16) -> io::Result<Box<dyn PtyBackend>> {
        let argv: Vec<&str> = self.shell.iter().map(|s| s.as_str()).collect();
        // Surface the workspace identity to the shell (a real launcher would set the container hostname).
        let pty = LocalPty::spawn(
            &argv,
            cols,
            rows,
            &[("TERM", "xterm-256color"), ("DD_WORKSPACE", &ws.name)],
        )?;
        Ok(Box::new(pty))
    }
}

/// Sanitize a workspace name into a filesystem-safe directory component.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

fn clean(s: &str) -> String {
    s.chars().filter(|&c| c != '\t' && c != '\n' && c != '\r').collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Vt;
    use std::time::{Duration, Instant};

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("dd-term-ws-{}-{}.conf", std::process::id(), tag));
        p
    }

    #[test]
    fn arch_parse_and_roundtrip() {
        for a in [Arch::Arm64, Arch::Amd64, Arch::DarwinArm64] {
            assert_eq!(Arch::parse(a.as_str()), Some(a));
        }
        assert_eq!(Arch::parse("aarch64"), Some(Arch::Arm64));
        assert_eq!(Arch::parse("x86_64"), Some(Arch::Amd64));
        assert_eq!(Arch::parse("nonsense"), None);
        assert_eq!(Arch::Amd64.platform(), Some("linux/amd64"));
        assert_eq!(Arch::DarwinArm64.platform(), None);
    }

    #[test]
    fn store_persists_across_reload() {
        let path = tmp_path("persist");
        let _ = std::fs::remove_file(&path);
        let mut store = WorkspaceStore::load(&path);
        assert!(store.all().is_empty());
        store.upsert(Workspace::new("ubuntu-dev", "ubuntu:24.04", Arch::Arm64)).unwrap();
        store.upsert(Workspace::new("legacy", "centos:7", Arch::Amd64)).unwrap();
        // upsert replaces by name.
        store.upsert(Workspace::new("ubuntu-dev", "ubuntu:22.04", Arch::Arm64)).unwrap();

        let reloaded = WorkspaceStore::load(&path);
        assert_eq!(reloaded.all().len(), 2);
        assert_eq!(reloaded.get("ubuntu-dev").unwrap().image, "ubuntu:22.04");
        assert_eq!(reloaded.get("legacy").unwrap().arch, Arch::Amd64);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn store_remove() {
        let path = tmp_path("remove");
        let _ = std::fs::remove_file(&path);
        let mut store = WorkspaceStore::load(&path);
        store.upsert(Workspace::new("a", "alpine", Arch::Arm64)).unwrap();
        assert!(store.remove("a").unwrap());
        assert!(!store.remove("a").unwrap()); // already gone
        assert!(WorkspaceStore::load(&path).all().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn upper_dir_is_named_and_stable() {
        let ws = Workspace::new("My Cool WS!", "ubuntu", Arch::Arm64);
        let dir = ws.upper_dir(Path::new("/home/u/.dd"));
        assert_eq!(dir, Path::new("/home/u/.dd/workspaces/My_Cool_WS_/upper"));
    }

    #[test]
    fn launch_workspace_runs_a_terminal() {
        // The whole configure→launch→terminal flow, headless: a workspace launches a shell we can drive.
        let ws = Workspace::new("demo", "ubuntu:24.04", Arch::Arm64);
        let launcher = LocalShellLauncher::default();
        let mut pty = launcher.launch(&ws, 40, 10).unwrap();
        pty.write(b"echo hello-$DD_WORKSPACE; exit\n").unwrap();

        let mut vt = Vt::new(40, 10);
        let fd = pty.master_fd().unwrap();
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut exited = false;
        loop {
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let pr = unsafe { libc::poll(&mut pfd, 1, 20) };
            if pr > 0 && pfd.revents & libc::POLLIN != 0 {
                let n = pty.read(&mut buf).unwrap_or(0);
                if n > 0 {
                    vt.advance_bytes(&buf[..n]);
                    continue;
                }
            }
            if exited || Instant::now() > deadline {
                break;
            }
            if pty.try_wait().is_some() {
                exited = true;
            }
        }
        let screen: String = (0..10).map(|r| vt.grid().row_text(r)).collect::<Vec<_>>().join("\n");
        assert!(screen.contains("hello-demo"), "workspace shell should have run the command; got:\n{screen}");
    }
}
