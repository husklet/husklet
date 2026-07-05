#![allow(unused_imports, dead_code)]
use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::images::*;
use crate::model::*;
use crate::networks::*;
use crate::registry::{Client, Credentials, ImageRef};
use crate::runtime::*;
use crate::system::*;
use crate::volumes::*;
use axum::body::Body;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use ddjit::{Guest, PortMap, SpawnConfig, Volume};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch, Mutex};

pub(crate) const API_VERSION: &str = "1.43";

/// Resolve a container ref (full id, **id prefix** like the docker CLI sends, or short name) to its
/// full map key. Docker clients show/round-trip the 12-char short id, so prefix resolution is
/// required for `docker logs/inspect/rm <shortid>` to work.
pub(crate) fn resolve_cid(g: &Inner, id: &str) -> Option<String> {
    if g.containers.contains_key(id) {
        return Some(id.to_string());
    }
    let hits: Vec<String> = g
        .containers
        .keys()
        .filter(|k| k.starts_with(id))
        .cloned()
        .collect();
    if hits.len() == 1 {
        return hits.into_iter().next();
    }
    // Fall back to the short-id "name" we expose in containers_json, then the user-assigned --name.
    let want = id.trim_start_matches('/');
    g.containers
        .keys()
        .find(|k| k.get(..12).map(|p| p == want).unwrap_or(false))
        .cloned()
        .or_else(|| {
            g.containers
                .iter()
                .find(|(_, c)| c.name == want)
                .map(|(k, _)| k.clone())
        })
}

/// One Docker log frame: `[stream(1B), 0,0,0, len(4B big-endian)] + payload`.
pub(crate) fn log_frame(stream: u8, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + data.len());
    out.push(stream);
    out.extend_from_slice(&[0, 0, 0]);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(data);
    out
}

pub(crate) fn no_such(id: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"message": format!("No such container: {id}")})),
    )
        .into_response()
}

/// A cheap, stable hex id for a built image (not a real digest — just a handle for the CLI).
pub(crate) fn md5_like(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Standard base64 (no line breaks).
pub(crate) fn base64_std(data: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---- persistence -----------------------------------------------------------

/// `~/.dd` (or `./.dd` if `$HOME` is unset) — the default state/volumes root.
pub(crate) fn dd_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".dd")
}

/// `~/.dd/buildcache` — the `docker build` layer cache root (one dir per cached step under `layers/`).
/// Distinct from `~/.dd/pcache` (the JIT translated-code cache surfaced as `system df` BuilderSize).
pub(crate) fn buildcache_dir() -> PathBuf {
    dd_home().join("buildcache")
}

// ---- daemon-write coherence (docker cp epoch blind spot) --------------
// The engine's path/metadata caches (dd-jit fscache.c) are invalidated by GUEST syscalls, but the daemon
// also writes into a LIVE container's filesystem from outside any engine — `docker cp`
// (PUT /containers/{id}/archive) and the exec-spawn /etc/{hosts,resolv.conf,hostname} rewrites — which no
// guest syscall announces, so a cached ENOENT could hide a file docker-cp just delivered. The contract
// with the engine: a 4-byte native-endian u32 "external-writer generation" file per container at
// `<dd_home>/containers/<cid>/fsgen`. spawn_cfg hands its path to every engine of the container
// (DD_FSGEN_FILE — run, exec and health probes share one file, keyed like tmpfs by the target container
// id); each engine maps it MAP_SHARED and polls it once per guest syscall, dropping ALL its caches when
// it moves. The daemon calls [`fsgen_bump`] AFTER completing any such write, making the write visible to
// the guest no later than its next syscall (kernel-dcache semantics, like real Docker on Linux).

/// The container's external-writer generation file (see the module comment above).
pub(crate) fn fsgen_path(cid: &str) -> PathBuf {
    dd_home().join("containers").join(cid).join("fsgen")
}

