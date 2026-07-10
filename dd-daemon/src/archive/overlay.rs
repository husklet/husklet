use super::*;

/// Render a container's `--mount`/Mounts as `-v`-style "source:target" bind strings so they can be fed
/// to [`archive_host_path`] alongside the real `-v`/Binds (which it resolves identically). A `type=bind`
/// mount's Source is an absolute host path; a `type=volume` mount's Source is a NAME that
/// `archive_host_path` then roots at `<volumes_dir>/<name>` (the local driver's mountpoint). Keeping the
/// binds-string contract means cp sees exactly the mount layout the guest does, without a wider signature
/// (build.rs reuses the same helper for COPY/ADD with no mounts). Empty-target/source mounts are skipped.
pub(crate) fn mounts_as_binds(mounts: &[Mount]) -> Vec<String> {
    mounts
        .iter()
        .filter(|m| !m.target.is_empty() && !m.source.is_empty())
        .map(|m| format!("{}:{}", m.source, m.target))
        .collect()
}

/// Container-side destination paths of every READ-ONLY mount — `-v src:dst:ro` binds and
/// `--mount ...,readonly` mounts. The runtime honors the read-only flag for the guest, so a `docker cp` /
/// archive PUT that resolved the mount to its HOST source and wrote there would silently bypass it; the
/// archive path must refuse writes under these targets instead.
pub(crate) fn readonly_mount_targets(binds: &[String], mounts: &[Mount]) -> Vec<String> {
    let mut targets = Vec::new();
    for b in binds {
        if let Some((_, dst, true)) = crate::containers::parse_bind(b) {
            targets.push(dst.to_string());
        }
    }
    for m in mounts {
        if m.read_only && !m.target.is_empty() {
            targets.push(m.target.clone());
        }
    }
    targets
}

/// Whether container `path` is inside (or equal to) any read-only mount target.
pub(crate) fn path_under_readonly_mount(path: &str, ro_targets: &[String]) -> bool {
    let p = path.trim_end_matches('/');
    ro_targets.iter().any(|t| {
        let t = t.trim_end_matches('/');
        !t.is_empty() && (p == t || p.starts_with(&format!("{t}/")))
    })
}

/// Map a container path to its host path. A path inside a bind/volume mount maps to its host source dir
/// (so `docker cp` to e.g. ddcli's mounted cwd, or a `-v name:/mnt` mount, hits the real files);
/// otherwise it lands in the container rootfs (the overlay upper). `..` is lexically clamped inside
/// whichever base so it can't escape. `binds` is "host:container" — for `--mount`/Mounts coverage the
/// caller appends [`mounts_as_binds`] (the local-driver volume name resolves to `<volumes_dir>/<name>`).
pub(crate) fn archive_host_path(
    rootfs: &str,
    binds: &[String],
    volumes_dir: &str,
    path: &str,
) -> std::path::PathBuf {
    // bind volumes first (host:container), same precedence as the JIT jail. The most specific
    // (longest container-dest) bind wins so nested binds resolve to the right source; a requested path
    // under a bind hits the HOST source, and only otherwise do we fall back to the rootfs.
    let mut best: Option<(String, &str)> = None; // (host source dir, container dest)
    for b in binds {
        let Some((host, cont)) = b.split_once(':') else {
            continue;
        };
        // A bind whose source is an absolute path is a host bind-mount; otherwise it's a NAMED VOLUME
        // (`name:/path`) whose host dir is its directory under volumes_dir -- matching spawn_cfg's
        // default-driver resolution (`<volumes_dir>/<name>`, which is the volume's mountpoint).
        let src = if host.starts_with('/') {
            host.to_string()
        } else {
            std::path::Path::new(volumes_dir)
                .join(host)
                .to_string_lossy()
                .into_owned()
        };
        let cont = cont.trim_end_matches('/');
        if path == cont || path.strip_prefix(cont).is_some_and(|r| r.starts_with('/')) {
            if best.as_ref().map_or(true, |(_, bc)| cont.len() > bc.len()) {
                best = Some((src, cont));
            }
        }
    }
    if let Some((host, cont)) = best {
        return clamp_join(&host, &path[cont.len()..]);
    }
    clamp_join(rootfs, path)
}

