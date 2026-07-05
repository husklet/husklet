#![allow(unused_imports, dead_code)]
//! Archive + registry transfer: `docker push` / `docker save` / `docker load` / `docker import`.
//!
//! dd's archive format is intentionally simple (not full OCI): a tar whose top level is the image's
//! `rootfs/` directory plus a `dd-manifest.json` sidecar recording the image identity (name + run
//! config). `docker save` produces it, `docker load` consumes it; `docker import` instead takes a
//! bare rootfs tar (no manifest) whose files land directly in a new image's rootfs.
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
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ddjit::Guest;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// POST /images/:name/push -- re-tar the local rootfs into a single-layer image and upload it to its
/// registry (`docker.io/...`, `ghcr.io/...`, `localhost:5000/...`) using the CLI's credentials.
pub(crate) async fn image_push(
    State(a): State<App>,
    Path(name): Path<String>,
    Query(q): Query<PushQ>,
    headers: axum::http::HeaderMap,
) -> Response {
    // The route `name` is collapsed to the bare image (e.g. `huttarichard/ddmac` -> `ddmac`), so match on
    // it AND the requested tag, then push to the image's FULL stored name so the registry namespace
    // (`huttarichard/…`) is preserved — otherwise the upload targets `library/<name>` and is denied.
    let want_tag = q
        .tag
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "latest".into());
    let img = {
        let g = a.inner.lock().await;
        g.images
            .iter()
            .find(|i| ref_name(&i.name) == ref_name(&name) && ref_tag(&i.name) == want_tag)
            .or_else(|| {
                g.images
                    .iter()
                    .find(|i| ref_name(&i.name) == ref_name(&name))
            })
            .cloned()
    };
    let Some(img) = img else {
        return push_progress(&name, &want_tag, 0, Err(format!("No such image: {name}")))
            .into_response();
    };
    let tag = want_tag;
    let iref = image_ref(&img.name, &tag);
    let arch = docker_arch(img.arch).to_string();
    let os = img.arch.os().to_string(); // "darwin" for mac images, else "linux"
    let creds = registry_auth(&headers);
    // On-disk rootfs size, captured before `img` is moved into the push task; reported as the layer
    // `Size` in the push progress/aux lines (a real registry manifest size would need registry.rs to
    // surface it — see note below).
    let size = image_size(&img.rootfs, &img.name);
    let work = std::path::PathBuf::from(format!("{}/.push-{}", a.images_dir, std::process::id()));
    let res = tokio::task::spawn_blocking(move || {
        Client::new(iref, creds).push(
            std::path::Path::new(&img.rootfs),
            &img.cmd,
            &arch,
            &os,
            &work,
        )
    })
    .await
    .unwrap_or_else(|e| Err(format!("push task crashed: {e}")));
    push_progress(&name, &tag, size, res).into_response()
}

#[derive(Deserialize)]
pub(crate) struct PushQ {
    tag: Option<String>,
}

/// docker-style push progress: a newline-delimited stream of JSON status lines (or an error line).
///
/// `digest` is the manifest digest returned by `Client::push` (the registry's `Docker-Content-Digest`),
/// `size` is the image's on-disk rootfs size used as the layer/aux `Size`. The stream ends with the
/// `aux` line (which the docker CLI parses to print `digest: … size: …`) followed by the matching
/// status line, so `docker push` reports the real pushed digest instead of a hardcoded `latest:`.
///
/// REPORT-only: a fully accurate `Size` would be the manifest byte length, not the rootfs size. The
/// registry client computes both the layer size and the manifest bytes internally; if `Client::push`
/// returned `(digest, manifest_size, layer_size)` instead of just the digest, dd could emit Docker's
/// exact `size:` value and real per-blob byte progress here.
pub(crate) fn push_progress(
    name: &str,
    tag: &str,
    size: i64,
    result: Result<String, String>,
) -> Response {
    let body = match result {
        Ok(digest) => {
            let layer_id = digest
                .trim_start_matches("sha256:")
                .chars()
                .take(12)
                .collect::<String>();
            let half = (size / 2).max(0);
            let status = |s: String| StreamStatus {
                status: s,
                progress_detail: None,
                id: None,
            };
            let pushing = |current: i64| StreamStatus {
                status: "Pushing".into(),
                progress_detail: Some(ProgressDetail { current, total: size }),
                id: Some(layer_id.clone()),
            };
            [
                serde_json::to_string(&status(format!("The push refers to repository [{name}]")))
                    .unwrap(),
                serde_json::to_string(&StreamStatus {
                    status: "Preparing".into(),
                    progress_detail: None,
                    id: Some(layer_id.clone()),
                })
                .unwrap(),
                serde_json::to_string(&pushing(half)).unwrap(),
                serde_json::to_string(&pushing(size)).unwrap(),
                serde_json::to_string(&StreamStatus {
                    status: "Pushed".into(),
                    progress_detail: None,
                    id: Some(layer_id.clone()),
                })
                .unwrap(),
                serde_json::to_string(&AuxLine {
                    progress_detail: Empty {},
                    aux: Aux {
                        tag: tag.to_string(),
                        digest: digest.clone(),
                        size,
                    },
                })
                .unwrap(),
                serde_json::to_string(&status(format!("{tag}: digest: {digest} size: {size}")))
                    .unwrap(),
            ]
            .join("\r\n")
                + "\r\n"
        }
        Err(e) => {
            json!({ "errorDetail": { "message": e.clone() }, "error": e }).to_string() + "\r\n"
        }
    };
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}

// ---- image save / load / import --------------------------------------------
#[derive(Deserialize)]
pub(crate) struct SaveQ {
    names: Option<String>,
}

