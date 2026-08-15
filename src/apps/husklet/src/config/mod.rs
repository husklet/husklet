//! `hl`-side workspace CONFIG: the bare [`hl_ws::Workspace`] run primitive PLUS its per-workspace FEATURE
//! settings, and their file-backed persistence.
//!
//! `hl-ws` owns only what is needed to run a workspace. Features such as VPN egress, the Docker-socket
//! mount, and terminal scrollback are application-side configuration rather than part of that primitive.
//! This module holds:
//!   - the feature data types ([`VpnConfig`]/[`VpnKind`]) — plain data, no mechanism;
//!   - [`WorkspaceConfig`], the wrapper = a bare `Workspace` + its feature settings (Derefs to `Workspace`
//!     so identity/run fields read through transparently);
//!   - [`WorkspaceStore`], the persistence (`~/.hl/workspaces.conf`) that round-trips the full config.
//!
//! `hl` (the CLI launcher) maps each setting to the owning crate's PRIMITIVE at launch (vpn→engine egress
//! arg and `docker_sock→mount`); this module is pure data + IO only.

use hl_ws::{Arch, Mount, Workspace};
use std::io;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;

mod document;

use document::{WorkspaceDocument, WorkspaceText};

#[cfg(feature = "gui")]
use hl_ws_term::config::{CursorShape, TermConfig};

/// The kind of VPN/proxy a workspace routes its egress through. Pure
/// data; the container domain applies the routing mechanism at launch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VpnKind {
    /// A SOCKS5 proxy `host:port` (the natively-supported case — becomes the engine's egress-socks argument).
    Socks5,
    /// An HTTP CONNECT proxy `host:port` (modeled + persisted; needs a helper to front it as SOCKS5).
    Http,
    /// A `WireGuard` `wg-quick` config path.
    Wireguard,
    /// An `OpenVPN` config path (needs the userspace-OpenVPN + tun2socks helper).
    Openvpn,
}

impl VpnKind {
    /// The stable on-disk / config token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            VpnKind::Socks5 => "socks5",
            VpnKind::Http => "http",
            VpnKind::Wireguard => "wireguard",
            VpnKind::Openvpn => "openvpn",
        }
    }
    /// Parse the config token (accepts common aliases). `None` = not a recognized kind.
    #[must_use]
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
/// as SOCKS5.
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
    #[must_use]
    pub fn parse(s: &str) -> Option<VpnConfig> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        let Some((kind, endpoint)) = s
            .split_once(':')
            .and_then(|(kind, endpoint)| VpnKind::parse(kind).map(|kind| (kind, endpoint.trim())))
        else {
            return Self::valid_proxy_endpoint(s).then(|| VpnConfig::socks5(s));
        };
        if endpoint.is_empty() {
            return None;
        }
        if matches!(kind, VpnKind::Socks5 | VpnKind::Http) && !Self::valid_proxy_endpoint(endpoint) {
            return None;
        }
        Some(VpnConfig {
            kind,
            endpoint: endpoint.to_owned(),
        })
    }

    fn valid_proxy_endpoint(endpoint: &str) -> bool {
        let Some((host, port)) = endpoint.rsplit_once(':') else {
            return false;
        };
        let host = host.trim();
        let port = port.trim();
        let bracketed = host.starts_with('[') && host.ends_with(']') && host.len() > 2;
        let plain = !host.contains(['[', ']', ':']);
        !host.is_empty()
            && !host.bytes().any(|byte| byte.is_ascii_whitespace())
            && (plain || bracketed)
            && !port.is_empty()
            && port.parse::<u16>().is_ok_and(|port| port > 0)
    }
    /// The canonical `<kind>:<endpoint>` persisted form (round-trips through [`VpnConfig::parse`]).
    #[must_use]
    pub fn to_spec(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.endpoint)
    }
    /// The SOCKS5 `host:port` the engine's egress redirect should dial, if this config resolves to one
    /// directly. `Socks5` endpoints qualify as-is; the tunnel kinds return `None` (they need a helper).
    #[must_use]
    pub fn socks_endpoint(&self) -> Option<&str> {
        match self.kind {
            VpnKind::Socks5 => Some(self.endpoint.as_str()),
            _ => None,
        }
    }
}

