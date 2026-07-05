//! Image discovery: walk the on-disk store, detect each image's target [`Arch`] from binary magic,
//! recover env from an on-disk OCI config, and dedup by tag. Runtime-agnostic — the caller maps the
//! returned [`DiscoveredImage`] values onto its own model.

use super::*;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

/// One image found on disk by [`discover_images`]: its identity + run config + detected [`Arch`]. Plain
/// data (no runtime type) — the caller maps it onto its own image model. `healthcheck` is carried as the
/// raw OCI/docker JSON so this crate stays free of any daemon healthcheck type.
#[derive(Clone, Debug)]
pub struct DiscoveredImage {
    /// The image reference (`repository:tag` or a bare name recovered from the dir).
    pub name: String,
    /// The unpacked root filesystem.
    pub rootfs: PathBuf,
    /// The target detected from the sidecar, else probed from the rootfs, else native arm64.
    pub arch: Arch,
    /// The default command (never empty — falls back to `/bin/sh`).
    pub cmd: Vec<String>,
    /// The environment (`K=V` lines), recovered from an on-disk OCI config when the sidecar had none.
    pub env: Vec<String>,
    /// The entrypoint (prepended to the command).
    pub entrypoint: Vec<String>,
    /// The working directory (empty if unset).
    pub workdir: String,
    /// The default run user (empty if unset).
    pub user: String,
    /// The exposed-port keys (e.g. `"5432/tcp"`).
    pub exposed_ports: Vec<String>,
    /// The rootfs mtime as unix seconds (image creation/discovery time).
    pub created: i64,
    /// The `docker stop` signal (`Config.StopSignal`); empty ⇒ SIGTERM.
    pub stop_signal: String,
    /// The dirs that get an anonymous volume at run (`Config.Volumes` keys).
    pub img_volumes: Vec<String>,
    /// The container healthcheck probe as raw JSON (`None` ⇒ no probe recorded).
    pub healthcheck: Option<Value>,
}

/// Discover `<images_dir>/<name>/rootfs` dirs, detecting each image's [`Arch`] from a probe binary and
/// recovering its run config from the `dd-image.json` sidecar (falling back to the on-disk OCI config for
/// env, and to the dir name for the image name). Collapses duplicate tags to a single best entry.
pub fn discover_images(images_dir: &str) -> Vec<DiscoveredImage> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(images_dir) else {
        return out;
    };
    for e in rd.flatten() {
        let rootfs = e.path().join("rootfs");
        if !rootfs.is_dir() {
            continue;
        }
        // Prefer dd-image.json so name/cmd/os round-trip exactly (macOS images have no probe-able ELF);
        // else parse the dir name + detect the arch from a probe binary.
        let meta = std::fs::read_to_string(e.path().join("dd-image.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let (name, cmd, arch) = match &meta {
            Some(m) => {
                let name = m["name"].as_str().unwrap_or("img").to_string();
                let cmd = json_strs(&m["cmd"]);
                // Prefer the arch the sidecar recorded at pull/build time (round-trips exactly, even for
                // images whose binaries can't be sniffed — distroless/scratch). `os:darwin` marks a
                // native-macOS (darwinjail) image. Fall back to probing the rootfs, then native arm64.
                let arch = m["arch"]
                    .as_str()
                    .and_then(|a| Arch::detect(m["os"].as_str().unwrap_or("linux"), a))
                    .or_else(|| (m["os"].as_str() == Some("darwin")).then_some(Arch::DarwinAarch64))
                    .or_else(|| detect_arch(&rootfs))
                    .unwrap_or(Arch::LinuxAarch64);
                (
                    name,
                    if cmd.is_empty() { vec!["/bin/sh".into()] } else { cmd },
                    arch,
                )
            }
            None => {
                let raw = e
                    .path()
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("img")
                    .to_string();
                let name = raw
                    .trim_end_matches("-bundle")
                    .split("__")
                    .next()
                    .unwrap_or("img")
                    .rsplit('_')
                    .next()
                    .unwrap_or("img")
                    .to_string();
                (
                    name,
                    vec!["/bin/sh".into()],
                    detect_arch(&rootfs).unwrap_or(Arch::LinuxAarch64),
                )
            }
        };
        let arr = |k: &str| meta.as_ref().map(|m| json_strs(&m[k])).unwrap_or_default();
        let entrypoint = arr("entrypoint");
        let workdir = meta_str(&meta, "workdir");
        let user = meta_str(&meta, "user");
        let exposed_ports = arr("exposed_ports");
        let created = std::fs::metadata(&rootfs)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // The sidecar is the source of truth, but pre-seeded/umoci-built images (and any image cached
        // before the pull path recorded env) carry an empty `env` — their environment lives only in the
        // on-disk OCI config. Recover it from there so a daemon restart doesn't drop TERM/HOME/LANG/PATH,
        // then persist it back into the sidecar so subsequent discovery is self-contained.
        let mut env = arr("env");
        if env.is_empty() {
            let recovered = oci_disk_env(&e.path());
            if !recovered.is_empty() {
                persist_discovered_env(
                    &e.path(),
                    meta.as_ref(),
                    &name,
                    &cmd,
                    &recovered,
                    &entrypoint,
                    &workdir,
                    arch,
                );
                env = recovered;
            }
        }
        // Lifecycle/volume image config — restored from the sidecar so `docker stop` picks the right
        // signal and anon volumes / healthcheck survive a daemon restart.
        let stop_signal = meta_str(&meta, "stop_signal");
        let img_volumes = arr("img_volumes");
        let healthcheck = meta
            .as_ref()
            .map(|m| m["healthcheck"].clone())
            .filter(|v| v.is_object());
        out.push(DiscoveredImage {
            name,
            rootfs,
            arch,
            cmd,
            env,
            entrypoint,
            workdir,
            user,
            exposed_ports,
            created,
            stop_signal,
            img_volumes,
            healthcheck,
        });
    }
    dedup_images(out)
}