/// GET /images/get?names=<name> -- `docker save`. Streams a tar of the image's `rootfs/` directory
/// plus a `dd-manifest.json` naming the image, as `application/x-tar`.
pub(crate) async fn image_save(State(a): State<App>, Query(q): Query<SaveQ>) -> Response {
    let names = q.names.unwrap_or_default();
    if names.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "names is required"})),
        )
            .into_response();
    }
    let img = {
        let g = a.inner.lock().await;
        g.images
            .iter()
            .find(|i| repo_tag(&i.name) == names || ref_name(&i.name) == ref_name(&names))
            .cloned()
    };
    let Some(img) = img else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("No such image: {names}")})),
        )
            .into_response();
    };
    // The `macos` image is the live host filesystem (rootfs ~ `/`); taring it would be catastrophic.
    if img.name == "macos" {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "cannot save the host `macos` image"})),
        )
            .into_response();
    }
    // dd-images owns the archive format (tar of `rootfs/` + a `dd-manifest.json` sidecar); the handler just
    // maps the store `Image` onto the manifest and streams the bytes back.
    let manifest = dd_images::Manifest {
        name: img.name.clone(),
        cmd: img.cmd.clone(),
        env: img.env.clone(),
        entrypoint: img.entrypoint.clone(),
        workdir: img.workdir.clone(),
        user: img.user.clone(),
        exposed_ports: img.exposed_ports.clone(),
        os: (img.arch.os() == "darwin").then(|| "darwin".to_string()),
        ..Default::default()
    };
    let rootfs = std::path::PathBuf::from(&img.rootfs);
    match dd_images::Store::new(&a.images_dir).save_archive(&rootfs, &manifest) {
        Ok(bytes) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/x-tar")
            .body(Body::from(bytes))
            .unwrap(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "message": e })),
        )
            .into_response(),
    }
}

/// POST /images/load -- `docker load`. Extracts a dd save archive (rootfs/ + dd-manifest.json) from
/// the request body into a new image directory and registers the image.
pub(crate) async fn image_load(State(a): State<App>, body: axum::body::Bytes) -> Response {
    // dd-images owns the archive extraction + manifest + on-disk placement (`rootfs/` under the store, a
    // `dd-image.json` sidecar so it round-trips through discovery). The handler maps the materialized
    // `LoadedImage` onto the daemon's `Image`, registers it, and emits the load event.
    let loaded = match dd_images::Store::new(&a.images_dir).load_archive(&body) {
        Ok(l) => l,
        Err(e) => return load_err(e),
    };
    let name = loaded.name.clone();
    let healthcheck = loaded
        .healthcheck
        .clone()
        .and_then(|v| serde_json::from_value::<crate::model::HealthConfig>(v).ok());
    let img = Image {
        name: name.clone(),
        rootfs: loaded.rootfs.to_string_lossy().into_owned(),
        arch: guest_of(loaded.arch),
        cmd: loaded.cmd,
        env: loaded.env,
        entrypoint: loaded.entrypoint,
        workdir: loaded.workdir,
        user: loaded.user,
        exposed_ports: loaded.exposed_ports,
        stop_signal: loaded.stop_signal,
        img_volumes: loaded.img_volumes,
        healthcheck,
        created: now_secs(),
        ..Default::default()
    };
    register_image(&a, img).await;
    crate::events::emit_event(&a.events, "image", "load", &name, json!({"name": name}));
    Json(LoadResponse {
        stream: format!("Loaded image: {}", repo_tag(&name)),
    })
    .into_response()
}

/// `docker import` -- extract a bare rootfs tar (request body) into a new image named by `repo`
/// (optionally `repo:tag`) and register it. Routed from `images_create` on `fromSrc`.
pub(crate) async fn image_import(
    a: App,
    repo: String,
    tag: String,
    src: &str,
    body: axum::body::Bytes,
) -> Response {
    if repo.is_empty() {
        return import_progress(Err("repo is required".into()));
    }
    // dd imports a rootfs tar streamed in the body (`docker import - <name>`); importing from a remote
    // URL is not supported (dd has no HTTP fetcher).
    if src != "-" {
        return import_progress(Err(format!(
            "unsupported import source {src:?}; pipe the rootfs to `-`"
        )));
    }
    let name = if tag.is_empty() {
        repo
    } else {
        format!("{repo}:{tag}")
    };
    // dd-images owns the rootfs-tar extraction into a new image dir (+ minimal `dd-image.json` sidecar);
    // the handler maps the result onto the daemon's `Image` and registers it.
    let loaded = match dd_images::Store::new(&a.images_dir).import_rootfs(&name, &body) {
        Ok(l) => l,
        Err(e) => return import_progress(Err(e)),
    };
    let img = Image {
        name: loaded.name,
        rootfs: loaded.rootfs.to_string_lossy().into_owned(),
        arch: guest_of(loaded.arch),
        cmd: loaded.cmd,
        created: now_secs(),
        ..Default::default()
    };
    register_image(&a, img).await;
    import_progress(Ok(format!("sha256:{}", fake_id(&name))))
}

/// `docker import` progress: a single JSON status line carrying the new image id, or an error line.
fn import_progress(result: Result<String, String>) -> Response {
    let body = match result {
        Ok(id) => serde_json::to_string(&ImportStatus { status: id }).unwrap() + "\r\n",
        Err(e) => {
            json!({ "errorDetail": { "message": e.clone() }, "error": e }).to_string() + "\r\n"
        }
    };
    (StatusCode::OK, [("Content-Type", "application/json")], body).into_response()
}

/// `docker load` failure -> 500 + a Docker-shaped `{"message": …}` error body.
fn load_err(msg: String) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"message": msg})),
    )
        .into_response()
}
