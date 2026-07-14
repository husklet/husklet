//! File-backed workspace persistence (`~/.dd/workspaces.conf`), in a tiny dependency-free block format
//! (`[workspace]` + `key = value` lines, repeatable `env`/`mount`) — no serde/toml so the core stays
//! std-only. Still reads the legacy one-line `name<TAB>arch<TAB>image` rows so old config keeps working.

use crate::model::{Arch, CudaDevice, Mount, VpnConfig, Workspace};
use std::io;
use std::path::PathBuf;

/// A file-backed set of workspaces.
pub struct WorkspaceStore {
    path: PathBuf,
    items: Vec<Workspace>,
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

fn clean(s: &str) -> String {
    s.chars().filter(|&c| c != '\t' && c != '\n' && c != '\r').collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("hl-ws-store-{}-{}.conf", std::process::id(), tag));
        p
    }

    #[test]
    fn store_persists_across_reload() {
        let path = tmp_path("persist");
        let _ = std::fs::remove_file(&path);
        let mut store = WorkspaceStore::load(&path);
        assert!(store.all().is_empty());
        store.upsert(Workspace::new("ubuntu-dev", "ubuntu:24.04", Arch::Arm64)).unwrap();
        store.upsert(Workspace::new("legacy", "centos:7", Arch::Amd64)).unwrap();
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
        ws.cuda = Some(CudaDevice::parse("dd Metal (CUDA-sim) Device|8.6|16384").unwrap());
        let mut store = WorkspaceStore::load(&path);
        store.upsert(ws.clone()).unwrap();

        let got = WorkspaceStore::load(&path).get("api").cloned().unwrap();
        assert_eq!(got, ws, "rich workspace should round-trip through the block format");
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
        store.upsert(Workspace::new("a", "alpine", Arch::Arm64)).unwrap();
        assert!(store.remove("a").unwrap());
        assert!(!store.remove("a").unwrap());
        assert!(WorkspaceStore::load(&path).all().is_empty());
        let _ = std::fs::remove_file(&path);
    }
}