/// Create the generation file (value 1) if it doesn't exist yet; returns its path. Called before any
/// engine of the container spawns (spawn_cfg) and defensively by [`fsgen_bump`]. Best-effort.
pub(crate) fn fsgen_ensure(cid: &str) -> PathBuf {
    let p = fsgen_path(cid);
    if !p.exists() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(&p, 1u32.to_ne_bytes());
    }
    p
}

/// Atomically increment the container's external-writer generation — call AFTER a daemon-side write into
/// the container's filesystem completes. mmap + atomic fetch_add matches the engine's read side (same
/// 32-bit width; Release pairs with the engine's acquire load so the flush orders after our file writes).
/// Best-effort: a failure only means the engine re-learns the write through its normal (slower) paths.
pub(crate) fn fsgen_bump(cid: &str) {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicU32, Ordering};
    let p = fsgen_ensure(cid);
    let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(&p) else {
        return;
    };
    if f.metadata().map(|m| m.len()).unwrap_or(0) < 4 && f.set_len(4).is_err() {
        return;
    }
    unsafe {
        let m = libc::mmap(
            std::ptr::null_mut(),
            4,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            f.as_raw_fd(),
            0,
        );
        if m == libc::MAP_FAILED {
            return;
        }
        (*(m as *const AtomicU32)).fetch_add(1, Ordering::Release);
        libc::munmap(m, 4);
    }
}

pub(crate) fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Nanoseconds since the unix epoch. Backs the sub-second precision of `State.StartedAt`/`FinishedAt`
/// (docker reports these to the nanosecond) so a quick `docker restart` — which can re-start within the
/// same wall-clock SECOND — still advances `StartedAt`, matching docker (a second-precision stamp would
/// collide and report the restart as a no-op).
pub(crate) fn now_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

/// Format a unix timestamp as an RFC3339 UTC string (Docker's inspect `Created` is a string).
/// Pure integer civil-date math (Howard Hinnant's algorithm) — no chrono dependency.
pub(crate) fn fmt_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400);
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// RFC3339 with nanosecond fraction (docker's `State.StartedAt`/`FinishedAt` shape, e.g.
/// `2026-07-02T21:32:17.123456789Z`). `nanos` is a unix-epoch nanosecond count; the whole-second part
/// reuses [`fmt_rfc3339`] and the fraction is appended, so two events in the same second differ.
pub(crate) fn fmt_rfc3339_nanos(nanos: i64) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let frac = nanos.rem_euclid(1_000_000_000);
    let base = fmt_rfc3339(secs);
    // splice the fraction before the trailing 'Z'
    format!("{}.{:09}Z", &base[..base.len() - 1], frac)
}

/// Write containers/volumes/networks to `path` atomically (temp file + rename). Best-effort:
/// persistence failures are logged but never abort a request.
pub(crate) fn save_state(inner: &Inner, path: &str) {
    let p = Persisted {
        containers: inner.containers.values().cloned().collect(),
        volumes: inner.volumes.clone(),
        networks: inner.networks.clone(),
    };
    let Ok(bytes) = serde_json::to_vec_pretty(&p) else {
        return;
    };
    if let Some(parent) = std::path::Path::new(path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = format!("{path}.tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        if let Err(e) = std::fs::rename(&tmp, path) {
            eprintln!("[dd-daemon] state save failed: {e}");
        }
    }
}

/// Load persisted state into `inner`, re-resolving each container's arch/rootfs from the
/// freshly discovered images.
pub(crate) fn load_state(inner: &mut Inner, path: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let Ok(p) = serde_json::from_slice::<Persisted>(&bytes) else {
        eprintln!("[dd-daemon] ignoring unreadable state file {path}");
        return;
    };
    for mut c in p.containers {
        if let Some(img) = inner
            .images
            .iter()
            .find(|i| i.name == c.image || format!("{}:latest", i.name) == c.image)
        {
            c.arch = Some(img.arch);
            c.rootfs = img.rootfs.clone();
        } else {
            c.arch = Some(Guest::LinuxAarch64);
        }
        inner.containers.insert(c.id.clone(), c);
    }
    inner.volumes = p.volumes;
    inner.networks = p.networks;
}

