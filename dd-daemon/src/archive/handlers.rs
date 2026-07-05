use super::*;

#[derive(serde::Deserialize)]
pub(crate) struct ArchiveQ {
    path: String,
}

pub(crate) async fn archive_head(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<ArchiveQ>,
) -> Response {
    let g = a.inner.lock().await;
    let Some((_, c)) = resolve_get(&g, &id) else {
        return no_such(&id);
    };
    let binds: Vec<String> = c
        .binds
        .iter()
        .cloned()
        .chain(mounts_as_binds(&c.mounts))
        .collect();
    match path_stat_b64(&overlay_host_path(
        &c.upper,
        &c.rootfs,
        &binds,
        &a.volumes_dir,
        &q.path,
        false,
    )) {
        Some(stat) => (StatusCode::OK, [("X-Docker-Container-Path-Stat", stat)]).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(
                json!({"message": format!("Could not find the file {} in container {id}", q.path)}),
            ),
        )
            .into_response(),
    }
}

pub(crate) async fn archive_get(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<ArchiveQ>,
) -> Response {
    let (upper, rootfs, binds) = {
        let g = a.inner.lock().await;
        let Some((_, c)) = resolve_get(&g, &id) else {
            return no_such(&id);
        };
        (
            c.upper.clone(),
            c.rootfs.clone(),
            c.binds
                .iter()
                .cloned()
                .chain(mounts_as_binds(&c.mounts))
                .collect::<Vec<_>>(),
        )
    };
    let host = overlay_host_path(&upper, &rootfs, &binds, &a.volumes_dir, &q.path, false);
    let Some(stat) = path_stat_b64(&host) else {
        return (
            StatusCode::NOT_FOUND,
            Json(
                json!({"message": format!("Could not find the file {} in container {id}", q.path)}),
            ),
        )
            .into_response();
    };
    let parent = host
        .parent()
        .unwrap_or(std::path::Path::new("/"))
        .to_path_buf();
    let base = host
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".into());
    match std::process::Command::new("tar")
        .arg("cf")
        .arg("-")
        .arg("-C")
        .arg(&parent)
        .arg(&base)
        .output()
    {
        Ok(o) if o.status.success() => (
            StatusCode::OK,
            [
                ("Content-Type", "application/x-tar".to_string()),
                ("X-Docker-Container-Path-Stat", stat),
            ],
            o.stdout,
        )
            .into_response(),
        Ok(o) => {
            eprintln!(
                "archive_get: tar failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": "failed to read archive from container"})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        )
            .into_response(),
    }
}

pub(crate) async fn archive_put(
    State(a): State<App>,
    Path(id): Path<String>,
    Query(q): Query<ArchiveQ>,
    body: axum::body::Bytes,
) -> Response {
    let (cid, upper, rootfs, binds) = {
        let g = a.inner.lock().await;
        let Some(full) = resolve_cid(&g, &id) else {
            return no_such(&id);
        };
        let Some(c) = g.containers.get(&full) else {
            return no_such(&id);
        };
        (
            full.clone(),
            c.upper.clone(),
            c.rootfs.clone(),
            c.binds
                .iter()
                .cloned()
                .chain(mounts_as_binds(&c.mounts))
                .collect::<Vec<_>>(),
        )
    };
    // cp INTO the container writes to the upper layer. The extraction dir may exist only in the read-only
    // image (lower); mirror it into the upper (copy-up the dir) so the files land in the writable layer
    // and the image is never touched.
    let host = overlay_host_path(&upper, &rootfs, &binds, &a.volumes_dir, &q.path, true);
    if !host.is_dir() {
        let lower = overlay_host_path("", &rootfs, &binds, &a.volumes_dir, &q.path, false);
        if lower.is_dir() {
            let _ = std::fs::create_dir_all(&host);
        }
    }
    if !host.is_dir() {
        // The copy-up attempt above may still have created dirs in the upper — announce that too.
        crate::util::fsgen_bump(&cid);
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"message": format!("extraction point {} is not a directory", q.path)})),
        )
            .into_response();
    }
    // Stream the archive straight into `tar xf - -C <host>` over stdin -- NO temp file. The former
    // per-daemon-PID temp path ("dd-cp-<pid>.tar") was SHARED across every in-flight cp of one daemon, so
    // CONCURRENT `docker cp`s clobbered each other's archive and raced its unlink: a cp could extract a
    // sibling's payload, or fail "tar: Failed to open archive" when another removed the file mid-read.
    // Piping is race-free (each request owns its child's stdin) and skips a disk round-trip. The body is
    // fed from a dedicated thread so a full stdin pipe can never deadlock against tar's stdout/stderr.
    let out = {
        use std::io::Write;
        use std::process::Stdio;
        match std::process::Command::new("tar")
            .arg("xf")
            .arg("-")
            .arg("-C")
            .arg(&host)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(mut si) = child.stdin.take() {
                    let body = body.clone(); // axum Bytes: cheap Arc clone, moved into the writer thread
                    std::thread::spawn(move || {
                        let _ = si.write_all(&body);
                    }); // drop closes stdin -> tar EOF
                }
                child.wait_with_output()
            }
            Err(e) => Err(e),
        }
    };
    // docker-cp coherence: the daemon just mutated a (possibly live) container's filesystem from
    // OUTSIDE any engine — no guest syscall announces it, so the running engines' path/metadata caches
    // could keep serving a stale ENOENT (or stale size/mtime) for the delivered paths. Bump the
    // container's external-writer generation AFTER the extraction so every engine of the container drops
    // its caches on its next syscall (util.rs fsgen_bump <-> fscache.c fsgen_poll). Bumped even on a
    // failed tar: a partial extraction is a mutation too. Stopped container: harmless no-op.
    crate::util::fsgen_bump(&cid);
    match out {
        Ok(o) if o.status.success() => (StatusCode::OK, Json(json!({}))).into_response(),
        Ok(o) => {
            eprintln!(
                "archive_put: tar failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"message": "failed to extract archive"})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"message": e.to_string()})),
        )
            .into_response(),
    }
}
