#![allow(unused_imports, dead_code)]
//! Build-cache reclamation and container snapshotting: `build_prune` (`docker builder prune` / the
//! build-cache portion of `docker system prune`) and `commit_container` (`docker commit`). Moved verbatim
//! from the former `build.rs`; shared types/helpers come from `mod.rs` via `use super::*`.
use super::*;

/// `POST /build/prune` — `docker builder prune` / the build-cache portion of `docker system prune`.
/// Reclaims BOTH dd build-cache slots: the new per-step layer cache (~/.dd/buildcache, populated by
/// `docker build`) and the persistent JIT translated-code cache (~/.dd/pcache, surfaced as `system df`
/// BuilderSize). Both are fully reclaimable — layers re-snapshot on the next build, pcache re-translates
/// on demand — so a wholesale drop only forces a one-time recompute.
pub(crate) async fn build_prune() -> axum::Json<serde_json::Value> {
    let (mut deleted, mut reclaimed) = (Vec::new(), 0i64);
    // 1) the build layer cache: one dir per cached step under ~/.dd/buildcache/layers.
    let layers = crate::util::buildcache_dir().join("layers");
    if let Ok(rd) = std::fs::read_dir(&layers) {
        for e in rd.filter_map(|e| e.ok()) {
            let sz = crate::util::dir_size(&e.path());
            if std::fs::remove_dir_all(e.path()).is_ok() {
                reclaimed += sz;
                deleted.push(format!("buildcache:{}", e.file_name().to_string_lossy()));
            }
        }
    }
    // 2) the persistent JIT translated-code cache: one <binid>.pcache file per guest binary.
    let dir = crate::util::dd_home().join("pcache");
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
    axum::Json(serde_json::json!({"CachesDeleted": deleted, "SpaceReclaimed": reclaimed}))
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
/// container's run config (cmd/env/workdir/labels, plus the source image's entrypoint). dd runs a
/// container directly in the (shared) image rootfs with no copy-on-write upper, so the snapshot is a
/// `cp -a` of the live rootfs — it captures every write the container made, matching `docker commit`'s
/// "freeze the current filesystem" semantics. Reuses the same registration path as build/load/import.
pub(crate) async fn commit_container(State(a): State<App>, Query(q): Query<CommitQ>) -> Response {
    let cid = q.container.unwrap_or_default();
    if cid.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": "container is required for commit"})),
        )
            .into_response();
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
    let (rootfs, cmd, entrypoint, env, workdir, labels, arch, pause_pid) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, &cid) else {
            return no_such(&cid);
        };
        let Some(c) = g.containers.get(&full) else {
            return no_such(&cid);
        };
        let img = g
            .images
            .iter()
            .find(|i| ref_name(&i.name) == ref_name(&c.image))
            .cloned();
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
            Json(json!({"message": "cannot commit a container with a host-filesystem rootfs"})),
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
        format!("commit-{}", &fake_id(&cid)[..12])
    } else {
        name.clone()
    };
    // Snapshot the rootfs into a fresh image dir (mirrors image_load/import's <images_dir>/<sanitized>).
    let target = PathBuf::from(format!("{}/{}", a.images_dir, key.replace(['/', ':'], "_")));
    let new_rootfs = target.join("rootfs");
    let _ = std::fs::remove_dir_all(&target);
    if let Err(e) = std::fs::create_dir_all(&target) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": format!("commit: {e}")})),
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
            Json(json!({"message": "commit: failed to snapshot rootfs"})),
        )
            .into_response();
    }
    // A content-ish id (deterministic per source + time so repeated commits get distinct ids).
    let id = {
        let manifest = format!(
            "commit:{cid}\nname:{key}\ncmd:{}\nenv:{}\nworkdir:{workdir}\nat:{}",
            cmd.join("\u{1}"),
            env.join("\u{1}"),
            now_secs()
        );
        let h = sha256_hex(manifest.as_bytes());
        if h.len() == 64 {
            h
        } else {
            fake_id(&manifest)
        }
    };
    // Persist a dd-image.json so the image survives a daemon restart (discover_images reads it).
    let mut dd = json!({"name": key, "cmd": cmd, "entrypoint": entrypoint, "env": env, "workdir": workdir, "labels": labels});
    if let Some(c) = &q.comment {
        dd["comment"] = json!(c);
    }
    if let Some(a) = &q.author {
        dd["author"] = json!(a);
    }
    let _ = std::fs::write(target.join("dd-image.json"), dd.to_string());
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
            labels,
            created: now_secs(),
            ..Default::default()
        });
    }
    crate::events::emit_event(&a.events, "image", "commit", &id, json!({"name": key}));
    (
        StatusCode::CREATED,
        Json(json!({"Id": format!("sha256:{id}")})),
    )
        .into_response()
}
