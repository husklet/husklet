//! `hl`-side workspace CONFIG: the bare [`hl_ws::Workspace`] run primitive PLUS its per-workspace FEATURE
//! settings, and the file-backed persistence for the whole thing.
//!
//! `hl-ws` owns only what is needed to RUN a workspace. A feature (vpn egress, a simulated CUDA device, the
//! GUI display toggle, the docker-socket mount, terminal scrollback) is NOT part of that primitive — it is
//! `hl`-side config. This module holds:
//!   - the feature DATA types ([`VpnConfig`]/[`VpnKind`], [`CudaDevice`]) — plain data, no mechanism;
//!   - [`WorkspaceConfig`], the wrapper = a bare `Workspace` + its feature settings (Derefs to `Workspace`
//!     so identity/run fields read through transparently);
//!   - [`WorkspaceStore`], the persistence (`~/.hl/workspaces.conf`) that round-trips the full config.
//!
//! `hl` (the CLI launcher) maps each setting to the owning crate's PRIMITIVE at launch (vpn→engine egress
//! arg, cuda→hl-gpu, gui→compositor socket, docker_sock→mount); this module is pure data + IO only.

use hl_ws::{Arch, Mount, Workspace};
use std::io;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

/// The kind of VPN/proxy a workspace routes its egress through (see [`VpnConfig`] / `docs/VPN.md`). Pure
/// data — the routing MECHANISM is an engine (`hl-jit`) primitive, applied by `hl` at launch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VpnKind {
    /// A SOCKS5 proxy `host:port` (the natively-supported case — becomes the engine's egress-socks argument).
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

/// A per-workspace VPN / proxy egress SETTING. `None` on a [`WorkspaceConfig`] = direct egress (default).
/// When set, `hl` maps it to the engine's egress primitive at launch: for [`VpnKind::Socks5`] the SOCKS5
/// `host:port` is passed straight through; the other kinds name a tunnel/config a userspace helper fronts
/// as SOCKS5 (see `docs/VPN.md`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VpnConfig {
    pub kind: VpnKind,
    /// For `Socks5`/`Http`: a `host:port` proxy endpoint. For `Wireguard`/`Openvpn`: a path to the tunnel config.
    pub endpoint: String,
}

impl VpnConfig {
    /// A SOCKS5 proxy at `host:port`.
    pub fn socks5(endpoint: impl Into<String>) -> VpnConfig {
        VpnConfig {
            kind: VpnKind::Socks5,
            endpoint: endpoint.into(),
        }
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
                return Some(VpnConfig {
                    kind,
                    endpoint: rest.to_string(),
                });
            }
        }
        Some(VpnConfig::socks5(s.to_string()))
    }
    /// The canonical `<kind>:<endpoint>` persisted form (round-trips through [`VpnConfig::parse`]).
    pub fn to_spec(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.endpoint)
    }
    /// The SOCKS5 `host:port` the engine's egress redirect should dial, if this config resolves to one
    /// directly. `Socks5` endpoints qualify as-is; the tunnel kinds return `None` (they need a helper).
    pub fn socks_endpoint(&self) -> Option<&str> {
        match self.kind {
            VpnKind::Socks5 => Some(self.endpoint.as_str()),
            _ => None,
        }
    }
}

/// A per-workspace **simulated CUDA device** SETTING. `None` on a [`WorkspaceConfig`] = no GPU device
/// presented. These fields are exactly what NVML / `cudaGetDeviceProperties` report — *presentation* data,
/// not real hardware. The injection MECHANISM (NVML shim, `nvidia-smi`, command-forward to Metal) is an
/// `hl-gpu` primitive that `hl` drives from these fields at launch (see `docs/ideas/CUDA_ON_METAL.md`).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct CudaDevice {
    /// Reported device name, e.g. `"hl Metal (CUDA-sim) Device"`.
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
            name: "hl Metal (CUDA-sim) Device".to_string(),
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

