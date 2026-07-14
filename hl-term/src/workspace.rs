//! Workspaces: a named, persistent dev environment = a specific image (distro) + architecture, launched
//! as a terminal. This is the model + on-disk persistence + the launch seam.
//!
//! The MODEL and persistence are pure (no container engine), so they build + test on any host. The
//! actual container launch is a [`Launcher`] trait: on macOS the GPU shell provides a dd-jit launcher
//! that starts a shell inside the image's container (with a persistent writable upper); here + in tests
//! a [`LocalShellLauncher`] runs a host shell, so the whole "configure a workspace → get a working
//! terminal" flow is exercised headlessly.

use hl_ws_term::pty::local::LocalPty;
use hl_ws_term::pty::PtyBackend;
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
    /// A proper, human-readable `os/arch` display label — the full platform name (not the terse config
    /// token): `linux/aarch64`, `linux/x86_64`, `darwin/aarch64`. The OS is derived from the workspace
    /// kind (darwin vs linux) and the arch from the variant. Used in the UI so a workspace's platform
    /// reads clearly as a table column instead of a bare `arm`/`amd`.
    pub fn os_arch_label(self) -> &'static str {
        match self {
            Arch::Arm64 => "linux/aarch64",
            Arch::Amd64 => "linux/x86_64",
            Arch::DarwinArm64 => "darwin/aarch64",
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

/// The kind of VPN/proxy a workspace routes its egress through (see [`VpnConfig`] / `docs/VPN.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VpnKind {
    /// A SOCKS5 proxy `host:port` (the natively-supported, engine-wired case — becomes `DD_EGRESS_SOCKS`).
    Socks5,
    /// An HTTP CONNECT proxy `host:port` (modeled + persisted; needs a helper to front it as SOCKS5).
    Http,
    /// A WireGuard `wg-quick` config path (needs the userspace-WG + SOCKS helper, per `docs/VPN.md` §4.1).
    Wireguard,
    /// An OpenVPN config path (needs the userspace-OpenVPN + tun2socks helper).
    Openvpn,
}

impl VpnKind {
    /// The stable on-disk / config token.
    pub fn as_str(self) -> &'static str {
        match self {
            VpnKind::Socks5 => "socks5",
            VpnKind::Http => "http",
            VpnKind::Wireguard => "wireguard",
            VpnKind::Openvpn => "openvpn",
        }
    }
    /// Parse the config token (accepts common aliases). `None` = not a recognized kind.
    pub fn parse(s: &str) -> Option<VpnKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "socks5" | "socks" | "socks5h" => Some(VpnKind::Socks5),
            "http" | "https" | "proxy" => Some(VpnKind::Http),
            "wireguard" | "wg" => Some(VpnKind::Wireguard),
            "openvpn" | "ovpn" => Some(VpnKind::Openvpn),
            _ => None,
        }
    }
}

/// A per-workspace VPN / proxy egress configuration. `None` on a [`Workspace`] = direct egress (no VPN),
/// which is the default and keeps normal networking untouched. When set, the workspace's outbound TCP is
/// funneled through `endpoint` (see `docs/VPN.md`): for [`VpnKind::Socks5`] the launcher arms the engine's
/// `DD_EGRESS_SOCKS` redirect directly; the other kinds name a tunnel/config that a userspace helper fronts
/// as a SOCKS5 proxy.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VpnConfig {
    pub kind: VpnKind,
    /// For `Socks5`/`Http`: a `host:port` proxy endpoint. For `Wireguard`/`Openvpn`: a path to the tunnel config.
    pub endpoint: String,
}

