//! `POST /images/create` (pull/import dispatch) + registry pull / config-refresh helpers and the
//! docker `--platform` ↔ arch mapping. The streamed pull-progress plumbing lives here too.
//!
//! Decomposed by concern:
//! - `stream` — streamed pull-progress plumbing (`pull_stream`/`pull_event_json`/`pull_progress`/`registry_auth`).
//! - `arch`   — docker `--platform` ↔ dd-arch mapping (`guest_of`/`manifest_arch`/`docker_arch`/`platform_arch(s)`).
//! - `config` — registry config-refresh / image-build helpers (`pull_image`/`refresh_*`/`image_from_config`).
use super::*;
use crate::model::*;
use crate::util::*;
use crate::prelude::*;

mod arch;
mod config;
mod stream;

pub(crate) use {arch::*, config::*, stream::*};

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
