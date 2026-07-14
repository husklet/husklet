//! The workspace MODEL + its settings TYPES — pure data, std-only.
//!
//! A workspace is a named, persistent dev environment = a specific image (distro) + architecture, launched
//! as a terminal. Per-workspace settings that a launch acts on — [`VpnConfig`] (egress), [`CudaDevice`]
//! (simulated GPU), the `gui` display toggle — are plain DATA types here. `hl-ws` owns only the data; the
//! actual *mechanism* for each is a primitive in the crate that owns it (the engine `hl-jit` for egress,
//! `hl-gpu` for the CUDA device injection), and `hl` maps the setting to that primitive at launch.

use std::path::{Path, PathBuf};

/// Target architecture for a workspace's image. Maps to the engine's `Guest` / `--platform` (an x86_64
/// image runs on an arm64 mac through the jit86 translator).
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
    /// A proper, human-readable `os/arch` display label: `linux/aarch64`, `linux/x86_64`, `darwin/aarch64`.
    pub fn os_arch_label(self) -> &'static str {
        match self {
            Arch::Arm64 => "linux/aarch64",
            Arch::Amd64 => "linux/x86_64",
            Arch::DarwinArm64 => "darwin/aarch64",
        }
    }
    /// The Docker `--platform` string the image resolver uses to pick which arch to pull, or `None` for darwin.
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

/// A per-workspace VPN / proxy egress SETTING. `None` on a [`Workspace`] = direct egress (default). When
/// set, `hl` maps it to the engine's egress primitive at launch: for [`VpnKind::Socks5`] the SOCKS5
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

/// A per-workspace **simulated CUDA device** SETTING. `None` on a [`Workspace`] = no GPU device presented.
/// These fields are exactly what NVML / `cudaGetDeviceProperties` report — *presentation* data, not real
/// hardware. The injection MECHANISM (NVML shim, `nvidia-smi`, command-forward to Metal) is an `hl-gpu`
/// primitive that `hl` drives from these fields at launch (see `docs/ideas/CUDA_ON_METAL.md`).
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

/// A configured workspace: identity (name + image + arch) plus its isolated-environment config and its
/// per-workspace settings (egress, GPU, display). All fields are plain data; `hl` maps the launch-affecting
/// settings to the owning crate's primitive at launch time.
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
    /// GUI display: bind the host compositor socket into the guest so a Linux GUI app renders on the Mac
    /// (default OFF). The render MECHANISM is an `hl-gpu`/engine primitive `hl` arms when this is set.
    pub gui: bool,
    /// Terminal scrollback (lines of history each shell retains). `None` = unlimited (the default).
    pub scrollback: Option<u64>,
    /// Per-workspace VPN/proxy egress setting. `None` = direct (default).
    pub vpn: Option<VpnConfig>,
    /// Per-workspace simulated CUDA device setting. `None` = no GPU device presented (default).
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
    /// VTE scrollback-line count to apply. Unlimited (`None`/`0`) maps to a very large, file-backed cap.
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
    /// The persistent writable-upper directory, passed to `Image::overlay(upper, [rootfs])`.
    pub fn upper_dir(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("upper")
    }
    /// Where a whole-workspace checkpoint (the frozen process tree) is written on close / read on reopen.
    pub fn checkpoint_dir(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("checkpoint")
    }
    /// The file recording the running container init's host pid.
    pub fn checkpoint_pid_file(&self, base: &Path) -> PathBuf {
        self.storage_dir(base).join("checkpoint.pid")
    }
    /// Per-pane checkpoint dir (each tab/split-pane is its own engine, frozen into its own slot).
    pub fn checkpoint_slot_dir(&self, base: &Path, slot: &str) -> PathBuf {
        self.storage_dir(base).join("checkpoint").join(sanitize(slot))
    }
    /// The pid file for a per-pane checkpoint slot.
    pub fn checkpoint_slot_pid_file(&self, base: &Path, slot: &str) -> PathBuf {
        self.storage_dir(base).join("checkpoint").join(format!("{}.pid", sanitize(slot)))
    }
    /// The default shell command to run in the workspace.
    pub fn default_shell() -> Vec<String> {
        vec!["/bin/bash".to_string(), "-l".to_string()]
    }
}

/// Sanitize a workspace name into a filesystem-safe directory component.
pub(crate) fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    #[test]
    fn vpn_spec_parses_and_roundtrips() {
        let c = VpnConfig::parse("127.30.0.1:1080").unwrap();
        assert_eq!(c, VpnConfig::socks5("127.30.0.1:1080"));
        assert_eq!(c.socks_endpoint(), Some("127.30.0.1:1080"));
        assert_eq!(VpnConfig::parse("socks5:127.0.0.1:9050").unwrap().kind, VpnKind::Socks5);
        let wg = VpnConfig::parse("wireguard:vpn/wg.conf").unwrap();
        assert_eq!((wg.kind, wg.endpoint.as_str()), (VpnKind::Wireguard, "vpn/wg.conf"));
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
        assert_eq!((full.name.as_str(), full.compute_capability.as_str(), full.vram_mb), ("My GPU", "7.5", 8192));
        let bare = CudaDevice::parse("JustAName").unwrap();
        assert_eq!((bare.name.as_str(), bare.compute_capability.as_str(), bare.vram_mb), ("JustAName", "8.6", 4096));
        assert_eq!(CudaDevice::parse(""), None);
    }

    #[test]
    fn upper_dir_is_named_and_stable() {
        let ws = Workspace::new("My Cool WS!", "ubuntu", Arch::Arm64);
        let dir = ws.upper_dir(Path::new("/home/u/.dd"));
        assert_eq!(dir, Path::new("/home/u/.dd/workspaces/My_Cool_WS_/upper"));
    }
}
