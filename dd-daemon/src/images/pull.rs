#![allow(unused_imports, dead_code)]
//! `POST /images/create` (pull/import dispatch) + registry pull / config-refresh helpers and the
//! docker `--platform` ↔ arch mapping. The streamed pull-progress plumbing lives here too.
use super::*;
use crate::api::*;
use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::model::*;
use crate::networks::*;
use crate::registry::{layer_short, Client, Credentials, ImageRef, PullEvent};
use crate::runtime::*;
use crate::system::*;
use crate::util::*;
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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, watch, Mutex};

#[derive(Deserialize)]
pub(crate) struct ImageCreateQ {
    #[serde(rename = "fromImage")]
    from_image: Option<String>,
    #[serde(rename = "fromSrc")]
    from_src: Option<String>,
    repo: Option<String>,
    #[serde(rename = "tag")]
    tag: Option<String>,
    platform: Option<String>,
}

/// POST /images/create -- `docker pull` (when `fromImage` is set) or `docker import` (when `fromSrc`
/// is set). For a pull: if the image isn't local (for the requested platform), pull it from its
/// registry (any registry) and unpack it into a rootfs, then register it. For an import: extract the
/// rootfs tar from the request body into a new image named by `repo`.
pub(crate) async fn images_create(
    State(a): State<App>,
    Query(q): Query<ImageCreateQ>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // `docker import` routes through this same endpoint but carries `fromSrc` (the rootfs source)
    // instead of `fromImage`; dispatch it to the import path before the pull logic.
    if let Some(src) = q.from_src.clone().filter(|s| !s.is_empty()) {
        return image_import(
            a,
            q.repo.clone().unwrap_or_default(),
            q.tag.clone().unwrap_or_default(),
            &src,
            body,
        )
        .await;
    }
    let name = q.from_image.unwrap_or_default();
    let tag = q
        .tag
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "latest".into());
    if name.is_empty() {
        return bad_request("fromImage is required");
    }
    // "already local" must match the FULL reference (registry/repo:tag) AND the requested --platform arch:
    // distinct images can share a short name across registries, and arm64/amd64 of one image are distinct.
    let want = image_ref(&name, &tag).short();
    let want_arch = platform_arch(q.platform.as_deref());
    let creds = registry_auth(&headers);
    let archs = platform_archs(q.platform.as_deref());
    if a.inner
        .lock()
        .await
        .images
        .iter()
        .any(|i| repo_tag(&i.name) == want && want_arch.map_or(true, |a| docker_arch(i.arch) == a))
    {
        // The tag is already present locally, but its stored config may be stale — e.g. an
        // entry discovered from disk with the `/bin/sh` fallback, so a bare `docker run <img>` would launch
        // `/bin/sh` instead of the real ENTRYPOINT/CMD. Refresh the config from the registry (layers are
        // cached; only the manifest+config blob are re-fetched), then report the tag as up to date.
        refresh_local_config(&a, &name, &tag, want, creds, archs).await;
        return pull_progress(&name, &tag, Ok(true), "", 0);
    }
    pull_stream(a, name, tag, want, creds, archs)
}

/// Best-effort refresh of an already-cached image's stored config on a re-pull. Runs the
/// blocking registry config re-fetch off the async runtime, then replaces the matching store entry's
/// run-config in place (preserving its on-disk rootfs). A miss (rootfs absent, registry unreachable,
/// or no matching store slot) is silently ignored — the cached entry is left as-is and the pull still
/// reports the tag as up to date, so an offline `docker pull` of a present image never breaks.
async fn refresh_local_config(
    a: &App,
    name: &str,
    tag: &str,
    want: String,
    creds: Credentials,
    archs: Vec<&'static str>,
) {
    let (dir, nm, tg) = (a.images_dir.clone(), name.to_string(), tag.to_string());
    let fresh =
        tokio::task::spawn_blocking(move || refresh_image_config(&dir, &nm, &tg, creds, &archs))
            .await
            .ok()
            .flatten();
    let Some(fresh) = fresh else { return };
    let mut g = a.inner.lock().await;
    if let Some(slot) = g
        .images
        .iter_mut()
        .find(|i| repo_tag(&i.name) == want && docker_arch(i.arch) == docker_arch(fresh.arch))
    {
        *slot = fresh;
    }
}

