//! Image discovery: walk the on-disk store, detect each image's target [`Arch`] from binary magic,
//! recover env from an on-disk OCI config, and dedup by tag. Runtime-agnostic — the caller maps the
//! returned [`DiscoveredImage`] values onto its own model.

use super::*;
use serde_json::Value;
use std::path::PathBuf;

mod env;
mod pipeline;
mod sniff;
use env::*;
use pipeline::DiscoveredImages;
pub use sniff::Rootfs;

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
    /// The image labels (`LABEL` / build/commit `--label`), recovered from the sidecar.
    pub labels: std::collections::HashMap<String, String>,
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
/// recovering its run config from the `hl-image.json` sidecar (falling back to the on-disk OCI config for
/// env, and to the dir name for the image name). Collapses duplicate tags to a single best entry.
/// Discovery of images stored below one directory.
pub struct Discovery<'a> {
    directory: &'a str,
}
impl<'a> Discovery<'a> {
    /// Creates discovery rooted at an image-store directory.
    pub fn new(directory: &'a str) -> Self {
        Self { directory }
    }
    /// Reads, enriches, and deterministically deduplicates stored images.
    pub fn images(&self) -> Vec<DiscoveredImage> {
        let images_dir = self.directory;
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(images_dir) else {
            return out;
        };
        for e in rd.flatten() {
            let rootfs = e.path().join("rootfs");
            if !rootfs.is_dir() {
                continue;
            }
            // Prefer hl-image.json so name/cmd/os round-trip exactly (even for images whose binaries can't be
            // sniffed); else parse the dir name + detect the arch from a probe binary.
            let meta = std::fs::read_to_string(e.path().join("hl-image.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok());
            let (name, cmd, arch) = match &meta {
                Some(m) => {
                    let name = m["name"].as_str().unwrap_or("img").to_string();
                    let cmd = Sidecar::strings(&m["cmd"]);
                    // Prefer the arch the sidecar recorded at pull/build time (round-trips exactly, even for
                    // images whose binaries can't be sniffed — distroless/scratch). Fall back to probing the
                    // rootfs, then native arm64.
                    let arch = m["arch"]
                        .as_str()
                        .and_then(|a| Arch::detect(m["os"].as_str().unwrap_or("linux"), a))
                        .or_else(|| Rootfs::new(&rootfs).architecture())
                        .unwrap_or(Arch::LinuxAarch64);
                    (
                        name,
                        if cmd.is_empty() {
                            vec!["/bin/sh".into()]
                        } else {
                            cmd
                        },
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
                        Rootfs::new(&rootfs)
                            .architecture()
                            .unwrap_or(Arch::LinuxAarch64),
                    )
                }
            };
            let arr = |k: &str| {
                meta.as_ref()
                    .map(|m| Sidecar::strings(&m[k]))
                    .unwrap_or_default()
            };
            let entrypoint = arr("entrypoint");
            let workdir = Sidecar(meta.as_ref()).string("workdir");
            let user = Sidecar(meta.as_ref()).string("user");
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
                let recovered = OciDisk(&e.path()).environment();
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
            let stop_signal = Sidecar(meta.as_ref()).string("stop_signal");
            let img_volumes = arr("img_volumes");
            // Image labels from the sidecar (a `{"k":"v"}` object) so they survive a daemon restart/discovery.
            let labels: std::collections::HashMap<String, String> = meta
                .as_ref()
                .and_then(|m| m["labels"].as_object())
                .map(|o| {
                    o.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
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
                labels,
                created,
                stop_signal,
                img_volumes,
                healthcheck,
            });
        }
        DiscoveredImages::from_vec(out).deduplicate()
    }
}