/// Discover <images>/<name>/rootfs dirs, detecting each image's guest arch from a probe ELF.
pub(crate) fn discover_images(images_dir: &str) -> Vec<Image> {
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
                let cmd: Vec<String> = m["cmd"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                // Prefer the arch the sidecar recorded at pull/build time (round-trips exactly, even for
                // images whose binaries can't be sniffed — distroless/scratch). `os:darwin` marks a
                // native-macOS (darwinjail) image. Fall back to probing the rootfs, then native arm64.
                let arch = m["arch"]
                    .as_str()
                    .and_then(|a| Guest::detect(m["os"].as_str().unwrap_or("linux"), a))
                    .or_else(|| {
                        (m["os"].as_str() == Some("darwin")).then_some(Guest::DarwinAarch64)
                    })
                    .or_else(|| detect_arch(&rootfs))
                    .unwrap_or(Guest::LinuxAarch64);
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
                    detect_arch(&rootfs).unwrap_or(Guest::LinuxAarch64),
                )
            }
        };
        let arr = |k: &str| {
            meta.as_ref()
                .and_then(|m| m[k].as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(String::from))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let entrypoint = arr("entrypoint");
        let workdir = meta
            .as_ref()
            .and_then(|m| m["workdir"].as_str())
            .unwrap_or("")
            .to_string();
        let user = meta
            .as_ref()
            .and_then(|m| m["user"].as_str())
            .unwrap_or("")
            .to_string();
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
        // Lifecycle/volume image config (Moby §6/§8) — restored from the sidecar so `docker stop` picks
        // the right signal and anon volumes / healthcheck survive a daemon restart.
        let stop_signal = meta
            .as_ref()
            .and_then(|m| m["stop_signal"].as_str())
            .unwrap_or("")
            .to_string();
        let img_volumes = arr("img_volumes");
        let healthcheck = meta.as_ref().and_then(|m| {
            serde_json::from_value::<crate::model::HealthConfig>(m["healthcheck"].clone()).ok()
        });
        out.push(Image {
            name,
            rootfs: rootfs.to_string_lossy().into_owned(),
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
            ..Default::default()
        });
    }
    dedup_images(out)
}

/// A coarse "richness" score for a discovered [`Image`], used to pick the best entry when several
/// directories resolve to the same image (see [`dedup_images`] / [`find_image`]). A non-empty
/// environment is the decisive signal: `poc/images` ships some images twice — a single-underscore
/// dd-format dir whose sidecar recorded an empty `env`, AND a umoci bundle dir carrying the full OCI
/// config — and the bundle one (real env) must win. The remaining run metadata break finer ties.
pub(crate) fn image_score(img: &Image) -> i32 {
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
    s += img.labels.len() as i32;
    // A recorded CMD beats the `/bin/sh` default the discovery fallback substitutes.
    if img.cmd.len() != 1 || img.cmd[0] != "/bin/sh" {
        s += 1;
    }
    s
}

/// Collapse images that resolve to the same `repository:tag` down to a single best entry so lookup is
/// deterministic regardless of `read_dir` order. Without this, two on-disk dirs for one tag (the
/// dd-format + umoci-bundle duplicates `poc/images` ships) would both enter the store and `docker
/// inspect`/`run` would surface whichever the filesystem happened to list first — sometimes the
/// empty-env one. Ranks by [`image_score`] (richest wins) and breaks exact ties on the name string so
/// the survivor is stable across runs and machines.
fn dedup_images(mut imgs: Vec<Image>) -> Vec<Image> {
    // Best-first ordering (then name, for a deterministic tie-break); keep the first seen per tag.
    imgs.sort_by(|a, b| {
        image_score(b)
            .cmp(&image_score(a))
            .then_with(|| a.name.cmp(&b.name))
    });
    let mut seen = std::collections::HashSet::new();
    imgs.retain(|i| seen.insert(repo_tag(&i.name)));
    imgs
}

