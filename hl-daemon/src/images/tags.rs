//! In-memory store mutations: `docker tag` / `docker rmi` / on-disk dir removal + rescan/register.
use super::*;
use crate::api::*;
use crate::model::*;
use crate::util::*;
use crate::prelude::*;

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
        return no_such_image(&name);
    };
    // Keep the FULL target repository (registry + namespace), e.g. `huttarichard/ddmac` — NOT the bare
    // name. Stripping it (ref_name) would later push to `library/<name>` and be denied. docker sends the
    // repo without a tag and the tag separately.
    let repo = q.repo.unwrap_or_default();
    if repo.is_empty() {
        return bad_request("repo required");
    }
    let full = match q.tag.filter(|t| !t.is_empty()) {
        Some(t) => format!("{repo}:{t}"),
        None => repo,
    };
    apply_tag(&mut g.images, &src, &full);
    // Persist the alias so it SURVIVES a daemon restart / rediscovery: tags live only in memory otherwise,
    // and discovery rebuilds images from on-disk rootfs dirs (which carry only the canonical name), so a
    // `docker tag` alias would silently vanish on restart. `persist_tag_alias` records alias -> rootfs.
    crate::util::persist_tag_alias(&a.images_dir, &full, &src.rootfs);
    crate::events::emit_event(&a.events, "image", "tag", &full, json!({"name": full}));
    StatusCode::CREATED.into_response()
}

/// Apply `docker tag`: make `full` (dest `repo:tag`) reference `src`'s content. If `full` already exists,
/// REPOINT it at the source (Docker replaces the destination mapping). The old code skipped the write when
/// the dest tag was present, so a retag was a silent no-op and clients kept running/pushing the STALE
/// rootfs the tag used to point at.
pub(crate) fn apply_tag(images: &mut Vec<Image>, src: &Image, full: &str) {
    let tagged = Image {
        name: full.to_string(),
        ..src.clone()
    };
    if let Some(existing) = images.iter_mut().find(|i| i.name == full) {
        *existing = tagged;
    } else {
        images.push(tagged);
    }
}

/// DELETE /images/:name -- `docker rmi`. Tag-precise, matching Docker semantics: `rmi <name>:<tag>`
/// (or bare `<name>`, which means `<name>:latest`) removes ONLY that one tag entry from the store. The
/// on-disk rootfs is deleted only when this was its LAST reference; if another tag (a `docker tag` alias)
/// still points at the same rootfs we just drop the tag (an untag) and keep the layers. So `rmi ubuntu`
/// with `ubuntu:24.04` also present untags only `ubuntu:latest` and leaves `ubuntu:24.04` resolvable.
#[derive(Deserialize, Default)]
pub(crate) struct RmiQ {
    force: Option<String>,
}

pub(crate) async fn image_delete(
    State(a): State<App>,
    Path(name): Path<String>,
    Query(q): Query<RmiQ>,
) -> Response {
    let mut g = a.inner.lock().await;
    let force = matches!(q.force.as_deref(), Some("1") | Some("true"));
    let (want_repo, want_tag) = (ref_repo(&name), ref_tag(&name));
    // The single tag entry the reference names (repository AND tag must match). Match on the FULLY-
    // QUALIFIED repository (`ref_repo`), NOT the bare basename: `rmi nginx` must not delete an unrelated
    // `linuxserver/nginx:latest` that merely shares the final path segment.
    let matches = |i: &Image| ref_repo(&i.name) == want_repo && ref_tag(&i.name) == want_tag;
    let Some(target) = g.images.iter().find(|i| matches(i)).cloned() else {
        return no_such_image(&name);
    };
    // Whether ANY container is still backed by this rootfs. Keyed on the resolved `rootfs`, NOT the tag
    // being deleted: a container created through an OLDER alias (`c.image = "old"`) still references the
    // same storage even when the last surviving tag is `new`, so a tag-only comparison would wrongly let
    // the store be deleted out from under it.
    let container_uses_rootfs = g.containers.values().any(|c| c.rootfs == target.rootfs);
    // If this is the LAST tag of the underlying rootfs (deleting it removes the image itself), a container
    // still backed by that rootfs makes `rmi` a 409 unless forced — docker refuses to delete an image in
    // use. A non-last tag just untags (rootfs kept alive by a sibling) and is allowed.
    let would_be_last = g.images.iter().filter(|i| i.rootfs == target.rootfs).count() == 1;
    if would_be_last && !force && target.name != "macos" && container_uses_rootfs {
        let cid = g
            .containers
            .values()
            .find(|c| c.rootfs == target.rootfs)
            .map(|c| c.id[..c.id.len().min(12)].to_string())
            .unwrap_or_default();
        return conflict(format!(
            "conflict: unable to delete {name} (must be forced) - image is being used by container {cid}"
        ));
    }
    let untagged = repo_tag(&target.name);
    // Whether removing THIS tag leaves the rootfs unreferenced (its layers should then be deleted). Computed
    // BEFORE mutating the store so a failed on-disk removal can abort WITHOUT having dropped image state.
    let last_ref = g.images.iter().filter(|i| i.rootfs == target.rootfs).count() == 1;
    let mut report = vec![DeleteRecord::Untagged(untagged)];
    // Delete the backing store only when this was the last tag AND no container still uses the rootfs —
    // even a FORCED rmi must not delete storage a live container reads (docker refcounts the layers by the
    // container; dd shares the rootfs directly, so deleting it would break the container's restart/export).
    if last_ref && target.name != "macos" && !container_uses_rootfs {
        // the host `macos` image's rootfs is the live `/` — never delete. hl-images owns the store-guarded
        // dir removal (a rootfs outside the writable store — a bundled starter — is left untouched). If the
        // on-disk removal FAILS, keep the image in state (retryable) and report an error rather than
        // dropping state while the store entry lingers on disk.
        if let Err(e) = hl_images::Store::new(&a.images_dir).remove_image_dir(&target.rootfs) {
            return server_error(format!("failed to remove image store entry: {e}"));
        }
        report.push(DeleteRecord::Deleted(image_id(&target)));
    }
    g.images.retain(|i| !matches(i)); // remove only this tag, never sibling tags of the same repo
    crate::events::emit_event(
        &a.events,
        "image",
        "delete",
        ref_name(&name), // event actor keeps the short reference name, as before
        json!({"name": repo_tag(&target.name)}),
    );
    Json(report).into_response()
}