impl VpnConfig {
    /// A SOCKS5 proxy at `host:port`.
    pub fn socks5(endpoint: impl Into<String>) -> VpnConfig {
        VpnConfig { kind: VpnKind::Socks5, endpoint: endpoint.into() }
    }
    /// Parse a user/CLI spec: `<kind>:<endpoint>` (e.g. `socks5:127.30.0.1:1080`, `wireguard:vpn/wg.conf`),
    /// or a bare `host:port` which defaults to a SOCKS5 proxy. Empty → `None`.
    pub fn parse(s: &str) -> Option<VpnConfig> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let Some((k, rest)) = s.split_once(':') {
            if let Some(kind) = VpnKind::parse(k) {
                let rest = rest.trim();
                if rest.is_empty() {
                    return None;
                }
                return Some(VpnConfig { kind, endpoint: rest.to_string() });
            }
        }
        // No recognized kind prefix → treat the whole thing as a SOCKS5 `host:port`.
        Some(VpnConfig::socks5(s.to_string()))
    }
    /// The canonical `<kind>:<endpoint>` persisted form (round-trips through [`VpnConfig::parse`]).
    pub fn to_spec(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.endpoint)
    }
    /// The SOCKS5 `host:port` the engine's `DD_EGRESS_SOCKS` redirect should dial, if this config resolves
    /// to one directly. `Socks5` endpoints qualify as-is; the tunnel kinds return `None` (they need a helper
    /// to spawn a SOCKS front-end first, per `docs/VPN.md` §4.1).
    pub fn socks_endpoint(&self) -> Option<&str> {
        match self.kind {
            VpnKind::Socks5 => Some(self.endpoint.as_str()),
            _ => None,
        }
    }
}

/// A per-workspace **simulated CUDA device**. `None` on a [`Workspace`] = no GPU device is presented
/// (the default). When set, the launcher injects dd's CUDA driver shims into the guest so the container
/// sees an NVIDIA-looking device (`nvidia-smi`, `torch.cuda.is_available()`), while the actual GPU work
/// is command-forwarded to the host Apple **Metal** GPU (see `docs/ideas/CUDA_ON_METAL.md`). The fields
/// are exactly what NVML / `cudaGetDeviceProperties` report; they are *presentation*, not real hardware.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CudaDevice {
    /// Reported device name, e.g. `"dd Metal (CUDA-sim) Device"`.
    pub name: String,
    /// Reported compute capability as `"major.minor"`, e.g. `"8.6"`.
    pub compute_capability: String,
    /// Reported VRAM in MB (on Apple Silicon this is carved from unified memory).
    pub vram_mb: u32,
}