/// Pick the single best image matching a docker reference (`docker inspect ubuntu`, `docker run
/// alpine`), deterministically. dd's lookup is lenient — a bare name matches any stored image with that
/// repository regardless of tag (see [`ref_name`]) — so several images can match one query (e.g. an
/// `ubuntu` and an `ubuntu:24.04`). Returning `Iterator::find`'s first hit made the result depend on
/// insertion order; instead rank the candidates and return the best:
///   1. an exact `repository:tag` match for the requested reference wins, then `<name>:latest`,
///   2. then the richest metadata (a real environment beats an empty one — see [`image_score`]),
///   3. then the name string (reversed so the lexicographically smallest wins) to settle any remainder.
pub(crate) fn find_image<'a>(images: &'a [Image], reference: &str) -> Option<&'a Image> {
    let want_repo = ref_repo(reference);
    let want = ref_name(reference);
    let want_rt = repo_tag(reference);
    let want_latest = format!("{want}:latest");
    images
        .iter()
        // Match on the fully-qualified repository (registry+namespace+name), NOT the bare basename, so a
        // bare official `nginx` never resolves to a third-party `linuxserver/nginx`.
        .filter(|i| ref_repo(&i.name) == want_repo)
        .max_by_key(|i| {
            (
                repo_tag(&i.name) == want_rt,
                repo_tag(&i.name) == want_latest,
                image_score(i),
                std::cmp::Reverse(i.name.clone()),
            )
        })
}

