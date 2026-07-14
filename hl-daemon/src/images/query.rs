//! Read/report image handlers: list, history, search, prune, distribution probe, inspect.
use super::*;
use crate::api::*;
use crate::model::*;
use crate::util::*;
use crate::prelude::*;

pub(crate) async fn images_json(State(a): State<App>) -> Json<Vec<ImageSummary>> {
    let imgs: Vec<ImageSummary> = a
        .inner
        .lock()
        .await
        .images
        .iter()
        .map(|i| {
            let size = image_size(&i.rootfs, &i.name);
            // Fields required by the Docker `ImageSummary` schema (strict clients like bollard reject the
            // object if any are absent). `VirtualSize` is a required i64 in API <=1.43 models (no serde
            // default), so it must be present; hl has no parent/registry-digest/shared-size accounting yet,
            // so the rest take the Docker "not calculated" sentinels (-1) or empties.
            ImageSummary {
                id: image_id(i),
                repo_tags: vec![repo_tag(&i.name)],
                created: i.created,
                size,
                virtual_size: size,
                parent_id: "",
                repo_digests: vec![],
                shared_size: -1,
                labels: i.labels.clone(),
                containers: -1,
            }
        })
        .collect();
    Json(imgs)
}

/// `GET /images/{name}/history` — `docker history`. A built image reports one row per Dockerfile
/// instruction (persisted at build time); a pulled/imported image with no recorded history reports a
/// single synthetic row. Rows are newest-first (Docker order): only the top row carries the image id,
/// tags and total size; older rows are `<missing>` with per-instruction `created_by`/`empty_layer`.
pub(crate) async fn image_history(State(a): State<App>, Path(name): Path<String>) -> Response {
    let g = a.inner.lock().await;
    match find_image(&g.images, &name) {
        Some(i) => {
            let total = image_size(&i.rootfs, &i.name);
            if i.history.is_empty() {
                return Json(vec![HistoryLayer {
                    id: image_id(i),
                    created: i.created,
                    created_by: "hl import".to_string(),
                    tags: vec![repo_tag(&i.name)],
                    size: total,
                    comment: "",
                    empty_layer: false,
                }])
                .into_response();
            }
            let rows: Vec<HistoryLayer> = i
                .history
                .iter()
                .rev() // newest instruction first, matching `docker history`
                .enumerate()
                .map(|(pos, h)| HistoryLayer {
                    id: if pos == 0 { image_id(i) } else { "<missing>".to_string() },
                    created: h.created,
                    created_by: h.created_by.clone(),
                    tags: if pos == 0 { vec![repo_tag(&i.name)] } else { vec![] },
                    // hl squashes to one rootfs, so report the whole size on the top row only.
                    size: if pos == 0 && !h.empty_layer { total } else { 0 },
                    comment: "",
                    empty_layer: h.empty_layer,
                })
                .collect();
            Json(rows).into_response()
        }
        None => no_such_image(&name),
    }
}

/// `GET /images/search` — `docker search`. hl has no search index; return an empty result set with
/// the correct shape rather than 404.
pub(crate) async fn image_search() -> Json<Vec<Value>> {
    Json(vec![])
}

/// `POST /images/prune` — `docker image prune`. Reclaims DANGLING images: those with no repository tag
/// (a `docker commit` with no repo, or an untagged leftover) that no container still references by rootfs.
/// Deletes their on-disk store dir and reports each as an untagged/deleted record (docker parity). The
/// default (`dangling=true`) semantics — untagged-only — is what hl tracks.
pub(crate) async fn images_prune(State(a): State<App>) -> Json<PruneReport> {
    let mut g = a.inner.lock().await;
    // A dangling image has an empty (untagged) name; keep it only if a container still points at its rootfs.
    let dangling: Vec<Image> = g
        .images
        .iter()
        .filter(|i| i.name.is_empty() && !i.name.eq("macos"))
        .filter(|i| !g.containers.values().any(|c| c.rootfs == i.rootfs))
        .cloned()
        .collect();
    let mut deleted: Vec<Value> = Vec::new();
    for img in &dangling {
        // Only delete the rootfs when no TAGGED image shares it (a tagged sibling keeps the layers alive).
        let shared = g
            .images
            .iter()
            .any(|i| i.rootfs == img.rootfs && !i.name.is_empty());
        if !shared {
            let _ = hl_images::Store::new(&a.images_dir).remove_image_dir(&img.rootfs);
            deleted.push(json!({ "Deleted": image_id(img) }));
        }
        deleted.push(json!({ "Untagged": image_id(img) }));
    }
    let dangling_rootfs: Vec<String> = dangling.iter().map(|i| i.rootfs.clone()).collect();
    g.images
        .retain(|i| !(i.name.is_empty() && dangling_rootfs.contains(&i.rootfs)));
    if !deleted.is_empty() {
        save_state(&g, &a.state_path);
    }
    Json(PruneReport {
        images_deleted: deleted,
        space_reclaimed: 0,
    })
}