/// Terminal preferences persisted with one workspace. Unset fields use built-in defaults.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct TerminalPreferences {
    pub font_family: Option<String>,
    pub font_size: Option<u16>,
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor_shape: Option<String>,
    pub cursor_blink: Option<bool>,
}

/// The full `hl`-side per-workspace config: the bare [`Workspace`] run primitive PLUS the feature settings
/// `hl` maps to engine primitives at launch. Derefs to the inner `Workspace`, so identity/run fields
/// (`name`, `image`, `arch`, `env`, `mounts`, `storage_dir(..)`, …) read through directly;
/// the feature settings are the wrapper's own fields.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceConfig {
    /// The bare run primitive.
    pub ws: Workspace,
    /// Mount the docker socket + set `DOCKER_HOST` so `docker` works inside (default on).
    pub docker_sock: bool,
    /// Terminal scrollback (lines of history each shell retains). `None` = explicitly unlimited. A
    /// TERMINAL knob (see `hl-ws-term`'s `TermConfig::scrollback`) persisted per-workspace here.
    pub scrollback: Option<u64>,
    /// Per-workspace VPN/proxy egress setting. `None` = direct (default).
    pub vpn: Option<VpnConfig>,
    /// Terminal appearance persisted with this workspace.
    pub terminal: TerminalPreferences,
}

pub(super) const DEFAULT_SCROLLBACK_LINES: u64 = 100_000;

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
    /// Stable, whitespace-free identity used at process and filesystem boundaries.
    #[must_use]
    pub fn key(&self) -> String {
        hl_ws::Workspace::storage_component(&self.name)
    }

    /// A fresh config: a bare `Workspace` with the safe feature defaults (docker socket on; everything else off).
    pub fn new(name: impl Into<String>, image: impl Into<String>, arch: Arch) -> WorkspaceConfig {
        WorkspaceConfig::from_ws(Workspace::new(name, image, arch))
    }
    /// Wrap an existing bare `Workspace` with the default feature settings.
    #[must_use]
    pub fn from_ws(ws: Workspace) -> WorkspaceConfig {
        WorkspaceConfig {
            ws,
            docker_sock: true,
            scrollback: Some(DEFAULT_SCROLLBACK_LINES),
            vpn: None,
            terminal: TerminalPreferences::default(),
        }
    }
    /// VTE scrollback-line count to apply. Unlimited (`None`/`0`) maps to a very large, file-backed cap
    /// (same convention as `hl_ws_term::TermConfig::scrollback_lines`).
    #[must_use]
    pub fn scrollback_lines(&self) -> i64 {
        match self.scrollback {
            None | Some(0) => 10_000_000,
            Some(n) => i64::try_from(n).unwrap_or(i64::MAX),
        }
    }

    /// Resolve this workspace's persisted terminal overrides onto terminal defaults.
    #[cfg(feature = "gui")]
    pub fn terminal_config(&self) -> TermConfig {
        let mut config = TermConfig::default();
        if let Some(value) = &self.terminal.font_family {
            config.font_family.clone_from(value);
        }
        if let Some(value) = self.terminal.font_size {
            config.font_size = f64::from(value);
        }
        if let Some(value) = &self.terminal.foreground {
            config.foreground.clone_from(value);
        }
        if let Some(value) = &self.terminal.background {
            config.background.clone_from(value);
        }
        if let Some(value) = self.terminal.cursor_shape.as_deref().and_then(CursorShape::parse) {
            config.cursor_shape = value;
        }
        if let Some(value) = self.terminal.cursor_blink {
            config.cursor_blink = value;
        }
        config.scrollback = self.scrollback;
        config
    }
}

#[cfg(all(test, feature = "gui"))]
mod terminal_tests {
    use super::*;