/// Best-effort recovery of an image's environment from an on-disk OCI config, used by
/// [`discover_images`] when the `dd-image.json` sidecar recorded no `env` (pre-seeded / umoci-built
/// images, or images cached before the pull path persisted env). Two layouts are understood, in order:
///   1. umoci's runtime `config.json` at the image dir root -> `process.env`.
///   2. an OCI image layout in the dir (`index.json` + `blobs/sha256/`) -> manifest -> image config
///      blob -> `config.Env`.
/// Returns an empty vec if neither is present/parseable — never panics, never fails discovery.
fn oci_disk_env(dir: &std::path::Path) -> Vec<String> {
    let strs = |v: &Value| {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    // 1. umoci runtime config: process.env.
    if let Some(cfg) = std::fs::read_to_string(dir.join("config.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    {
        let env = strs(&cfg["process"]["env"]);
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
            return strs(&cfg["config"]["Env"]);
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
    dir: &std::path::Path,
    meta: Option<&Value>,
    name: &str,
    cmd: &[String],
    env: &[String],
    entrypoint: &[String],
    workdir: &str,
    arch: Guest,
) {
    let mut m = meta.cloned().unwrap_or_else(|| {
        json!({
            "name": name, "cmd": cmd, "entrypoint": entrypoint, "workdir": workdir,
            "arch": arch.arch(), "os": arch.os(),
        })
    });
    m["env"] = json!(env);
    let _ = std::fs::write(dir.join("dd-image.json"), m.to_string());
}

/// Classify a binary by its leading magic bytes: ELF -> linux (e_machine = aarch64/x86_64),
/// Mach-O 64 -> darwin (cputype = arm64). Returns `None` for anything else (scripts, data, an
/// unrecognized machine).
fn sniff_magic(b: &[u8]) -> Option<Guest> {
    if b.len() > 19 && &b[0..4] == b"\x7fELF" {
        return match u16::from_le_bytes([b[18], b[19]]) {
            // ELF e_machine
            0xB7 => Some(Guest::LinuxAarch64),
            0x3E => Some(Guest::LinuxX86_64),
            _ => None,
        };
    }
    if b.len() > 7 && b[0..4] == [0xCF, 0xFA, 0xED, 0xFE] {
        // MH_MAGIC_64 (little-endian)
        return match u32::from_le_bytes([b[4], b[5], b[6], b[7]]) {
            // cputype
            0x0100000C => Some(Guest::DarwinAarch64), // CPU_TYPE_ARM64
            _ => None,
        };
    }
    None
}

/// Read just the header of `p` (following symlinks) and classify its magic. Cheap: only the first 20
/// bytes are read, never the whole binary.
fn sniff_path(p: &std::path::Path) -> Option<Guest> {
    use std::io::Read;
    let mut f = std::fs::File::open(p).ok()?;
    let mut buf = [0u8; 20];
    let n = f.read(&mut buf).ok()?;
    sniff_magic(&buf[..n])
}

/// Fallback arch probe: a bounded breadth-first scan of the rootfs for the first binary whose magic
/// identifies a guest target. Catches images that ship a single executable at a non-standard path
/// (hello-world's `/hello`, nats's `/nats-server`) which the fixed probe list in [`detect_arch`] misses.
/// Shallow entries are examined first (top-level binaries win immediately) and the total entry budget is
/// capped so a large rootfs can never make discovery pathological. Symlinked directories are not
/// descended (avoids cycles); symlinked *files* are still classified (their target is read).
fn scan_for_binary(rootfs: &std::path::Path) -> Option<Guest> {
    use std::collections::VecDeque;
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

/// Probe the rootfs and pick the guest target from a binary's magic. Tries a handful of well-known
/// executable locations first (fast path), then falls back to a bounded scan of the whole rootfs so an
/// image with its binary at a non-standard path is still detected.
pub(crate) fn detect_arch(rootfs: &std::path::Path) -> Option<Guest> {
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

pub(crate) fn fake_id(seed: &str) -> String {
    let mut h: u64 = 1469598103934665603;
    for b in seed.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    format!("{h:016x}{h:016x}{h:016x}{h:08x}")
}

/// On-disk size of an image's rootfs, cached per rootfs path (computed once; rootfs rarely changes).
/// The host-fs `macos` image is skipped (walking `/` would be catastrophic).
pub(crate) fn image_size(rootfs: &str, name: &str) -> i64 {
    if name == "macos" {
        return 0;
    }
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(s) = cache.lock().unwrap().get(rootfs) {
        return *s;
    }
    let s = dir_size(std::path::Path::new(rootfs));
    cache.lock().unwrap().insert(rootfs.to_string(), s);
    s
}

/// Recursively sum the size of regular files under `p` (symlinks are not followed).
pub(crate) fn dir_size(p: &std::path::Path) -> i64 {
    let mut total = 0i64;
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    for e in rd.flatten() {
        let Ok(md) = e.path().symlink_metadata() else {
            continue;
        };
        let ft = md.file_type();
        if ft.is_symlink() {
            continue;
        } else if ft.is_dir() {
            total += dir_size(&e.path());
        } else {
            total += md.len() as i64;
        }
    }
    total
}

/// The bare repository name of a docker image reference, ignoring registry, namespace and tag/digest:
/// `docker.io/library/ubuntu:latest` -> `ubuntu`, `library/ubuntu` -> `ubuntu`, `ubuntu:22.04` -> `ubuntu`.
/// Lets `docker run ubuntu` match an image discovered/tagged as `ubuntu` regardless of how docker
/// canonicalizes the reference.
pub(crate) fn ref_name(s: &str) -> &str {
    let last = s.rsplit('/').next().unwrap_or(s);
    last.split('@')
        .next()
        .unwrap_or(last)
        .split(':')
        .next()
        .unwrap_or(last)
}

/// The FULLY-QUALIFIED canonical repository of an image reference — registry + namespace + name, tag
/// stripped, with Docker Hub's implicit `library/` namespace made explicit. This is the correct key
/// for "is this the same image?" because it distinguishes repositories that merely share a final path
/// component: `nginx`, `library/nginx`, `docker.io/library/nginx:1.25` all map to
/// `registry-1.docker.io/library/nginx`, but `linuxserver/nginx` maps to
/// `registry-1.docker.io/linuxserver/nginx`. Using the bare basename ([`ref_name`]) instead made
/// `docker run nginx` resolve to a locally-present `linuxserver/nginx` — a cross-repo
/// collision. Prefer this for run/inspect resolution; `ref_name` remains only for loose display uses.
pub(crate) fn ref_repo(s: &str) -> String {
    let r = ImageRef::parse(s);
    format!("{}/{}", r.registry, r.repository)
}

/// A fresh container/exec id with real entropy, shaped exactly like Docker's: 32 random bytes
/// (256 bits) hex-encoded to 64 lowercase chars. Docker derives the 12-char short id that clients
/// display/round-trip from the leading bytes of this, so the short id inherits the full entropy too.
/// (The previous implementation hashed a seed to a single 64-bit value and TILED it 4x — a 64-hex
/// string with only 16 hex of real entropy, trivially collidable and visibly not a real Docker id.)
/// Reads the OS CSPRNG (`/dev/urandom`); if that is somehow unavailable it falls back to a splitmix64
/// stream seeded from the nanosecond clock, pid and a never-reset per-process counter (the `image`
/// arg only seeds this fallback), so ids stay unique even across create/rm churn.
pub(crate) fn new_id(image: &str) -> String {
    let mut buf = [0u8; 32];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .is_err()
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let mut s = nanos
            ^ (std::process::id() as u64).rotate_left(32)
            ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ md5_like(image);
        for chunk in buf.chunks_mut(8) {
            // splitmix64: distinct 64-bit output per 8-byte chunk.
            s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            for (i, b) in chunk.iter_mut().enumerate() {
                *b = (z >> (i * 8)) as u8;
            }
        }
    }
    let mut out = String::with_capacity(64);
    for b in &buf {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod id_resolution_tests {
    use super::*;

    fn img(name: &str) -> Image {
        Image {
            name: name.into(),
            ..Default::default()
        }
    }

    // a container id must be 64 hex of REAL entropy, not a 16-hex value tiled 4x.
    #[test]
    fn new_id_is_full_entropy_64_hex() {
        let a = new_id("alpine");
        assert_eq!(a.len(), 64, "docker container ids are 64 hex chars");
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "id must be lowercase hex: {a}"
        );
        // The old bug tiled one 16-hex value 4x: the first three quarters were identical. Reject that.
        let (q0, q1, q2) = (&a[0..16], &a[16..32], &a[32..48]);
        assert!(!(q0 == q1 && q1 == q2), "id looks tiled (low entropy): {a}");
        // Consecutive ids differ (no counter/clock collision).
        assert_ne!(a, new_id("alpine"));
        // The 12-char short id is also unique across many draws (entropy reaches the leading bytes).
        let mut shorts = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(shorts.insert(new_id("x")[..12].to_string()));
        }
    }

    // the fully qualified repository distinguishes cross-repo basename collisions.
    #[test]
    fn ref_repo_distinguishes_cross_repo_basenames() {
        assert_eq!(ref_repo("nginx"), ref_repo("library/nginx"));
        assert_eq!(ref_repo("nginx"), ref_repo("docker.io/library/nginx:1.25"));
        assert_eq!(ref_repo("nginx"), ref_repo("nginx:latest"));
        assert_ne!(ref_repo("nginx"), ref_repo("linuxserver/nginx"));
        assert_ne!(ref_repo("nginx"), ref_repo("ghcr.io/o/nginx"));
    }

    // `docker run nginx` must NOT resolve to a locally-present `linuxserver/nginx`.
    #[test]
    fn find_image_does_not_cross_repo_collide() {
        let only_third_party = vec![img("linuxserver/nginx:latest")];
        assert!(
            find_image(&only_third_party, "nginx").is_none(),
            "bare official nginx must not resolve to linuxserver/nginx"
        );
        let both = vec![img("linuxserver/nginx:latest"), img("nginx:latest")];
        assert_eq!(find_image(&both, "nginx").unwrap().name, "nginx:latest");
        // The third-party image still resolves to itself when its full repo is named.
        assert_eq!(
            find_image(&both, "linuxserver/nginx").unwrap().name,
            "linuxserver/nginx:latest"
        );
        // library/ prefix and docker.io/ registry are equivalent to the bare name.
        assert_eq!(
            find_image(&both, "library/nginx").unwrap().name,
            "nginx:latest"
        );
    }
}
