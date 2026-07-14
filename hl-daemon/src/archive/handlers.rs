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
            Json(crate::api::ErrorMessage {
                message: format!("Could not find the file {} in container {id}", q.path),
            }),
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
            Json(crate::api::ErrorMessage {
                message: format!("Could not find the file {} in container {id}", q.path),
            }),
        )
            .into_response();
    };
    // For a directory backed by the overlay, tar a MERGED upper-over-lower view (staged in a temp tree)
    // so lower-layer entries the container never touched are not dropped; otherwise tar the single
    // physical path directly. `--format=posix` preserves sub-second (nanosecond) mtimes in the output.
    let staged = merged_overlay_dir(&upper, &rootfs, &binds, &a.volumes_dir, &q.path);
    let (parent, base) = match &staged {
        Some(s) => (s.parent.clone(), s.base.clone()),
        None => (
            host.parent()
                .unwrap_or(std::path::Path::new("/"))
                .to_path_buf(),
            host.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".into()),
        ),
    };
    let out = std::process::Command::new("tar")
        .args(get_tar_argv(&parent, &base))
        .output();
    if let Some(s) = &staged {
        let _ = std::fs::remove_dir_all(&s.tmp_root);
    }
    match out {
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
                Json(crate::api::ErrorMessage {
                    message: "failed to read archive from container".into(),
                }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::api::ErrorMessage {
                message: e.to_string(),
            }),
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
    let (cid, upper, rootfs, binds, ro_targets) = {
        let g = a.inner.lock().await;
        let Some((full, c)) = resolve_get(&g, &id) else {
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
            readonly_mount_targets(&c.binds, &c.mounts),
        )
    };
    // A read-only bind/volume mount must reject writes through `docker cp` / archive PUT — otherwise the
    // write lands in the host source dir, silently bypassing the read-only flag the guest sees.
    if path_under_readonly_mount(&q.path, &ro_targets) {
        return (
            StatusCode::FORBIDDEN,
            Json(crate::api::ErrorMessage {
                message: format!("cannot copy to {}: mounted path is marked read-only", q.path),
            }),
        )
            .into_response();
    }
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
            Json(crate::api::ErrorMessage {
                message: format!("extraction point {} is not a directory", q.path),
            }),
        )
            .into_response();
    }
    // Stream the archive straight into tar over stdin -- NO temp file. The former per-daemon-PID temp path
    // ("hl-cp-<pid>.tar") was SHARED across every in-flight cp of one daemon, so CONCURRENT `docker cp`s
    // clobbered each other's archive and raced its unlink: a cp could extract a sibling's payload, or fail
    // "tar: Failed to open archive" when another removed the file mid-read. Piping is race-free (each
    // request owns its child's stdin) and skips a disk round-trip. `extract_archive_into` also contains the
    // extraction against a pre-existing destination symlink escaping the target, and restores archive
    // uid/gid (`--numeric-owner -p`) the way docker cp does.
    let out = extract_archive_into(&host, body.as_ref());
    // docker-cp coherence: the daemon just mutated a (possibly live) container's filesystem from
    // OUTSIDE any engine — no guest syscall announces it, so the running engines' path/metadata caches
    // could keep serving a stale ENOENT (or stale size/mtime) for the delivered paths. Bump the
    // container's external-writer generation AFTER the extraction so every engine of the container drops
    // its caches on its next syscall (util.rs fsgen_bump <-> fscache.c fsgen_poll). Bumped even on a
    // failed tar: a partial extraction is a mutation too. Stopped container: harmless no-op.
    crate::util::fsgen_bump(&cid);
    match out {
        Ok(o) if o.status.success() => {
            (StatusCode::OK, Json(crate::api::Empty {})).into_response()
        }
        Ok(o) => {
            eprintln!(
                "archive_put: tar failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(crate::api::ErrorMessage {
                message: "failed to extract archive".into(),
            }),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(crate::api::ErrorMessage {
                message: e.to_string(),
            }),
        )
            .into_response(),
    }
}

/// argv (after `tar`) for the GET side. `--format=posix` (pax) is required so sub-second (nanosecond)
/// mtimes survive in the stream — the default ustar format truncates mtimes to whole seconds.
fn get_tar_argv(parent: &std::path::Path, base: &str) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    vec![
        OsString::from("--format=posix"),
        OsString::from("-c"),
        OsString::from("-f"),
        OsString::from("-"),
        OsString::from("-C"),
        parent.as_os_str().to_os_string(),
        OsString::from(base),
    ]
}

/// argv (after `tar`) for the PUT side. `--numeric-owner -p` makes tar restore the archive's numeric
/// uid/gid and full permission bits instead of dropping ownership to the extractor — matching docker cp,
/// which preserves the copied files' ownership. (The actual chown requires the daemon to run as root; the
/// flags REQUEST it so a privileged daemon does the right thing.)
fn put_tar_argv(host: &std::path::Path) -> Vec<std::ffi::OsString> {
    use std::ffi::OsString;
    vec![
        OsString::from("-x"),
        OsString::from("-f"),
        OsString::from("-"),
        OsString::from("--numeric-owner"),
        OsString::from("-p"),
        OsString::from("-C"),
        host.as_os_str().to_os_string(),
    ]
}

