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
            // default), so it must be present; dd has no parent/registry-digest/shared-size accounting yet,
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

/// `GET /images/{name}/history` — `docker history`. dd squashes images to a single rootfs, so we
/// report one synthetic layer.
pub(crate) async fn image_history(State(a): State<App>, Path(name): Path<String>) -> Response {
    let g = a.inner.lock().await;
    match find_image(&g.images, &name) {
        Some(i) => Json(vec![HistoryLayer {
            id: image_id(i),
            created: i.created,
            created_by: "dd import",
            tags: vec![repo_tag(&i.name)],
            size: image_size(&i.rootfs, &i.name),
            comment: "",
        }])
        .into_response(),
        None => no_such_image(&name),
    }
}

/// `GET /images/search` — `docker search`. dd has no search index; return an empty result set with
/// the correct shape rather than 404.
pub(crate) async fn image_search() -> Json<Vec<Value>> {
    Json(vec![])
}

/// `POST /images/prune` — `docker image prune`. Reclaims DANGLING images: those with no repository tag
/// (a `docker commit` with no repo, or an untagged leftover) that no container still references by rootfs.
/// Deletes their on-disk store dir and reports each as an untagged/deleted record (docker parity). The
/// default (`dangling=true`) semantics — untagged-only — is what dd tracks.
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
            let _ = dd_images::Store::new(&a.images_dir).remove_image_dir(&img.rootfs);
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

/// `GET /distribution/{name}/json` — registry manifest probe. Minimal conformant descriptor.
pub(crate) async fn distribution_inspect(Path(name): Path<String>) -> Response {
    Json(DistributionInspect {
        descriptor: Descriptor {
            media_type: "application/vnd.docker.distribution.manifest.v2+json",
            digest: format!("sha256:{}", fake_id(&name)),
            size: 0,
        },
        platforms: vec![PlatformDesc {
            architecture: "arm64",
            os: "linux",
        }],
    })
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