/// A JSON string array flattened to `Vec<String>` (non-array / non-string entries dropped).
fn json_strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// A string field from an optional sidecar, empty when absent.
fn meta_str(meta: &Option<Value>, key: &str) -> String {
    meta.as_ref().and_then(|m| m[key].as_str()).unwrap_or("").to_string()
}

/// A coarse "richness" score for a [`DiscoveredImage`], used to pick the best entry when several
/// directories resolve to the same tag (see [`dedup_images`]). A non-empty environment is the decisive
/// signal: `poc/images` ships some images twice — a single-underscore dd-format dir whose sidecar
/// recorded an empty `env`, AND a umoci bundle dir carrying the full OCI config — and the bundle one
/// (real env) must win. The remaining run metadata break finer ties.
pub fn image_score(img: &DiscoveredImage) -> i32 {
    let mut s = 0;
    if !img.env.is_empty() {
        s += 1000;
    }
    if !img.entrypoint.is_empty() {
        s += 10;
    }
    if !img.workdir.is_empty() {
        s += 5;
    }
    // A recorded CMD beats the `/bin/sh` default the discovery fallback substitutes.
    if img.cmd.len() != 1 || img.cmd[0] != "/bin/sh" {
        s += 1;
    }
    s
}

/// Collapse images that resolve to the same `repository:tag` down to a single best entry so lookup is
/// deterministic regardless of `read_dir` order. Ranks by [`image_score`] (richest wins) and breaks
/// exact ties on the name string so the survivor is stable across runs and machines.
fn dedup_images(mut imgs: Vec<DiscoveredImage>) -> Vec<DiscoveredImage> {
    imgs.sort_by(|a, b| {
        image_score(b)
            .cmp(&image_score(a))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut seen = std::collections::HashSet::new();
    imgs.retain(|i| seen.insert(repo_tag(&i.name)));
    imgs
}

/// Best-effort recovery of an image's environment from an on-disk OCI config, used by
/// [`discover_images`] when the `dd-image.json` sidecar recorded no `env` (pre-seeded / umoci-built
/// images, or images cached before the pull path persisted env). Two layouts are understood, in order:
///   1. umoci's runtime `config.json` at the image dir root -> `process.env`.
///   2. an OCI image layout in the dir (`index.json` + `blobs/sha256/`) -> manifest -> image config
///      blob -> `config.Env`.
/// Returns an empty vec if neither is present/parseable — never panics, never fails discovery.
fn oci_disk_env(dir: &Path) -> Vec<String> {
    // 1. umoci runtime config: process.env.
    if let Some(cfg) = std::fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        let env = json_strs(&cfg["process"]["env"]);
        if !env.is_empty() {
            return env;
        }
    }
    // 2. OCI image layout: index.json -> first manifest blob -> image config blob -> config.Env.
    let read_blob = |digest: &str| -> Option<Value> {
        let hex = digest.strip_prefix("sha256:")?;
        std::fs::read_to_string(dir.join("blobs/sha256").join(hex))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    };
    let index = std::fs::read_to_string(dir.join("index.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    if let Some(mdigest) = index
        .as_ref()
        .and_then(|i| i["manifests"].as_array())
        .and_then(|a| a.first())
        .and_then(|m| m["digest"].as_str())
    {
        if let Some(cfg) = read_blob(mdigest)
            .and_then(|m| m["config"]["digest"].as_str().map(String::from))
            .and_then(|d| read_blob(&d))
        {
            return json_strs(&cfg["config"]["Env"]);
        }
    }
    Vec::new()
}

/// Persist an env recovered by [`oci_disk_env`] back into the image's `dd-image.json` sidecar so the
/// next discovery round-trips it directly (and never has to re-parse the OCI config). Merges into the
/// existing sidecar when present so other recorded fields are preserved; otherwise writes a fresh one
/// from the values [`discover_images`] already resolved. Best-effort: a write failure (e.g. a
/// read-only image store) is ignored — the in-memory env is still surfaced for this run.
#[allow(clippy::too_many_arguments)]
fn persist_discovered_env(
    dir: &Path,
    meta: Option<&Value>,
    name: &str,
    cmd: &[String],
    env: &[String],
    entrypoint: &[String],
    workdir: &str,
    arch: Arch,
) {
    let mut m = meta.cloned().unwrap_or_else(|| {
        json!({
            "name": name, "cmd": cmd, "entrypoint": entrypoint, "workdir": workdir,
            "arch": arch.isa(), "os": arch.os(),
        })
    });
    m["env"] = json!(env);
    let _ = std::fs::write(dir.join("dd-image.json"), m.to_string());
}

/// Classify a binary by its leading magic bytes: ELF -> linux (e_machine = aarch64/x86_64), Mach-O 64 ->
/// darwin (cputype = arm64). Returns `None` for anything else (scripts, data, an unrecognized machine).
fn sniff_magic(b: &[u8]) -> Option<Arch> {
    if b.len() > 19 && &b[0..4] == b"\x7fELF" {
        return match u16::from_le_bytes([b[18], b[19]]) {
            // ELF e_machine
            0xB7 => Some(Arch::LinuxAarch64),
            0x3E => Some(Arch::LinuxX86_64),
            _ => None,
        };
    }
    if b.len() > 7 && b[0..4] == [0xCF, 0xFA, 0xED, 0xFE] {
        // MH_MAGIC_64 (little-endian)
        return match u32::from_le_bytes([b[4], b[5], b[6], b[7]]) {
            // cputype
            0x0100000C => Some(Arch::DarwinAarch64), // CPU_TYPE_ARM64
            _ => None,
        };
    }
    None
}

/// Read just the header of `p` (following symlinks) and classify its magic. Cheap: only the first 20
/// bytes are read, never the whole binary.
fn sniff_path(p: &Path) -> Option<Arch> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = [0u8; 20];
    let n = f.read(&mut buf).ok()?;
    sniff_magic(&buf[..n])
}

/// Fallback arch probe: a bounded breadth-first scan of the rootfs for the first binary whose magic
/// identifies a target. Catches images that ship a single executable at a non-standard path
/// (hello-world's `/hello`, nats's `/nats-server`) which the fixed probe list in [`detect_arch`] misses.
/// Shallow entries are examined first (top-level binaries win immediately) and the total entry budget is
/// capped so a large rootfs can never make discovery pathological. Symlinked directories are not
/// descended (avoids cycles); symlinked *files* are still classified (their target is read).
fn scan_for_binary(rootfs: &Path) -> Option<Arch> {
    let mut queue = VecDeque::from([rootfs.to_path_buf()]);
    let mut budget = 4096; // cap on entries examined across the whole walk
    while let Some(dir) = queue.pop_front() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            match e.file_type() {
                Ok(ft) if ft.is_dir() => queue.push_back(e.path()),
                Ok(ft) if ft.is_file() || ft.is_symlink() => {
                    if let Some(g) = sniff_path(&e.path()) {
                        return Some(g);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Probe the rootfs and pick the target [`Arch`] from a binary's magic. Tries a handful of well-known
/// executable locations first (fast path), then falls back to a bounded scan of the whole rootfs so an
/// image with its binary at a non-standard path is still detected.
pub fn detect_arch(rootfs: &Path) -> Option<Arch> {
    // Includes darwin-userland paths (`profile/bin/*`, `opt/homebrew/bin/*`) so a *pulled* macOS image
    // — whose `dd-image.json` sidecar didn't survive the registry round-trip — is still detected as
    // darwin from its packed Mach-O binaries. `sniff_path` follows the profile symlinks to the real
    // Mach-O in the packed `/nix` (or Homebrew) closure.
    for probe in [
        "bin/busybox",
        "bin/sh",
        "bin/true",
        "usr/bin/coreutils",
        "usr/lib/dyld",
        "profile/bin/bash",
        "profile/bin/sh",
        "opt/homebrew/bin/brew",
    ] {
        if let Some(g) = sniff_path(&rootfs.join(probe)) {
            return Some(g);
        }
    }
    scan_for_binary(rootfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, "poor" DiscoveredImage (empty env, default `/bin/sh` cmd) to perturb per test.
    fn base(name: &str) -> DiscoveredImage {
        DiscoveredImage {
            name: name.to_string(),
            rootfs: PathBuf::from("/nonexistent"),
            arch: Arch::LinuxAarch64,
            cmd: vec!["/bin/sh".to_string()],
            env: vec![],
            entrypoint: vec![],
            workdir: String::new(),
            user: String::new(),
            exposed_ports: vec![],
            created: 0,
            stop_signal: String::new(),
            img_volumes: vec![],
            healthcheck: None,
        }
    }

    #[test]
    fn image_score_env_is_decisive() {
        // A poor bundle scores 0; env alone is worth more than every finer signal combined.
        assert_eq!(image_score(&base("x")), 0);

        let mut with_env = base("x");
        with_env.env = vec!["PATH=/usr/bin".to_string()];
        assert_eq!(image_score(&with_env), 1000);

        // The finer tie-breakers: entrypoint (+10), workdir (+5), non-default cmd (+1).
        let mut rich_but_no_env = base("x");
        rich_but_no_env.entrypoint = vec!["/entry".to_string()];
        rich_but_no_env.workdir = "/app".to_string();
        rich_but_no_env.cmd = vec!["/run".to_string()];
        assert_eq!(image_score(&rich_but_no_env), 16);
        // env still wins outright over all finer metadata combined.
        assert!(image_score(&with_env) > image_score(&rich_but_no_env));
    }

    #[test]
    fn dedup_prefers_the_bundle_with_env() {
        // Two dirs resolve to the same tag (`busybox:latest`): one poor (no env), one with env.
        let poor = base("busybox");
        let mut rich = base("busybox");
        rich.env = vec!["HOME=/root".to_string()];

        // Order must not matter: the env-carrying entry always survives.
        for imgs in [vec![poor.clone(), rich.clone()], vec![rich.clone(), poor.clone()]] {
            let out = dedup_images(imgs);
            assert_eq!(out.len(), 1, "same tag collapses to one entry");
            assert_eq!(out[0].env, vec!["HOME=/root".to_string()], "env bundle wins");
        }
    }

    #[test]
    fn dedup_keeps_distinct_tags() {
        let out = dedup_images(vec![base("busybox"), base("alpine")]);
        assert_eq!(out.len(), 2);
    }
}
