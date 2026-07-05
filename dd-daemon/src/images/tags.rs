#![allow(unused_imports, dead_code)]
//! In-memory store mutations: `docker tag` / `docker rmi` / on-disk dir removal + rescan/register.
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
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use ddjit::Guest;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---- image management: tag / rmi -------------------------------------------
#[derive(Deserialize)]
pub(crate) struct TagQ {
    repo: Option<String>,
    tag: Option<String>,
}

/// POST /images/:name/tag -- alias an image under a new repo[:tag] (same rootfs). Honors both the
/// `repo` and `tag` query params (`docker tag src dst:v2` -> repo=dst, tag=v2).
pub(crate) async fn image_tag(
    State(a): State<App>,
    Path(name): Path<String>,
    Query(q): Query<TagQ>,
) -> Response {
    let mut g = a.inner.lock().await;
    let Some(src) = find_image(&g.images, &name).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("No such image: {name}")})),
        )
            .into_response();
    };
    // Keep the FULL target repository (registry + namespace), e.g. `huttarichard/ddmac` — NOT the bare
    // name. Stripping it (ref_name) would later push to `library/<name>` and be denied. docker sends the
    // repo without a tag and the tag separately.
    let repo = q.repo.unwrap_or_default();
    if repo.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "repo required"})),
        )
            .into_response();
    }
    let full = match q.tag.filter(|t| !t.is_empty()) {
        Some(t) => format!("{repo}:{t}"),
        None => repo,
    };
    if !g.images.iter().any(|i| i.name == full) {
        g.images.push(Image {
            name: full.clone(),
            ..src
        });
    }
    crate::events::emit_event(&a.events, "image", "tag", &full, json!({"name": full}));
    StatusCode::CREATED.into_response()
}

/// DELETE /images/:name -- `docker rmi`. Tag-precise, matching Docker semantics: `rmi <name>:<tag>`
/// (or bare `<name>`, which means `<name>:latest`) removes ONLY that one tag entry from the store. The
/// on-disk rootfs is deleted only when this was its LAST reference; if another tag (a `docker tag` alias)
/// still points at the same rootfs we just drop the tag (an untag) and keep the layers. So `rmi ubuntu`
/// with `ubuntu:24.04` also present untags only `ubuntu:latest` and leaves `ubuntu:24.04` resolvable.
pub(crate) async fn image_delete(State(a): State<App>, Path(name): Path<String>) -> Response {
    let mut g = a.inner.lock().await;
    let (want_repo, want_tag) = (ref_name(&name).to_string(), ref_tag(&name));
    // The single tag entry the reference names (repository AND tag must match). `ref_name`/`ref_tag`
    // mirror the lenient matching used elsewhere (registry/namespace ignored).
    let matches = |i: &Image| ref_name(&i.name) == want_repo && ref_tag(&i.name) == want_tag;
    let Some(target) = g.images.iter().find(|i| matches(i)).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"message": format!("No such image: {name}")})),
        )
            .into_response();
    };
    let untagged = repo_tag(&target.name);
    g.images.retain(|i| !matches(i)); // remove only this tag, never sibling tags of the same repo
                                      // Delete the on-disk rootfs only when this was its last reference: another tag sharing the same
                                      // rootfs (a `docker tag` alias) keeps it alive, so we report an untag and leave the layers in place.
    let last_ref = !g.images.iter().any(|i| i.rootfs == target.rootfs);
    let mut report = vec![DeleteRecord::Untagged(untagged)];
    if last_ref && target.name != "macos" {
        // the host `macos` image's rootfs is the live `/` — never delete. dd-images owns the store-guarded
        // dir removal (a rootfs outside the writable store — a bundled starter — is left untouched).
        dd_images::Store::new(&a.images_dir).remove_image_dir(&target.rootfs);
        report.push(DeleteRecord::Deleted(format!(
            "sha256:{}",
            fake_id(&target.name)
        )));
    }
    crate::events::emit_event(
        &a.events,
        "image",
        "delete",
        &want_repo,
        json!({"name": repo_tag(&target.name)}),
    );
    Json(report).into_response()
}

/// Re-scan the writable images dir from disk and merge any images not already in the in-memory store
/// (keyed by `repository:tag`). A safety net for a lookup miss: an image whose rootfs + `dd-image.json`
/// exist on disk but isn't registered in memory (e.g. pulled/built by another daemon process, or dropped
/// in out-of-band) becomes visible without a daemon restart. Returns true if anything new was added.
pub(crate) async fn rescan_images(a: &App) -> bool {
    let dir = a.images_dir.clone();
    let found = tokio::task::spawn_blocking(move || discover_images(&dir))
        .await
        .unwrap_or_default();
    let mut g = a.inner.lock().await;
    let mut added = false;
    for img in found {
        let tag = repo_tag(&img.name);
        if !g.images.iter().any(|i| repo_tag(&i.name) == tag) {
            g.images.push(img);
            added = true;
        }
    }
    added
}

/// Register a freshly load/import-ed image in the daemon's in-memory state, replacing any existing
/// image sharing the same `repository:tag` (mirrors the re-pull dedupe in `images_create`).
pub(crate) async fn register_image(a: &App, img: Image) {
    let mut g = a.inner.lock().await;
    let tag = repo_tag(&img.name);
    g.images.retain(|i| repo_tag(&i.name) != tag);
    g.images.push(img);
}