/// Stream a fresh `docker pull` as newline-delimited JSON, flushing each status line as the download
/// proceeds (mirrors the `events.rs` streamed-body pattern). A background task drives the blocking
/// registry pull, forwarding its per-layer [`PullEvent`]s into the response body live; on completion it
/// registers the image and emits the closing `Digest:`/`Status:` lines (or an error line). This replaces
/// the old "block until done, then dump a fixed sequence" behavior so the client renders moving bars.
fn pull_stream(
    a: App,
    name: String,
    tag: String,
    want: String,
    creds: Credentials,
    archs: Vec<&'static str>,
) -> Response {
    // Lines flow out through `line_rx`; the body stream just drains it (closed when the worker drops tx).
    // An awaited `send` gives natural backpressure (a slow/stalled client throttles the producer rather
    // than silently dropping lines); a send error just means the client hung up — stop quietly.
    let (line_tx, line_rx) = mpsc::channel::<Vec<u8>>(256);
    tokio::spawn(async move {
        macro_rules! emit {
            ($v:expr) => {
                if line_tx
                    .send(($v.to_string() + "\r\n").into_bytes())
                    .await
                    .is_err()
                {
                    return;
                }
            };
        }
        let repo = image_ref(&name, &tag).repository;
        emit!(json!({ "status": format!("Pulling from {repo}"), "id": tag }));
        // The blocking pull reports progress over `pev`; forward+format each event into a status line.
        let (pev_tx, mut pev_rx) = mpsc::channel::<PullEvent>(256);
        let (dir, nm, tg) = (a.images_dir.clone(), name.clone(), tag.clone());
        let blocking = tokio::task::spawn_blocking(move || {
            let mut cb = |e: PullEvent| {
                let _ = pev_tx.blocking_send(e);
            };
            pull_image(&dir, &nm, &tg, creds, &archs, &mut cb)
        });
        while let Some(e) = pev_rx.recv().await {
            emit!(pull_event_json(&e));
        }
        let res = blocking
            .await
            .unwrap_or_else(|e| Err(format!("pull task crashed: {e}")));
        match res {
            Ok(img) => {
                let digest = format!("sha256:{}", fake_id(&img.name));
                {
                    let mut g = a.inner.lock().await;
                    g.images.retain(|i| repo_tag(&i.name) != want); // a re-pull (new platform) replaces the old
                    g.images.push(img);
                }
                crate::events::emit_event(&a.events, "image", "pull", &want, json!({"name": want}));
                emit!(json!({ "status": format!("Digest: {digest}") }));
                emit!(
                    json!({ "status": format!("Status: Downloaded newer image for {name}:{tag}") })
                );
            }
            Err(e) => emit!(json!({ "errorDetail": { "message": e.clone() }, "error": e })),
        }
    });
    let body = futures_util::stream::unfold(line_rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|b| (Ok::<Vec<u8>, std::io::Error>(b), rx))
    });
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from_stream(body))
        .unwrap()
}

/// Format one live [`PullEvent`] into the docker-shaped JSON status object the CLI renders as a bar.
fn pull_event_json(e: &PullEvent) -> Value {
    match e {
        PullEvent::Layer { id } => json!({ "status": "Pulling fs layer", "id": id }),
        PullEvent::Downloading { id, current, total } => {
            json!({ "status": "Downloading", "progressDetail": { "current": current, "total": total }, "id": id })
        }
        PullEvent::DownloadComplete { id } => json!({ "status": "Download complete", "id": id }),
        PullEvent::Extracting { id, current, total } => {
            json!({ "status": "Extracting", "progressDetail": { "current": current, "total": total }, "id": id })
        }
        PullEvent::PullComplete { id } => json!({ "status": "Pull complete", "id": id }),
    }
}

/// Decode the CLI's `X-Registry-Auth` header (base64 JSON credentials) into [`Credentials`].
pub(crate) fn registry_auth(headers: &axum::http::HeaderMap) -> Credentials {
    headers
        .get("X-Registry-Auth")
        .and_then(|v| v.to_str().ok())
        .and_then(Credentials::from_x_registry_auth)
        .unwrap_or_default()
}