    #[test]
    fn terminal_config_preserves_explicit_scrollback_policy() {
        let mut workspace = WorkspaceConfig::new("test", "image", Arch::Arm64);
        workspace.scrollback = None;
        assert_eq!(workspace.terminal_config().scrollback, None);

        workspace.scrollback = Some(42);
        assert_eq!(workspace.terminal_config().scrollback, Some(42));
    }
}

/// A file-backed set of workspace configs (`~/.hl/workspaces.conf`) in a small dependency-free block format:
/// `[workspace]` sections with `key = value` fields and repeatable `env` and `mount` fields.
#[derive(Debug)]
pub struct WorkspaceStore {
    path: PathBuf,
    items: Vec<WorkspaceConfig>,
}

impl WorkspaceStore {
    /// Opens the store at `path`. An absent file is an empty store.
    ///
    /// # Errors
    ///
    /// Returns read failures and malformed workspace documents. Callers must not replace a store they
    /// could not read, because doing so could destroy recoverable configuration.
    pub fn load(path: impl Into<PathBuf>) -> io::Result<WorkspaceStore> {
        let path = path.into();
        let items = match std::fs::read_to_string(&path) {
            Ok(text) => WorkspaceDocument::parse(&text)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        Ok(WorkspaceStore { path, items })
    }

    #[must_use]
    pub fn all(&self) -> &[WorkspaceConfig] {
        &self.items
    }
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&WorkspaceConfig> {
        self.items.iter().find(|w| w.name == name)
    }

    #[must_use]
    pub fn get_key(&self, key: &str) -> Option<&WorkspaceConfig> {
        self.items.iter().find(|workspace| workspace.key() == key)
    }

    /// Add or replace a workspace by name, then persist.
    pub fn upsert(&mut self, ws: WorkspaceConfig) -> io::Result<()> {
        let mut items = self.items.clone();
        items.retain(|workspace| workspace.name != ws.name);
        items.push(ws);
        self.save(&items)?;
        self.items = items;
        Ok(())
    }

    /// Remove a workspace by name; returns whether one was removed, then persists.
    pub fn remove(&mut self, name: &str) -> io::Result<bool> {
        let mut items = self.items.clone();
        let before = items.len();
        items.retain(|workspace| workspace.name != name);
        let removed = items.len() != before;
        self.save(&items)?;
        self.items = items;
        Ok(removed)
    }

    fn save(&self, items: &[WorkspaceConfig]) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut out = WorkspaceText::new();
        for w in items {
            out.section();
            out.field("name", &w.name);
            out.field("image", &w.image);
            out.field("arch", w.arch.as_str());
            if let Some(s) = &w.storage {
                out.field("storage", &s.to_string_lossy());
            }
            if let Some(s) = &w.shell {
                out.field("shell", s);
            }
            if let Some(c) = w.cpus {
                out.field("cpus", &c.to_string());
            }
            if let Some(m) = w.memory_mb {
                out.field("memory", &m.to_string());
            }
            out.field("docker_sock", if w.docker_sock { "true" } else { "false" });
            match w.scrollback {
                Some(sb) => out.field("scrollback", &sb.to_string()),
                None => out.field("scrollback", "unlimited"),
            }
            if let Some(vpn) = &w.vpn {
                out.field("vpn", &vpn.to_spec());
            }
            if let Some(value) = &w.terminal.font_family {
                out.field("terminal_font", value);
            }
            if let Some(value) = w.terminal.font_size {
                out.field("terminal_size", &value.to_string());
            }
            if let Some(value) = &w.terminal.foreground {
                out.field("terminal_foreground", value);
            }
            if let Some(value) = &w.terminal.background {
                out.field("terminal_background", value);
            }
            if let Some(value) = &w.terminal.cursor_shape {
                out.field("terminal_cursor", value);
            }
            if let Some(value) = w.terminal.cursor_blink {
                out.field("terminal_cursor_blink", if value { "true" } else { "false" });
            }
            for (k, v) in &w.env {
                out.field("env", &format!("{k}={v}"));
            }
            for m in &w.mounts {
                out.mount(m);
            }
        }
        hl_fs::File::from(self.path.clone()).replace(out.into_string()?)
    }
}

#[cfg(test)]
mod test;