/// The full `hl`-side per-workspace config: the bare [`Workspace`] run primitive PLUS the feature settings
/// `hl` maps to engine primitives at launch. Derefs to the inner `Workspace`, so identity/run fields
/// (`name`, `image`, `arch`, `env`, `mounts`, `storage_dir(..)`, `checkpoint_*`, …) read through directly;
/// the feature settings are the wrapper's own fields.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceConfig {
    /// The bare run primitive.
    pub ws: Workspace,
    /// Mount the docker socket + set `DOCKER_HOST` so `docker` works inside (default on).
    pub docker_sock: bool,
    /// GUI display: bind the host compositor socket into the guest so a Linux GUI app renders on the Mac
    /// (default OFF). The render MECHANISM is an `hl-gpu`/engine primitive `hl` arms when this is set.
    pub gui: bool,
    /// Terminal scrollback (lines of history each shell retains). `None` = unlimited (the default). A
    /// TERMINAL knob (see `hl-ws-term`'s `TermConfig::scrollback`) persisted per-workspace here.
    pub scrollback: Option<u64>,
    /// Per-workspace VPN/proxy egress setting. `None` = direct (default).
    pub vpn: Option<VpnConfig>,
    /// Per-workspace simulated CUDA device setting. `None` = no GPU device presented (default).
    pub cuda: Option<CudaDevice>,
}

impl Deref for WorkspaceConfig {
    type Target = Workspace;
    fn deref(&self) -> &Workspace {
        &self.ws
    }
}
impl DerefMut for WorkspaceConfig {
    fn deref_mut(&mut self) -> &mut Workspace {
        &mut self.ws
    }
}

impl WorkspaceConfig {
    /// A fresh config: a bare `Workspace` with the safe feature defaults (docker socket on; everything else off).
    pub fn new(name: impl Into<String>, image: impl Into<String>, arch: Arch) -> WorkspaceConfig {
        WorkspaceConfig::from_ws(Workspace::new(name, image, arch))
    }
    /// Wrap an existing bare `Workspace` with the default feature settings.
    pub fn from_ws(ws: Workspace) -> WorkspaceConfig {
        WorkspaceConfig {
            ws,
            docker_sock: true,
            gui: false,
            scrollback: None,
            vpn: None,
            cuda: None,
        }
    }
    /// VTE scrollback-line count to apply. Unlimited (`None`/`0`) maps to a very large, file-backed cap
    /// (same convention as `hl_ws_term::TermConfig::scrollback_lines`).
    pub fn scrollback_lines(&self) -> i64 {
        match self.scrollback {
            None | Some(0) => 10_000_000,
            Some(n) => n as i64,
        }
    }
}

/// A file-backed set of workspace configs (`~/.hl/workspaces.conf`), in a tiny dependency-free block format
/// (`[workspace]` + `key = value` lines, repeatable `env`/`mount`) — no serde/toml. Still reads the legacy
/// one-line `name<TAB>arch<TAB>image` rows so old config keeps working. Persists the full [`WorkspaceConfig`]
/// (bare workspace + feature settings), byte-compatible with the format the old `hl-ws` store wrote.
pub struct WorkspaceStore {
    path: PathBuf,
    items: Vec<WorkspaceConfig>,
}