/// `GET /distribution/{name}/json` — `docker manifest inspect` / `buildx imagetools inspect`. This
/// endpoint resolves the image's manifest DESCRIPTOR (real content digest + size + platforms) from the
/// registry. hl does not perform remote manifest resolution and stores no registry digest locally, so it
/// cannot produce a truthful descriptor. Returning an invented `sha256:<hash-of-name>` with size 0 (the
/// former behavior) misleads any client that trusts the digest, so return an honest Docker-shaped 404
/// instead — never fabricated metadata.
pub(crate) async fn distribution_inspect(Path(name): Path<String>) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorMessage {
            message: format!(
                "no distribution descriptor for {name}: hl does not resolve remote registry manifests"
            ),
        }),
    )
        .into_response()
}

/// GET /images/:name/json — `docker image inspect` / `docker run`'s local-image probe. Returns the
/// image config (Cmd/Entrypoint/Env) so the CLI doesn't treat the image as missing and re-pull.
pub(crate) async fn image_inspect(State(a): State<App>, Path(name): Path<String>) -> Response {
    // On a miss, re-scan the images dir from disk before reporting 404: the image may be on disk
    // (freshly pulled/built) yet absent from the in-memory store.
    if find_image(&a.inner.lock().await.images, &name).is_none() {
        rescan_images(&a).await;
    }
    let g = a.inner.lock().await;
    match find_image(&g.images, &name) {
        Some(i) => {
            let tag = repo_tag(&i.name);
            let size = image_size(&i.rootfs, &i.name);
            // The image stores ENTRYPOINT separately; Docker reports a missing entrypoint as null
            // (not []), and `docker inspect` clients distinguish the two.
            let entrypoint = if i.entrypoint.is_empty() {
                None
            } else {
                Some(i.entrypoint.clone())
            };
            // Use the image's recorded ENV; fall back to a sane PATH so containers run by a client
            // that copies Config.Env verbatim still resolve binaries.
            let env: Vec<String> = if i.env.is_empty() {
                vec!["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".into()]
            } else {
                i.env.clone()
            };
            Json(ImageInspect {
                id: image_id(i),
                repo_tags: vec![tag.clone()],
                repo_digests: vec![],
                architecture: docker_arch(i.arch).to_string(),
                os: i.arch.os().to_string(),
                size,
                virtual_size: size,
                // RFC3339 string shape strict clients (bollard) expect; `created` is unix secs.
                created: fmt_rfc3339(i.created),
                config: ImageConfig {
                    image: tag,
                    cmd: i.cmd.clone(),
                    entrypoint,
                    env,
                    working_dir: i.workdir.clone(),
                    user: i.user.clone(),
                    // OCI stores ExposedPorts as a set; re-materialize `{ "5432/tcp": {} }` for inspect.
                    exposed_ports: i.exposed_ports.iter().map(|p| (p.clone(), Empty {})).collect(),
                    labels: i.labels.clone(),
                    // Lifecycle/volume config docker clients diff (StopSignal null when unset; Volumes a
                    // set of dirs; Healthcheck the probe or null).
                    stop_signal: if i.stop_signal.is_empty() {
                        None
                    } else {
                        Some(i.stop_signal.clone())
                    },
                    volumes: i.img_volumes.iter().map(|p| (p.clone(), Empty {})).collect(),
                    healthcheck: i.healthcheck.clone(),
                },
                root_fs: RootFs {
                    type_: "layers",
                    layers: vec![],
                },
            })
            .into_response()
        }
        None => no_such_image(&name),
    }
}