/// Re-scan the writable images dir from disk and merge any images not already in the in-memory store
/// (keyed by `repository:tag`). A safety net for a lookup miss: an image whose rootfs + `hl-image.json`
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Image;

    fn img(name: &str, rootfs: &str) -> Image {
        Image {
            name: name.into(),
            rootfs: rootfs.into(),
            ..Default::default()
        }
    }

    // "docker tag Onto Existing Tag Is A Silent No-Op" (P1): retagging an existing dest must REPOINT it
    // at the source content, not leave the stale mapping.
    #[test]
    fn image_tag_existing_repo_tag_repoints_to_source() {
        let mut images = vec![img("src:latest", "/store/src"), img("dst:latest", "/store/old-dst")];
        let src = images[0].clone();
        apply_tag(&mut images, &src, "dst:latest");
        let dst = images.iter().find(|i| i.name == "dst:latest").unwrap();
        assert_eq!(dst.rootfs, "/store/src", "retag must repoint dst at the source rootfs");
        assert_eq!(
            images.iter().filter(|i| i.name == "dst:latest").count(),
            1,
            "retag must not duplicate the dest entry"
        );
    }

    // "Image Aliases Report Different IDs For The Same Rootfs" (P1): a new tag of one rootfs is another
    // reference to the SAME content, so both must report the same content-derived image id.
    #[test]
    fn image_tag_new_alias_shares_content_image_id() {
        let mut images = vec![img("src:latest", "/store/src")];
        let src = images[0].clone();
        apply_tag(&mut images, &src, "src:v2");
        assert_eq!(images.len(), 2, "a new tag adds a second reference");
        assert_eq!(
            image_id(&images[0]),
            image_id(&images[1]),
            "two tags of one rootfs must share one image id"
        );
    }

    #[test]
    fn image_id_differs_for_distinct_rootfs() {
        assert_ne!(
            image_id(&img("a:latest", "/store/a")),
            image_id(&img("b:latest", "/store/b")),
            "distinct rootfs content must yield distinct image ids"
        );
    }

    // "rmi nginx Removes Unrelated Repositories Sharing Basename" (P1): the rmi match key is the fully-
    // qualified repository, so `rmi nginx` must not match a same-basename `linuxserver/nginx`.
    #[test]
    fn rmi_match_key_is_fully_qualified_repository() {
        // The closure image_delete uses: fully-qualified repo AND tag must match.
        let want_repo = ref_repo("nginx");
        let want_tag = ref_tag("nginx");
        let matches = |i: &Image| ref_repo(&i.name) == want_repo && ref_tag(&i.name) == want_tag;
        assert!(matches(&img("nginx:latest", "/store/n")), "bare nginx matches nginx:latest");
        assert!(
            !matches(&img("linuxserver/nginx:latest", "/store/ls")),
            "bare nginx must NOT match linuxserver/nginx"
        );
    }
}