/// Resolve a container path to its host path through the per-container copy-on-write overlay. A path under
/// a bind/volume mount maps to the host source (as in `archive_host_path`); a plain rootfs path lands in
/// the writable UPPER (`upper`). For reads (`write == false`) a rootfs path the container hasn't copied up
/// yet falls back to the read-only image rootfs (`lower`) so `docker cp` can read unmodified image files;
/// a `.wh.NAME` whiteout in the upper hides a lower file (the upper path, which doesn't exist, is kept so
/// the read 404s). For writes (`write == true`) the upper is always selected so `docker cp` INTO the
/// container lands in the writable layer and never mutates the shared image. Empty `upper` (darwin/legacy
/// containers) means a flat rootfs -- identical to `archive_host_path`.
pub(crate) fn overlay_host_path(
    upper: &str,
    lower: &str,
    binds: &[String],
    volumes_dir: &str,
    path: &str,
    write: bool,
) -> std::path::PathBuf {
    if upper.is_empty() {
        return archive_host_path(lower, binds, volumes_dir, path);
    }
    let up = archive_host_path(upper, binds, volumes_dir, path);
    // A bind/volume path resolves to the same host source regardless of base, so up.exists() already
    // covers it; only genuine rootfs paths absent from the upper fall through to the lower.
    if write || up.exists() {
        return up;
    }
    if let (Some(dir), Some(name)) = (up.parent(), up.file_name()) {
        // deleted in the container (whiteout) -> keep the nonexistent upper path so the read reports absent
        if dir.join(format!(".wh.{}", name.to_string_lossy())).exists() {
            return up;
        }
    }
    archive_host_path(lower, binds, volumes_dir, path)
}

/// Join `rel` onto `base`, dropping `.`/`..` so the result stays within `base`.
pub(crate) fn clamp_join(base: &str, rel: &str) -> std::path::PathBuf {
    let root = std::path::Path::new(base).to_path_buf();
    let mut out = root.clone();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out != root {
                    out.pop();
                }
            }
            p => out.push(p),
        }
    }
    out
}

/// Go `os.FileMode` bits for docker's path-stat (the CLI keys off the dir/symlink flags).
pub(crate) fn go_filemode(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let mut m = md.permissions().mode() & 0o7777;
    let ft = md.file_type();
    if ft.is_dir() {
        m |= 1 << 31;
    }
    if ft.is_symlink() {
        m |= 1 << 27;
    }
    m
}

/// The `X-Docker-Container-Path-Stat` header value: base64(JSON{name,size,mode,mtime,linkTarget}).
pub(crate) fn path_stat_b64(host: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let md = std::fs::symlink_metadata(host).ok()?;
    let name = host
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let link = if md.file_type().is_symlink() {
        std::fs::read_link(host)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        String::new()
    };
    let stat = json!({"name": name, "size": md.len(), "mode": go_filemode(&md),
        "mtime": fmt_rfc3339(md.mtime()), "linkTarget": link});
    Some(base64_std(stat.to_string().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ro_mount(target: &str) -> Mount {
        Mount { typ: "volume".into(), source: "vol".into(), target: target.into(), read_only: true }
    }

    // "Archive PUT Writes Through Read-Only Bind Mounts" (P1): a read-only bind/volume mount's target
    // must be recognized so archive PUT can refuse writes into it.
    #[test]
    fn readonly_targets_cover_ro_binds_and_mounts() {
        let binds = vec![
            "/h/ro:/data:ro".to_string(),
            "/h/rw:/rw".to_string(),
            "vol:/named:ro,z".to_string(),
        ];
        let mounts = vec![ro_mount("/mnt/ro"), Mount {
            typ: "bind".into(),
            source: "/h/x".into(),
            target: "/mnt/rw".into(),
            read_only: false,
        }];
        let t = readonly_mount_targets(&binds, &mounts);
        assert!(t.contains(&"/data".to_string()), "ro bind target");
        assert!(t.contains(&"/named".to_string()), "ro,z bind target");
        assert!(t.contains(&"/mnt/ro".to_string()), "ro --mount target");
        assert!(!t.contains(&"/rw".to_string()), "rw bind not included");
        assert!(!t.contains(&"/mnt/rw".to_string()), "rw mount not included");
    }

    #[test]
    fn path_under_readonly_mount_matches_target_and_children() {
        let ro = vec!["/data".to_string()];
        assert!(path_under_readonly_mount("/data", &ro), "the mount root itself");
        assert!(path_under_readonly_mount("/data/sub/x.txt", &ro), "a child path");
        assert!(path_under_readonly_mount("/data/", &ro), "trailing slash");
        assert!(!path_under_readonly_mount("/database", &ro), "sibling prefix must NOT match");
        assert!(!path_under_readonly_mount("/other", &ro));
    }
}