/// List the member paths of an in-memory tar archive (`tar -t -f -`). Best-effort: an unreadable archive
/// yields no entries (extraction itself then surfaces the error).
fn list_tar_entries(body: &[u8]) -> Vec<String> {
    use std::io::Write;
    use std::process::Stdio;
    let child = std::process::Command::new("tar")
        .arg("-t")
        .arg("-f")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return Vec::new();
    };
    if let Some(mut si) = child.stdin.take() {
        let body = body.to_vec();
        std::thread::spawn(move || {
            let _ = si.write_all(&body);
        });
    }
    match child.wait_with_output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Contain a PUT extraction against destination-symlink traversal (docker cp PUT vuln): if a path
/// component of an incoming entry already exists AT the destination as a symlink, host `tar` would FOLLOW
/// it and write through it — e.g. a pre-existing `linkout -> ../outside` plus an entry `linkout/file.txt`
/// lands the write OUTSIDE the target. Walk each entry's cumulative path (clamped inside `host`, so we can
/// never touch anything outside the target) and unlink any component currently a symlink; tar then
/// materializes a real dir/file inside the target instead of escaping through the link.
fn scrub_traversal_symlinks(host: &std::path::Path, entries: &[String]) {
    for entry in entries {
        let mut cur = host.to_path_buf();
        for part in entry.split('/') {
            match part {
                "" | "." => continue,
                ".." => {
                    if cur.as_path() != host {
                        cur.pop();
                    }
                    continue;
                }
                p => cur.push(p),
            }
            if !cur.starts_with(host) {
                break; // paranoia: a component escaped the target — stop before touching it
            }
            if let Ok(md) = std::fs::symlink_metadata(&cur) {
                if md.file_type().is_symlink() {
                    let _ = std::fs::remove_file(&cur);
                }
            }
        }
    }
}

/// Extract an in-memory tar archive into `host` the way docker cp PUT must: contained against
/// destination-symlink traversal ([`scrub_traversal_symlinks`]) and preserving numeric ownership/perms
/// ([`put_tar_argv`]). The body is streamed into tar's stdin from a dedicated thread so a full pipe can't
/// deadlock against tar's stdout/stderr.
fn extract_archive_into(
    host: &std::path::Path,
    body: &[u8],
) -> std::io::Result<std::process::Output> {
    scrub_traversal_symlinks(host, &list_tar_entries(body));
    use std::io::Write;
    use std::process::Stdio;
    let mut child = std::process::Command::new("tar")
        .args(put_tar_argv(host))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut si) = child.stdin.take() {
        let body = body.to_vec();
        std::thread::spawn(move || {
            let _ = si.write_all(&body);
        }); // drop closes stdin -> tar EOF
    }
    child.wait_with_output()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hl-cp-test-{}-{}-{}",
            tag,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|x| x.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn as_strings(argv: &[std::ffi::OsString]) -> Vec<String> {
        argv.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    // Finding 23: PUT extraction must restore the archive's uid/gid & perms (`--numeric-owner -p`), else
    // docker cp'd files silently lose ownership. (Real chown needs root; here we assert the request.)
    #[test]
    fn put_tar_argv_requests_numeric_owner_and_preserve() {
        let s = as_strings(&put_tar_argv(std::path::Path::new("/dest")));
        assert!(s.iter().any(|a| a == "-x"), "must extract: {s:?}");
        assert!(s.iter().any(|a| a == "--numeric-owner"), "missing --numeric-owner: {s:?}");
        assert!(s.iter().any(|a| a == "-p"), "missing -p (preserve perms): {s:?}");
    }

    // Finding 25: GET must emit pax (`--format=posix`) so nanosecond mtimes are not truncated to seconds.
    #[test]
    fn get_tar_argv_requests_posix_format_for_nanosecond_mtimes() {
        let s = as_strings(&get_tar_argv(std::path::Path::new("/p"), "base"));
        assert!(s.iter().any(|a| a == "--format=posix"), "missing --format=posix: {s:?}");
        assert!(s.iter().any(|a| a == "base"), "member name passed: {s:?}");
    }

    // Finding 22 (P1 security): a PUT through a pre-existing destination symlink pointing outside the
    // target must NOT write outside it. The malicious `linkout -> ../outside` must be replaced by a real
    // dir holding the delivered file, and nothing may appear in the sibling `outside/` tree.
    #[test]
    fn put_does_not_escape_through_existing_destination_symlink() {
        let root = scratch("escape");
        let target = root.join("container");
        let outside = root.join("outside");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // pre-existing malicious symlink INSIDE the target, pointing out of it
        std::os::unix::fs::symlink("../outside", target.join("linkout")).unwrap();
        // an archive delivering linkout/file.txt
        let src = root.join("src");
        std::fs::create_dir_all(src.join("linkout")).unwrap();
        std::fs::write(src.join("linkout/file.txt"), b"pwned").unwrap();
        let tar = std::process::Command::new("tar")
            .arg("-c")
            .arg("-f")
            .arg("-")
            .arg("-C")
            .arg(&src)
            .arg("linkout")
            .output()
            .unwrap();
        assert!(tar.status.success(), "building fixture archive failed");

        let out = extract_archive_into(&target, &tar.stdout).unwrap();
        assert!(out.status.success(), "tar stderr: {}", String::from_utf8_lossy(&out.stderr));

        assert!(!outside.join("file.txt").exists(), "write ESCAPED the target via the symlink");
        let md = std::fs::symlink_metadata(target.join("linkout")).unwrap();
        assert!(md.file_type().is_dir(), "destination component is still a traversing symlink");
        assert_eq!(std::fs::read(target.join("linkout/file.txt")).unwrap(), b"pwned");

        let _ = std::fs::remove_dir_all(&root);
    }
}
