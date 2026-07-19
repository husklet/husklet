//! Build-cache reclamation and container snapshotting: `build_prune` (`docker builder prune` / the
//! build-cache portion of `docker system prune`) and `commit_container` (`docker commit`). Moved verbatim
//! from the former `build.rs`; shared types/helpers come from `mod.rs` via `use super::*`.
use super::*;

/// `POST /build/prune` — `docker builder prune` / the build-cache portion of `docker system prune`.
/// Reclaims BOTH hl build-cache slots: the new per-step layer cache (~/.hl/buildcache, populated by
/// `docker build`) and the persistent JIT translated-code cache (~/.hl/pcache, surfaced as `system df`
/// BuilderSize). Both are fully reclaimable — layers re-snapshot on the next build, pcache re-translates
/// on demand — so a wholesale drop only forces a one-time recompute.
pub(crate) async fn build_prune() -> axum::Json<crate::api::BuildPruneReport> {
    let (mut deleted, mut reclaimed) = (Vec::new(), 0i64);
    // 1) the build layer cache: one dir per cached step under ~/.hl/buildcache/layers.
    let layers = crate::util::buildcache_dir().join("layers");
    if let Ok(rd) = std::fs::read_dir(&layers) {
        for e in rd.filter_map(|e| e.ok()) {
            let sz = crate::util::PathSize::size(&e.path());
            if std::fs::remove_dir_all(e.path()).is_ok() {
                reclaimed += sz;
                deleted.push(format!("buildcache:{}", e.file_name().to_string_lossy()));
            }
        }
    }
    // 2) the persistent JIT translated-code cache: one <binid>.pcache file per guest binary.
    let dir = crate::util::hl_home().join("pcache");
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let sz = e
                .metadata()
                .ok()
                .filter(|m| m.is_file())
                .map(|m| m.len() as i64);
            if let Some(sz) = sz {
                if std::fs::remove_file(e.path()).is_ok() {
                    reclaimed += sz;
                    deleted.push(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
    }
    axum::Json(crate::api::BuildPruneReport {
        caches_deleted: deleted,
        space_reclaimed: reclaimed,
    })
}

#[derive(Deserialize)]
pub(crate) struct CommitQ {
    container: Option<String>,
    repo: Option<String>,
    tag: Option<String>,
    comment: Option<String>,
    author: Option<String>,
    pause: Option<String>,
}

/// `POST /commit?container=<id>&repo=<r>&tag=<t>` — `docker commit`. Snapshots the container's CURRENT
/// rootfs into a new image directory under `images_dir` and registers it as `repo:tag` carrying the
/// container's run config (cmd/env/workdir/labels, plus the source image's entrypoint). hl runs a
/// container directly in the (shared) image rootfs with no copy-on-write upper, so the snapshot is a
/// `cp -a` of the live rootfs — it captures every write the container made, matching `docker commit`'s
/// "freeze the current filesystem" semantics. Reuses the same registration path as build/load/import.
impl Container {
    pub(crate) async fn commit(State(a): State<App>, Query(q): Query<CommitQ>) -> Response {
        let cid = q.container.unwrap_or_default();
        if cid.is_empty() {
            return ErrorMessage::bad_request("container is required for commit");
        }
        // Resolve the source container; carry its run config into the new image. The container only stores
        // the resolved cmd/env it ran with, so inherit the source image's ENTRYPOINT (and arch as a fallback).
        // `docker commit` pauses the container by default while snapshotting (so the image isn't captured
        // mid-write); `--pause=false` opts out. `pause_pid` is the live pid to SIGSTOP/SIGCONT around the
        // `cp -a` — Some only when we will pause AND the guest is actually running.
        let pause = !matches!(
            q.pause.as_deref(),
            Some("0") | Some("false") | Some("False")
        );
        let (rootfs, upper, user, cmd, entrypoint, env, workdir, labels, arch, pause_pid) = {
            let g = a.inner.lock().await;
            let Some(full) = ContainerId::resolve(&g, &cid) else {
                return ErrorMessage::no_such(&cid);
            };
            let Some(c) = g.containers.get(&full) else {
                return ErrorMessage::no_such(&cid);
            };
            // Resolve the source image through the repo-qualified `find_image` (NOT a bare-basename match):
            // a container from `linuxserver/nginx:latest` must inherit that image's config, not an unrelated
            // `nginx:latest` that merely shares the basename.
            let img = DiscoveredImages::new(&g.images).find(&c.image).cloned();
            let entrypoint = img
                .as_ref()
                .map(|i| i.entrypoint.clone())
                .unwrap_or_default();
            let arch = c
                .arch
                .or(img.as_ref().map(|i| i.arch))
                .unwrap_or(Guest::LinuxAarch64);
            let pause_pid = (pause && c.status == "running")
                .then(|| g.live.get(&full).and_then(|l| *l.pid.lock().unwrap()))
                .flatten();
            (
                c.rootfs.clone(),
                c.upper.clone(),
                c.user.clone(),
                c.cmd.clone(),
                entrypoint,
                c.env.clone(),
                c.working_dir.clone(),
                c.labels.clone(),
                arch,
                pause_pid,
            )
        };
        // The host-fs `macos` image (rootfs "/") can't be snapshotted — copying it would be catastrophic.
        if rootfs.is_empty() || rootfs == "/" {
            return (
                StatusCode::BAD_REQUEST,
                Json(crate::api::ErrorMessage {
                    message: "cannot commit a container with a host-filesystem rootfs".into(),
                }),
            )
                .into_response();
        }
        // repo[:tag]. Docker defaults the tag to "latest"; an empty repo commits a dangling <none> image.
        let repo = q.repo.unwrap_or_default();
        let tag = q
            .tag
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "latest".into());
        let name = if repo.is_empty() {
            String::new()
        } else {
            format!("{}:{}", repo.trim_end_matches(':'), tag)
        };
        let key = if name.is_empty() {
            format!("commit-{}", &Digest::fake(&cid)[..12])
        } else {
            name.clone()
        };
        // Persisted the rootfs into a fresh image dir (mirrors image_load/import's <images_dir>/<sanitized>).
        // Use the shared injective store encoder so commit dirs match load/import/pull and can't collide
        // (`owner/app:1_2` vs `owner/app_1:2` both flattened to the same `_`-joined name under the old scheme).
        let target = PathBuf::from(format!(
            "{}/{}",
            a.images_dir,
            hl_images::Key::from_name(&key).as_str()
        ));
        let new_rootfs = target.join("rootfs");
        let _ = std::fs::remove_dir_all(&target);
        if let Err(e) = std::fs::create_dir_all(&target) {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::api::ErrorMessage {
                    message: format!("commit: {e}"),
                }),
            )
                .into_response();
        }
        // `cp -a SRC DST` (DST absent) copies the SRC tree to DST, preserving perms/links/timestamps.
        // Default-pause: freeze the guest (SIGSTOP) around the snapshot so the rootfs isn't copied while the
        // container is mid-write, then SIGCONT — unconditionally, even if the copy failed, so we never leave
        // it stopped.
        if let Some(pid) = pause_pid {
            unsafe {
                libc::kill(pid as i32, libc::SIGSTOP);
            }
        }
        let cp = std::process::Command::new("cp")
            .arg("-a")
            .arg(&rootfs)
            .arg(&new_rootfs)
            .status();
        if let Some(pid) = pause_pid {
            unsafe {
                libc::kill(pid as i32, libc::SIGCONT);
            }
        }
        if !matches!(cp, Ok(s) if s.success()) {
            let _ = std::fs::remove_dir_all(&target);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::api::ErrorMessage {
                    message: "commit: failed to snapshot rootfs".into(),
                }),
            )
                .into_response();
        }
        // Overlay the container's writable UPPER layer on top of the copied lower rootfs so the committed
        // image captures the container's file WRITES (docker commits the container filesystem, not the base
        // image). Without this, commit silently produced an image with stale lower-layer content. `cp -a
        // upper/. new_rootfs/` merges additions/modifications (upper wins); linux containers only (darwin has
        // no upper). Deletions modeled as overlay whiteouts aren't replayed — a best-effort merge of writes.
        if !upper.is_empty() && std::path::Path::new(&upper).is_dir() {
            let _ = std::process::Command::new("cp")
                .arg("-a")
                .arg(format!("{upper}/."))
                .arg(&new_rootfs)
                .status();
        }
        // A content-ish id (deterministic per source + time so repeated commits get distinct ids).
        let id = {
            let manifest = format!(
                "commit:{cid}\nname:{key}\ncmd:{}\nenv:{}\nworkdir:{workdir}\nat:{}",
                cmd.join("\u{1}"),
                env.join("\u{1}"),
                now_secs()
            );
            Sha256Digest::from_bytes(manifest.as_bytes()).to_string()
        };
        // Persist a hl-image.json so the image survives a daemon restart (discover_images reads it). Include
        // the effective `user` (Config.User) and the guest `arch`/`os` so a committed ELF-less image keeps its
        // default runtime user and doesn't get relabeled to arm64 by discovery's ELF-sniffing fallback.
        let mut desc = json!({"name": key, "cmd": cmd, "entrypoint": entrypoint, "env": env,
        "workdir": workdir, "labels": labels, "user": user,
        "arch": arch.arch(), "os": arch.os()});
        if let Some(c) = &q.comment {
            desc["comment"] = json!(c);
        }
        if let Some(a) = &q.author {
            desc["author"] = json!(a);
        }
        let _ = std::fs::write(target.join("hl-image.json"), desc.to_string());
        {
            let mut g = a.inner.lock().await;
            // Replace any existing image sharing this repo:tag (mirrors the build/load re-tag dedupe).
            if !key.is_empty() {
                g.images.retain(|im| im.name != key);
            }
            g.images.push(Image {
                name: key.clone(),
                rootfs: new_rootfs.to_string_lossy().into_owned(),
                arch,
                cmd,
                entrypoint,
                env,
                workdir,
                user,
                labels,
                created: now_secs(),
                ..Default::default()
            });
        }
        crate::events::emit_event(&a.events, "image", "commit", &id, json!({"name": key}));
        (
            StatusCode::CREATED,
            Json(crate::api::CommitResponse {
                id: format!("sha256:{id}"),
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod commit_source_tests {
    use crate::model::Image;
    use crate::util::DiscoveredImages;

    fn img(name: &str, entrypoint: &str) -> Image {
        Image {
            name: name.into(),
            rootfs: format!("/store/{name}"),
            entrypoint: vec![entrypoint.into()],
            ..Default::default()
        }
    }

    // "docker commit Can Inherit Config From Wrong Repository" (P1): the commit source image is resolved
    // via repo-qualified find_image, so a container from linuxserver/nginx must not inherit nginx's config.
    #[test]
    fn commit_source_resolves_full_repository_not_basename() {
        let imgs = vec![
            img("nginx:latest", "/wrong-entrypoint"),
            img("linuxserver/nginx:latest", "/right-entrypoint"),
        ];
        let resolved = DiscoveredImages::new(&imgs)
            .find("linuxserver/nginx:latest")
            .unwrap();
        assert_eq!(resolved.entrypoint, vec!["/right-entrypoint".to_string()]);
    }
}