impl WorkspaceStore {
    /// Load the store at `path` (absent/empty → empty store; malformed entries skipped).
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
                    let mut f = line.splitn(3, '\t');
                    if let (Some(name), Some(arch), Some(image)) = (f.next(), f.next(), f.next()) {
                        if let Some(arch) = Arch::parse(arch) {
                            items.push(WorkspaceConfig::new(name, image, arch));
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

    pub fn all(&self) -> &[WorkspaceConfig] {
        &self.items
    }
    pub fn get(&self, name: &str) -> Option<&WorkspaceConfig> {
        self.items.iter().find(|w| w.name == name)
    }

    /// Add or replace a workspace by name, then persist.
    pub fn upsert(&mut self, ws: WorkspaceConfig) -> io::Result<()> {
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
        let mut out = String::from("# hl workspaces\n");
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
            kv(
                &mut out,
                "docker_sock",
                if w.docker_sock { "true" } else { "false" },
            );
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
                kv(
                    &mut out,
                    "mount",
                    &format!(
                        "{}:{}:{}",
                        clean(&m.host),
                        clean(&m.container),
                        if m.ro { "ro" } else { "rw" }
                    ),
                );
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
                    self.env
                        .push((ek.trim().to_string(), ev.trim().to_string()));
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

    fn build(self) -> Option<WorkspaceConfig> {
        let (name, image, arch) = (self.name?, self.image?, self.arch?);
        Some(WorkspaceConfig {
            ws: Workspace {
                name,
                image,
                arch,
                storage: self.storage,
                shell: self.shell,
                cpus: self.cpus,
                memory_mb: self.memory_mb,
                env: self.env,
                mounts: self.mounts,
            },
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

fn clean(s: &str) -> String {
    s.chars()
        .filter(|&c| c != '\t' && c != '\n' && c != '\r')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("hl-store-{}-{}.conf", std::process::id(), tag));
        p
    }

    #[test]
    fn vpn_spec_parses_and_roundtrips() {
        let c = VpnConfig::parse("127.30.0.1:1080").unwrap();
        assert_eq!(c, VpnConfig::socks5("127.30.0.1:1080"));
        assert_eq!(c.socks_endpoint(), Some("127.30.0.1:1080"));
        assert_eq!(
            VpnConfig::parse("socks5:127.0.0.1:9050").unwrap().kind,
            VpnKind::Socks5
        );
        let wg = VpnConfig::parse("wireguard:vpn/wg.conf").unwrap();
        assert_eq!(
            (wg.kind, wg.endpoint.as_str()),
            (VpnKind::Wireguard, "vpn/wg.conf")
        );
        assert_eq!(wg.socks_endpoint(), None);
        assert_eq!(VpnConfig::parse(&wg.to_spec()).unwrap(), wg);
        assert_eq!(VpnConfig::parse(""), None);
        assert_eq!(VpnConfig::parse("   "), None);
    }

    #[test]
    fn cuda_device_spec_parses_and_roundtrips() {
        let d = CudaDevice::default_device();
        assert_eq!(d.compute_capability, "8.6");
        assert_eq!(CudaDevice::parse(&d.to_spec()).unwrap(), d);
        let full = CudaDevice::parse("My GPU|7.5|8192").unwrap();
        assert_eq!(
            (
                full.name.as_str(),
                full.compute_capability.as_str(),
                full.vram_mb
            ),
            ("My GPU", "7.5", 8192)
        );
        let bare = CudaDevice::parse("JustAName").unwrap();
        assert_eq!(
            (
                bare.name.as_str(),
                bare.compute_capability.as_str(),
                bare.vram_mb
            ),
            ("JustAName", "8.6", 4096)
        );
        assert_eq!(CudaDevice::parse(""), None);
    }

    #[test]
    fn store_persists_across_reload() {
        let path = tmp_path("persist");
        let _ = std::fs::remove_file(&path);
        let mut store = WorkspaceStore::load(&path);
        assert!(store.all().is_empty());
        store
            .upsert(WorkspaceConfig::new(
                "ubuntu-dev",
                "ubuntu:24.04",
                Arch::Arm64,
            ))
            .unwrap();
        store
            .upsert(WorkspaceConfig::new("legacy", "centos:7", Arch::Amd64))
            .unwrap();
        store
            .upsert(WorkspaceConfig::new(
                "ubuntu-dev",
                "ubuntu:22.04",
                Arch::Arm64,
            ))
            .unwrap();

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
        let mut cfg = WorkspaceConfig::new("api", "node:20", Arch::Amd64);
        cfg.storage = Some(PathBuf::from("/data/api"));
        cfg.shell = Some("/bin/zsh".into());
        cfg.cpus = Some(4);
        cfg.memory_mb = Some(2048);
        cfg.docker_sock = false;
        cfg.gui = true;
        cfg.scrollback = Some(5000);
        cfg.ws.env = vec![("FOO".into(), "bar=baz".into()), ("N".into(), "1".into())];
        cfg.ws.mounts = vec![Mount {
            host: "/h".into(),
            container: "/c".into(),
            ro: true,
        }];
        cfg.vpn = Some(VpnConfig::socks5("127.30.0.1:1080"));
        cfg.cuda = Some(CudaDevice::parse("hl Metal (CUDA-sim) Device|8.6|16384").unwrap());
        let mut store = WorkspaceStore::load(&path);
        store.upsert(cfg.clone()).unwrap();

        let got = WorkspaceStore::load(&path).get("api").cloned().unwrap();
        assert_eq!(
            got, cfg,
            "rich workspace config should round-trip through the block format"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_tab_format_still_loads() {
        let path = tmp_path("legacy-tab");
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
        store
            .upsert(WorkspaceConfig::new("a", "alpine", Arch::Arm64))
            .unwrap();
        assert!(store.remove("a").unwrap());
        assert!(!store.remove("a").unwrap());
        assert!(WorkspaceStore::load(&path).all().is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
