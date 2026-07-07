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

/// A host→guest bind mount for a workspace.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Mount {
    pub host: String,
    pub container: String,
    pub ro: bool,
}

/// A configured workspace: identity (name + image + arch) plus its isolated-environment config —
/// where it lives on disk, the shell, resource caps, environment, bind mounts, and whether the docker
/// socket is mounted so the normal `docker` CLI works inside it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Workspace {
    pub name: String,
    pub image: String,
    pub arch: Arch,
    /// Where the workspace's persistent state lives; `None` = the default `<base>/workspaces/<name>`.
    pub storage: Option<PathBuf>,
    /// Login shell command line, e.g. `"/bin/bash -l"`; `None` = auto (bash if present, else sh).
    pub shell: Option<String>,
    /// CPU cap (cores) and memory cap (MB); `None` = unbounded.
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    /// Environment variables injected into every shell in the workspace.
    pub env: Vec<(String, String)>,
    /// Host→guest bind mounts.
    pub mounts: Vec<Mount>,
    /// Mount the docker socket + set `DOCKER_HOST` so `docker` works inside (default on).
    pub docker_sock: bool,
}

impl Workspace {
    pub fn new(name: impl Into<String>, image: impl Into<String>, arch: Arch) -> Workspace {
        Workspace {
            name: name.into(),
            image: image.into(),
            arch,
            storage: None,
            shell: None,
            cpus: None,
            memory_mb: None,
            env: Vec::new(),
            mounts: Vec::new(),
            docker_sock: true,
        }
    }
    /// The on-disk directory holding this workspace's persistent state (honors a configured `storage`).
    pub fn storage_dir(&self, base: &Path) -> PathBuf {
        self.storage
            .clone()
            .unwrap_or_else(|| base.join("workspaces").join(sanitize(&self.name)))
    }
    /// The persistent writable-upper directory. The dd-jit launcher passes this to
    /// `Image::overlay(upper, [rootfs])` so state survives app restarts.
    pub fn upper_dir(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("upper")
    }
    /// Where a whole-workspace checkpoint (the frozen process tree: shells + jobs + children) is written on
    /// close and read back on reopen. The dd-jit launcher arms the engine with this dir.
    pub fn checkpoint_dir(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("checkpoint")
    }
    /// The file recording the running container init's host pid, so `workspace checkpoint` can signal the
    /// live tree from a separate process.
    pub fn checkpoint_pid_file(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("checkpoint.pid")
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
    /// Load the store at `path` (absent/empty → empty store; malformed entries skipped). Parses the
    /// block format (`[workspace]` + `key = value` lines, repeatable `env`/`mount`) and still reads the
    /// legacy one-line `name<TAB>arch<TAB>image` rows so old config keeps working.
    pub fn load(path: impl Into<PathBuf>) -> WorkspaceStore {
        let path = path.into();
        let mut items = Vec::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut cur: Option<WsBuilder> = None;
            for raw in text.lines() {
                let line = raw.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if line == "[workspace]" {
                    if let Some(w) = cur.take().and_then(|b| b.build()) {
                        items.push(w);
                    }
                    cur = Some(WsBuilder::default());
                    continue;
                }
                if cur.is_none() && line.contains('\t') {
                    // Legacy row.
                    let mut f = line.splitn(3, '\t');
                    if let (Some(name), Some(arch), Some(image)) = (f.next(), f.next(), f.next()) {
                        if let Some(arch) = Arch::parse(arch) {
                            items.push(Workspace::new(name, image, arch));
                        }
                    }
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    if let Some(b) = cur.as_mut() {
                        b.set(k.trim(), v.trim());
                    }
                }
            }
            if let Some(w) = cur.take().and_then(|b| b.build()) {
                items.push(w);
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
        let mut out = String::from("# dd workspaces\n");
        for w in &self.items {
            out.push_str("\n[workspace]\n");
            kv(&mut out, "name", &clean(&w.name));
            kv(&mut out, "image", &clean(&w.image));
            kv(&mut out, "arch", w.arch.as_str());
            if let Some(s) = &w.storage {
                kv(&mut out, "storage", &clean(&s.to_string_lossy()));
            }
            if let Some(s) = &w.shell {
                kv(&mut out, "shell", &clean(s));
            }
            if let Some(c) = w.cpus {
                kv(&mut out, "cpus", &c.to_string());
            }
            if let Some(m) = w.memory_mb {
                kv(&mut out, "memory", &m.to_string());
            }
            kv(&mut out, "docker_sock", if w.docker_sock { "true" } else { "false" });
            for (k, v) in &w.env {
                kv(&mut out, "env", &format!("{}={}", clean(k), clean(v)));
            }
            for m in &w.mounts {
                kv(&mut out, "mount", &format!("{}:{}:{}", clean(&m.host), clean(&m.container), if m.ro { "ro" } else { "rw" }));
            }
        }
        std::fs::write(&self.path, out)
    }
}

/// Accumulates a workspace's `key = value` lines within a `[workspace]` block, then builds it.
#[derive(Default)]
struct WsBuilder {
    name: Option<String>,
    image: Option<String>,
    arch: Option<Arch>,
    storage: Option<PathBuf>,
    shell: Option<String>,
    cpus: Option<u32>,
    memory_mb: Option<u32>,
    env: Vec<(String, String)>,
    mounts: Vec<Mount>,
    docker_sock: Option<bool>,
}

impl WsBuilder {
    fn set(&mut self, k: &str, v: &str) {
        match k {
            "name" => self.name = Some(v.to_string()),
            "image" => self.image = Some(v.to_string()),
            "arch" => self.arch = Arch::parse(v),
            "storage" if !v.is_empty() => self.storage = Some(PathBuf::from(v)),
            "shell" if !v.is_empty() => self.shell = Some(v.to_string()),
            "cpus" => self.cpus = v.parse().ok(),
            "memory" => self.memory_mb = v.parse().ok(),
            "docker_sock" => self.docker_sock = Some(matches!(v, "true" | "1" | "yes" | "on")),
            "env" => {
                if let Some((ek, ev)) = v.split_once('=') {
                    self.env.push((ek.trim().to_string(), ev.trim().to_string()));
                }
            }
            "mount" => {
                let p: Vec<&str> = v.split(':').collect();
                if p.len() >= 2 && !p[0].is_empty() && !p[1].is_empty() {
                    self.mounts.push(Mount {
                        host: p[0].to_string(),
                        container: p[1].to_string(),
                        ro: p.get(2).map(|s| *s == "ro").unwrap_or(false),
                    });
                }
            }
            _ => {}
        }
    }

    fn build(self) -> Option<Workspace> {
        let (name, image, arch) = (self.name?, self.image?, self.arch?);
        Some(Workspace {
            name,
            image,
            arch,
            storage: self.storage,
            shell: self.shell,
            cpus: self.cpus,
            memory_mb: self.memory_mb,
            env: self.env,
            mounts: self.mounts,
            docker_sock: self.docker_sock.unwrap_or(true),
        })
    }
}

fn kv(out: &mut String, k: &str, v: &str) {
    out.push_str(k);
    out.push_str(" = ");
    out.push_str(v);
    out.push('\n');
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
    fn rich_config_roundtrips() {
        let path = tmp_path("rich");
        let _ = std::fs::remove_file(&path);
        let mut ws = Workspace::new("api", "node:20", Arch::Amd64);
        ws.storage = Some(PathBuf::from("/data/api"));
        ws.shell = Some("/bin/zsh".into());
        ws.cpus = Some(4);
        ws.memory_mb = Some(2048);
        ws.docker_sock = false;
        ws.env = vec![("FOO".into(), "bar=baz".into()), ("N".into(), "1".into())];
        ws.mounts = vec![Mount { host: "/h".into(), container: "/c".into(), ro: true }];
        let mut store = WorkspaceStore::load(&path);
        store.upsert(ws.clone()).unwrap();

        let got = WorkspaceStore::load(&path).get("api").cloned().unwrap();
        assert_eq!(got, ws, "rich workspace should round-trip through the block format");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_tab_format_still_loads() {
        let path = tmp_path("legacy");
        std::fs::write(&path, "# old\nubuntu-dev\tarm64\tubuntu:24.04\n").unwrap();
        let store = WorkspaceStore::load(&path);
        let w = store.get("ubuntu-dev").unwrap();
        assert_eq!(w.image, "ubuntu:24.04");
        assert_eq!(w.arch, Arch::Arm64);
        assert!(w.docker_sock, "legacy rows default docker_sock on");
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