/// docker-style pull progress: a newline-delimited stream of JSON status lines the CLI renders.
///
/// `digest`/`size` describe the pulled image (its synthetic content digest and on-disk rootfs size);
/// they drive a docker-shaped per-layer progress sequence on a fresh pull. dd squashes an image to a
/// single rootfs, so we surface ONE synthetic layer (id = first 12 hex of the digest) rather than the
/// registry's real per-blob layers. See the registry note in the push helper for what real byte
/// progress would require.
pub(crate) fn pull_progress(
    name: &str,
    tag: &str,
    result: Result<bool, String>,
    digest: &str,
    size: i64,
) -> Response {
    let body = match result {
        Ok(true) => format!(
            "{}\r\n",
            json!({ "status": format!("Status: Image is up to date for {name}:{tag}") })
        ),
        Ok(false) => {
            let repo = image_ref(name, tag).repository;
            let layer_id = digest
                .trim_start_matches("sha256:")
                .chars()
                .take(12)
                .collect::<String>();
            let layer = layer_id.as_str();
            let half = (size / 2).max(0);
            [
                json!({ "status": format!("Pulling from {repo}"), "id": tag }).to_string(),
                json!({ "status": "Pulling fs layer", "id": layer }).to_string(),
                json!({ "status": "Downloading", "progressDetail": { "current": half, "total": size }, "id": layer }).to_string(),
                json!({ "status": "Downloading", "progressDetail": { "current": size, "total": size }, "id": layer }).to_string(),
                json!({ "status": "Verifying Checksum", "id": layer }).to_string(),
                json!({ "status": "Download complete", "id": layer }).to_string(),
                json!({ "status": "Extracting", "progressDetail": { "current": size, "total": size }, "id": layer }).to_string(),
                json!({ "status": "Pull complete", "id": layer }).to_string(),
                json!({ "status": format!("Digest: {digest}") }).to_string(),
                json!({ "status": format!("Status: Downloaded newer image for {name}:{tag}") }).to_string(),
            ].join("\r\n") + "\r\n"
        }
        Err(e) => {
            json!({ "errorDetail": { "message": e.clone() }, "error": e }).to_string() + "\r\n"
        }
    };
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}

/// Map a dd-images (runtime-agnostic) target arch onto the runtime's guest personality.
pub(crate) fn guest_of(a: dd_images::Arch) -> Guest {
    match a {
        dd_images::Arch::LinuxAarch64 => Guest::LinuxAarch64,
        dd_images::Arch::LinuxX86_64 => Guest::LinuxX86_64,
        dd_images::Arch::DarwinAarch64 => Guest::DarwinAarch64,
    }
}

/// The image config's declared guest arch, if recognizable (dd-images detection mapped to a `Guest`).
pub(crate) fn manifest_arch(config: &Value) -> Option<Guest> {
    dd_images::arch_from_config(config).map(guest_of)
}

/// Pull an image from its registry (any registry) and unpack it under `<images_dir>/<safe>/rootfs`,
/// preferring the linux/arm64 variant (native; falls back to amd64). Returns the registered [`Image`].
pub(crate) fn pull_image(
    images_dir: &str,
    from_image: &str,
    tag: &str,
    creds: Credentials,
    archs: &[&str],
    progress: &mut dyn FnMut(PullEvent),
) -> Result<Image, String> {
    // dd-images owns the pull + rootfs unpack; the daemon just maps the result onto its Docker model.
    let li = dd_images::Store::new(images_dir).pull_archs(from_image, tag, creds, archs, progress)?;
    Ok(image_from_config(images_dir, &li.iref, &li.config, &li.rootfs))
}

/// Re-fetch an already-cached image's run config from its registry and rebuild its [`Image`] WITHOUT
/// re-downloading the layers. `docker pull <img>:<tag>` on a tag that is already present
/// must still refresh the stored Entrypoint/Cmd/Env/WorkingDir — otherwise a stale store entry (e.g.
/// one discovered from disk with the `/bin/sh` fallback) makes a later bare `docker run <img>` launch
/// `/bin/sh` instead of the image's real entrypoint. Best-effort: returns `None` when the rootfs isn't
/// actually on disk (let a real pull handle it) or the registry is unreachable (keep the cached entry).
pub(crate) fn refresh_image_config(
    images_dir: &str,
    from_image: &str,
    tag: &str,
    creds: Credentials,
    archs: &[&str],
) -> Option<Image> {
    let iref = image_ref(from_image, tag);
    let rootfs = std::path::PathBuf::from(format!("{images_dir}/{}/rootfs", safe_name(&iref)));
    if !rootfs.is_dir() {
        return None;
    }
    let config = Client::new(iref.clone(), creds).fetch_config(archs).ok()?;
    // A registry that answered but gave no `config` object tells us nothing to refresh with — don't
    // clobber the cached entry with an empty config.
    if !config.get("config").map(|c| c.is_object()).unwrap_or(false) {
        return None;
    }
    Some(image_from_config(images_dir, &iref, &config, &rootfs))
}