impl CudaDevice {
    /// A sensible default simulated device (Ampere-class, 4 GiB reported).
    pub fn default_device() -> CudaDevice {
        CudaDevice {
            name: "dd Metal (CUDA-sim) Device".to_string(),
            compute_capability: "8.6".to_string(),
            vram_mb: 4096,
        }
    }
    /// Parse the persisted `name | cc | vram_mb` spec (pipe-separated so the name may contain spaces).
    /// A bare non-empty string with no pipes is treated as just the device name with default cc/VRAM.
    /// Empty → `None`.
    pub fn parse(s: &str) -> Option<CudaDevice> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let mut d = CudaDevice::default_device();
        let parts: Vec<&str> = s.split('|').collect();
        if let Some(n) = parts.first() {
            let n = n.trim();
            if !n.is_empty() {
                d.name = n.to_string();
            }
        }
        if let Some(cc) = parts.get(1) {
            let cc = cc.trim();
            if !cc.is_empty() {
                d.compute_capability = cc.to_string();
            }
        }
        if let Some(v) = parts.get(2) {
            if let Ok(mb) = v.trim().parse::<u32>() {
                d.vram_mb = mb;
            }
        }
        Some(d)
    }
    /// The canonical persisted form (round-trips through [`CudaDevice::parse`]).
    pub fn to_spec(&self) -> String {
        format!("{}|{}|{}", self.name, self.compute_capability, self.vram_mb)
    }
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
    /// GUI display: bind-mount the host `dd-display` Wayland/DDP socket into the guest and set
    /// `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR` so a Linux GUI app renders on the Mac (default OFF; headless
    /// workspaces are unaffected). Same mount-not-bake pattern as `docker_sock` — see `docs/ideas/RENDERING_PLAN.md`.
    pub gui: bool,
    /// Terminal scrollback (lines of history each shell retains). `None` = unlimited (the default);
    /// `Some(n)` caps it at `n` lines. Persisted so history survives across app restarts.
    pub scrollback: Option<u64>,
    /// Per-workspace VPN/proxy egress. `None` = direct (default, normal networking). `Some(cfg)` routes the
    /// workspace's outbound TCP through the configured proxy/tunnel (see `docs/VPN.md`).
    pub vpn: Option<VpnConfig>,
    /// Per-workspace simulated CUDA device. `None` = no GPU device presented (default). `Some(cfg)`
    /// makes the container see an NVIDIA-looking CUDA device backed by the host Metal GPU (see
    /// `docs/ideas/CUDA_ON_METAL.md`).
    pub cuda: Option<CudaDevice>,
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
            gui: false,
            scrollback: None,
            vpn: None,
            cuda: None,
        }
    }
    /// VTE scrollback-line count to apply. Unlimited (`None`/`0`) maps to a very large, file-backed cap
    /// (VTE streams scrollback to a compressed temp file, so a high cap costs no memory until filled).
    pub fn scrollback_lines(&self) -> i64 {
        match self.scrollback {
            None | Some(0) => 10_000_000,
            Some(n) => n as i64,
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
    /// Per-pane checkpoint dir. Each terminal tab/split-pane is its OWN engine (container init), so a window
    /// with several shells freezes each into its own SLOT and restores them independently — otherwise a
    /// multi-tab window has no single coherent freeze and would lose everything on close.
    pub fn checkpoint_slot_dir(&self, base: &Path, slot: &str) -> PathBuf {
        self.storage_dir(base).join("checkpoint").join(sanitize(slot))
    }
    /// The pid file for a per-pane checkpoint slot (the running init's host pid for that pane's engine).
    pub fn checkpoint_slot_pid_file(&self, base: &Path, slot: &str) -> PathBuf {
        self.storage_dir(base).join("checkpoint").join(format!("{}.pid", sanitize(slot)))
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
            if w.gui {
                kv(&mut out, "gui", "true");
            }
            if let Some(sb) = w.scrollback {
                kv(&mut out, "scrollback", &sb.to_string());
            }
            if let Some(vpn) = &w.vpn {
                kv(&mut out, "vpn", &clean(&vpn.to_spec()));
            }
            if let Some(cuda) = &w.cuda {
                kv(&mut out, "cuda", &clean(&cuda.to_spec()));
            }
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
    gui: Option<bool>,
    scrollback: Option<u64>,
    vpn: Option<VpnConfig>,
    cuda: Option<CudaDevice>,
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
            "gui" => self.gui = Some(matches!(v, "true" | "1" | "yes" | "on")),
            "scrollback" => self.scrollback = v.parse().ok(),
            "vpn" if !v.is_empty() => self.vpn = VpnConfig::parse(v),
            "cuda" if !v.is_empty() => self.cuda = CudaDevice::parse(v),
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
            gui: self.gui.unwrap_or(false),
            scrollback: self.scrollback,
            vpn: self.vpn,
            cuda: self.cuda,
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
    use hl_ws_term::Vt;
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
    fn os_arch_label_is_full_os_slash_arch() {
        assert_eq!(Arch::Arm64.os_arch_label(), "linux/aarch64");
        assert_eq!(Arch::Amd64.os_arch_label(), "linux/x86_64");
        assert_eq!(Arch::DarwinArm64.os_arch_label(), "darwin/aarch64");
        // Every label is a proper `os/arch` (one slash, an os prefix, no bare `arm`/`amd`).
        for a in [Arch::Arm64, Arch::Amd64, Arch::DarwinArm64] {
            let l = a.os_arch_label();
            assert_eq!(l.matches('/').count(), 1, "{l} should be os/arch");
            let (os, arch) = l.split_once('/').unwrap();
            assert!(matches!(os, "linux" | "darwin"), "unexpected os in {l}");
            assert!(!arch.is_empty());
        }
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
        ws.vpn = Some(VpnConfig::socks5("127.30.0.1:1080"));
        let mut store = WorkspaceStore::load(&path);
        store.upsert(ws.clone()).unwrap();

        let got = WorkspaceStore::load(&path).get("api").cloned().unwrap();
        assert_eq!(got, ws, "rich workspace should round-trip through the block format");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn vpn_spec_parses_and_roundtrips() {
        // Bare host:port defaults to SOCKS5.
        let c = VpnConfig::parse("127.30.0.1:1080").unwrap();
        assert_eq!(c, VpnConfig::socks5("127.30.0.1:1080"));
        assert_eq!(c.socks_endpoint(), Some("127.30.0.1:1080"));
        // Explicit kind prefixes.
        assert_eq!(VpnConfig::parse("socks5:127.0.0.1:9050").unwrap().kind, VpnKind::Socks5);
        let wg = VpnConfig::parse("wireguard:vpn/wg.conf").unwrap();
        assert_eq!((wg.kind, wg.endpoint.as_str()), (VpnKind::Wireguard, "vpn/wg.conf"));
        assert_eq!(wg.socks_endpoint(), None, "tunnel kinds need a helper, no direct socks endpoint");
        // Round-trips through the canonical spec form.
        assert_eq!(VpnConfig::parse(&wg.to_spec()).unwrap(), wg);
        // Empty / blank → None.
        assert_eq!(VpnConfig::parse(""), None);
        assert_eq!(VpnConfig::parse("   "), None);

        // Persist a VPN workspace and reload it.
        let path = tmp_path("vpn");
        let _ = std::fs::remove_file(&path);
        let mut ws = Workspace::new("vpnws", "alpine", Arch::Arm64);
        ws.vpn = Some(VpnConfig::socks5("127.31.0.1:1080"));
        let mut store = WorkspaceStore::load(&path);
        store.upsert(ws.clone()).unwrap();
        let got = WorkspaceStore::load(&path).get("vpnws").cloned().unwrap();
        assert_eq!(got.vpn, ws.vpn, "vpn config should round-trip through the block format");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cuda_device_spec_parses_and_roundtrips() {
        // default helper + explicit spec.
        let d = CudaDevice::default_device();
        assert_eq!(d.compute_capability, "8.6");
        assert_eq!(CudaDevice::parse(&d.to_spec()).unwrap(), d, "spec round-trips");
        // full pipe-separated spec (name may contain spaces).
        let full = CudaDevice::parse("My GPU|7.5|8192").unwrap();
        assert_eq!((full.name.as_str(), full.compute_capability.as_str(), full.vram_mb), ("My GPU", "7.5", 8192));
        // bare name → default cc/vram.
        let bare = CudaDevice::parse("JustAName").unwrap();
        assert_eq!((bare.name.as_str(), bare.compute_capability.as_str(), bare.vram_mb), ("JustAName", "8.6", 4096));
        // empty / blank → None.
        assert_eq!(CudaDevice::parse(""), None);
        assert_eq!(CudaDevice::parse("   "), None);

        // Persist a CUDA-enabled workspace and reload it.
        let path = tmp_path("cuda");
        let _ = std::fs::remove_file(&path);
        let mut ws = Workspace::new("cudaws", "nvidia/cuda:12.4.0-base-ubuntu22.04", Arch::Arm64);
        ws.cuda = Some(CudaDevice::parse("dd Metal (CUDA-sim) Device|8.6|16384").unwrap());
        let mut store = WorkspaceStore::load(&path);
        store.upsert(ws.clone()).unwrap();
        let got = WorkspaceStore::load(&path).get("cudaws").cloned().unwrap();
        assert_eq!(got.cuda, ws.cuda, "cuda device should round-trip through the block format");
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