/// Build a registered [`Image`] from a freshly-fetched OCI config blob + an already-unpacked rootfs,
/// and (re)write the `dd-image.json` sidecar so the config survives a daemon restart. Shared by
/// `pull_image` (fresh pull) and `refresh_image_config` (re-pull of a cached tag) so both derive
/// Cmd/Entrypoint/Env/WorkingDir/User/etc. identically.
pub(crate) fn image_from_config(
    images_dir: &str,
    iref: &ImageRef,
    config: &Value,
    rootfs: &std::path::Path,
) -> Image {
    // Distroless/scratch images carry no ELF/Mach-O to sniff, so the rootfs scan comes up empty.
    // Fall back to the config's `architecture`+`os`, then to native arm64 — never fail on undetectable arch.
    let arch = detect_arch(rootfs)
        .or_else(|| manifest_arch(config))
        .unwrap_or(Guest::LinuxAarch64);
    let darwin = arch.os() == "darwin";
    // Keep Entrypoint and Cmd *separate* (NOT flattened like `config_cmd`) so docker's override semantics
    // survive the round-trip — `containers_create` rebuilds argv = entrypoint ++ cmd and `--entrypoint`/CMD
    // overrides act on the right half (see containers.rs).
    let entrypoint = config_strs(config, "Entrypoint");
    let env = config_strs(config, "Env");
    let workdir = config["config"]["WorkingDir"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let user = config["config"]["User"].as_str().unwrap_or("").to_string();
    let exposed_ports = config_exposed_ports(config);
    let labels = config_labels(config);
    // Lifecycle/volume image config a container inherits at run (Moby §6/§8).
    let stop_signal = config_stop_signal(config);
    let img_volumes = config_volumes(config);
    let healthcheck = config_healthcheck(config);
    // A pulled macOS image's `dd-image.json` sidecar doesn't survive the registry round-trip and its
    // userland shell lives on the in-jail PATH (`/profile/bin/bash`), not `/bin/sh` — so default a
    // darwin image to a bare `bash` (resolved via PATH by the darwinjail) rather than `/bin/sh`. Only fall
    // back when the config supplies neither Entrypoint nor Cmd (an entrypoint-only image keeps empty cmd).
    let mut cmd = config_strs(config, "Cmd");
    if cmd.is_empty() && entrypoint.is_empty() {
        cmd = if darwin {
            vec!["bash".into()]
        } else {
            default_shell(rootfs)
        };
    }
    let name = iref.short();
    // Record name + the full OCI run config (cmd/env/entrypoint/workdir, +os for darwin) so the image keeps
    // its identity AND its entrypoint/env/workdir across a daemon restart (the dir name alone doesn't
    // round-trip -- e.g. "docker.io_library_alpine_latest"). Mirrors the `docker load` path (`image_load`).
    let mut meta = json!({ "name": name.clone(), "cmd": cmd.clone(), "env": env.clone(),
                           "entrypoint": entrypoint.clone(), "workdir": workdir.clone(),
                           "user": user.clone(), "exposed_ports": exposed_ports.clone(),
                           "stop_signal": stop_signal.clone(), "img_volumes": img_volumes.clone(),
                           "healthcheck": healthcheck.clone(),
                           "arch": arch.arch(), "os": arch.os() });
    if darwin {
        meta["os"] = json!("darwin");
    }
    let _ = std::fs::write(
        format!("{images_dir}/{}/dd-image.json", safe_name(iref)),
        meta.to_string(),
    );
    Image {
        name,
        rootfs: rootfs.to_string_lossy().into_owned(),
        arch,
        cmd,
        env,
        entrypoint,
        workdir,
        user,
        exposed_ports,
        labels,
        created: now_secs(),
        stop_signal,
        img_volumes,
        healthcheck,
    }
}

/// The `config.config.Healthcheck` of an OCI image config → [`HealthConfig`]. `Test=["NONE"]` (or absent)
/// yields None — no probe. Durations are the config's nanoseconds, carried through verbatim.
pub(crate) fn config_healthcheck(config: &Value) -> Option<crate::model::HealthConfig> {
    let hc = config["config"]["Healthcheck"].as_object()?;
    let test: Vec<String> = hc
        .get("Test")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if test.is_empty() || test.first().map(|s| s.as_str()) == Some("NONE") {
        return None;
    }
    let num = |k: &str| hc.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
    Some(crate::model::HealthConfig {
        test,
        interval: num("Interval"),
        timeout: num("Timeout"),
        retries: num("Retries"),
        start_period: num("StartPeriod"),
    })
}

/// docker arch label for a guest target.
pub(crate) fn docker_arch(g: Guest) -> &'static str {
    if g.arch() == "x86_64" {
        "amd64"
    } else {
        "arm64"
    }
}

/// A docker `--platform` value ("linux/amd64", "arm64", …) mapped to dd's arch label, if recognized.
pub(crate) fn platform_arch(platform: Option<&str>) -> Option<&'static str> {
    match platform?.rsplit('/').next().unwrap_or("") {
        "amd64" | "x86_64" => Some("amd64"),
        "arm64" | "aarch64" => Some("arm64"),
        _ => None,
    }
}

/// Preferred arch list when pulling for a given platform: the requested one, else native-arm64 first.
pub(crate) fn platform_archs(platform: Option<&str>) -> Vec<&'static str> {
    match platform_arch(platform) {
        Some(a) => vec![a],
        None => vec!["arm64", "amd64"],
    }
}

#[cfg(test)]
mod refresh_tests {
    use super::*;
    use serde_json::json;

    // a re-pull of an already-present tag must refresh the stored config so `docker run` uses the
    // image's real Entrypoint/Cmd/Env/WorkingDir, NOT the `/bin/sh` fallback a stale/discovered entry
    // carries. `image_from_config` is the shared rebuilder both the fresh pull and the refresh use.
    #[test]
    fn image_from_config_uses_real_entrypoint_not_bin_sh() {
        let dir =
            std::env::temp_dir().join(format!("dd-refresh-{}-{}", std::process::id(), now_nanos()));
        let iref = ImageRef::parse("nginx:latest");
        let img_dir = dir.join(safe_name(&iref));
        let rootfs = img_dir.join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let config = json!({
            "architecture": "arm64", "os": "linux",
            "config": {
                "Entrypoint": ["/docker-entrypoint.sh"],
                "Cmd": ["nginx", "-g", "daemon off;"],
                "Env": ["PATH=/usr/local/sbin:/usr/bin", "NGINX_VERSION=1.25.4"],
                "WorkingDir": "/work",
            }
        });

        let image = image_from_config(dir.to_str().unwrap(), &iref, &config, &rootfs);

        assert_eq!(image.name, "nginx:latest");
        assert_eq!(image.entrypoint, vec!["/docker-entrypoint.sh"]);
        assert_eq!(image.cmd, vec!["nginx", "-g", "daemon off;"]);
        assert!(image.env.iter().any(|e| e == "NGINX_VERSION=1.25.4"));
        assert_eq!(image.workdir, "/work");
        // The critical assertion: the run command is the image's, not the /bin/sh fallback.
        assert!(
            !image.cmd.iter().any(|c| c == "/bin/sh"),
            "refresh must not fall back to /bin/sh"
        );
        // The refreshed config is persisted to the sidecar so it survives a daemon restart.
        let side = std::fs::read_to_string(img_dir.join("dd-image.json")).unwrap();
        assert!(side.contains("docker-entrypoint.sh") && side.contains("nginx"));

        std::fs::remove_dir_all(&dir).ok();
    }

    // An entrypoint-only image (no Cmd) keeps an empty Cmd rather than injecting /bin/sh.
    #[test]
    fn image_from_config_entrypoint_only_keeps_empty_cmd() {
        let dir = std::env::temp_dir().join(format!(
            "dd-refresh2-{}-{}",
            std::process::id(),
            now_nanos()
        ));
        let iref = ImageRef::parse("busybox:latest");
        let rootfs = dir.join(safe_name(&iref)).join("rootfs");
        std::fs::create_dir_all(&rootfs).unwrap();
        let config = json!({ "architecture": "arm64", "os": "linux",
                             "config": { "Entrypoint": ["/bin/busybox"] } });
        let image = image_from_config(dir.to_str().unwrap(), &iref, &config, &rootfs);
        assert_eq!(image.entrypoint, vec!["/bin/busybox"]);
        assert!(
            image.cmd.is_empty(),
            "entrypoint-only image must keep Cmd empty"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
