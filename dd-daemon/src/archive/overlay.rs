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

/// A staged, MERGED directory for `docker cp container:/dir -`. `overlay_host_path` alone hands back the
/// single physical dir that wins (upper if present) — so lower-layer entries a container never touched are
/// silently dropped from the tar. This carries the merged upper-over-lower view (built under a throwaway
/// temp tree) plus the `-C parent base` the GET tar should use; the caller tars it then deletes `tmp_root`.
pub(crate) struct StagedGet {
    pub(crate) parent: std::path::PathBuf,
    pub(crate) base: String,
    pub(crate) tmp_root: std::path::PathBuf,
}

/// Copy `src` to `dst` preserving mode/mtime/ownership/symlinks (`cp -a`) — the leaf op the merged walk
/// uses so staged entries keep the metadata docker cp must reproduce (nanosecond mtimes included).
fn copy_preserving(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    let st = std::process::Command::new("cp")
        .arg("-a")
        .arg("--")
        .arg(src)
        .arg(dst)
        .status()?;
    if st.success() {
        Ok(())
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "cp -a failed"))
    }
}

/// Recursively stage the overlay-merged contents of `upper` over `lower` into a fresh `dest` directory,
/// honoring the overlay whiteout conventions so the result is exactly what the container sees:
///   * `upper` entries shadow same-named `lower` entries;
///   * a `.wh.NAME` marker in `upper` hides `lower`'s NAME (and the marker itself never lands in `dest`);
///   * a `.wh..wh..opq` marker makes `upper` OPAQUE — all of `lower`'s entries at that level are dropped.
/// Either side may be absent. Directories present on both sides recurse (so nested whiteouts/opaque and
/// deep merges are handled and stray `.wh.*` control files inside upper-only subtrees are scrubbed);
/// entries unique to one side are copied wholesale.
pub(crate) fn stage_overlay_merge(
    upper: &std::path::Path,
    lower: &std::path::Path,
    dest: &std::path::Path,
) -> std::io::Result<()> {
    use std::collections::HashSet;
    std::fs::create_dir_all(dest)?;
    // Match the merged dir's own permission bits to whichever layer supplies it (upper wins).
    let src_dir = if upper.is_dir() {
        Some(upper)
    } else if lower.is_dir() {
        Some(lower)
    } else {
        None
    };
    if let Some(sd) = src_dir {
        if let Ok(md) = std::fs::metadata(sd) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                dest,
                std::fs::Permissions::from_mode(md.permissions().mode() & 0o7777),
            );
        }
    }
    let upper_is_dir = upper.is_dir();
    let opaque = upper_is_dir && upper.join(".wh..wh..opq").exists();
    let mut whiteouts: HashSet<String> = HashSet::new();
    let mut upper_names: HashSet<String> = HashSet::new();
    let mut upper_children: Vec<(String, std::path::PathBuf, bool)> = Vec::new(); // (name, path, is_dir)
    if upper_is_dir {
        for e in std::fs::read_dir(upper)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".wh..wh..opq" {
                continue; // opaque marker — already captured, never emit it
            }
            if let Some(stripped) = name.strip_prefix(".wh.") {
                whiteouts.insert(stripped.to_string()); // hides lower NAME; marker never emitted
                continue;
            }
            // read_dir file_type is lstat-based: a symlink is a leaf, not a dir to recurse into.
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            upper_names.insert(name.clone());
            upper_children.push((name, e.path(), is_dir));
        }
    }
    // Lower entries survive only where the upper neither shadows nor whites them out, and never under an
    // opaque upper.
    if lower.is_dir() && !opaque {
        for e in std::fs::read_dir(lower)?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if whiteouts.contains(&name) || upper_names.contains(&name) {
                continue;
            }
            copy_preserving(&e.path(), &dest.join(&name))?;
        }
    }
    for (name, upath, is_dir) in upper_children {
        let dpath = dest.join(&name);
        if is_dir {
            stage_overlay_merge(&upath, &lower.join(&name), &dpath)?;
        } else {
            copy_preserving(&upath, &dpath)?;
        }
    }
    Ok(())
}

/// A unique temp directory (`<tmp>/<prefix>-<pid>-<nanos>-<counter>`) — private per GET so concurrent
/// `docker cp` reads can't clobber each other's staged tree.
fn unique_tmp_dir(prefix: &str) -> Option<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let n = CTR.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{}-{}", std::process::id(), nanos, n));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// If `path` names an overlay-backed DIRECTORY, stage its merged upper-over-lower view and return the
/// `-C parent base` for the GET tar. Returns `None` — caller tars the single physical path directly — for
/// flat rootfs (`upper` empty), bind/volume-mounted paths (one physical source, nothing to merge), and
/// non-directories (a file resolves correctly through `overlay_host_path`).
pub(crate) fn merged_overlay_dir(
    upper: &str,
    lower: &str,
    binds: &[String],
    volumes_dir: &str,
    path: &str,
) -> Option<StagedGet> {
    if upper.is_empty() {
        return None; // flat rootfs — nothing to merge
    }
    let up = archive_host_path(upper, binds, volumes_dir, path);
    // A bind/volume path resolves to its host source, not the upper base — leave it to the direct tar.
    if up != clamp_join(upper, path) {
        return None;
    }
    let low = clamp_join(lower, path);
    let is_dir = up.is_dir() || (!up.exists() && low.is_dir());
    if !is_dir {
        return None;
    }
    let base = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let tmp_root = unique_tmp_dir("dd-cp-get")?;
    let (staged, base_str) = match base {
        Some(b) => (tmp_root.join(&b), b),
        None => (tmp_root.clone(), ".".to_string()), // cp of "/" -> tar the merged root itself
    };
    if stage_overlay_merge(&up, &low, &staged).is_err() {
        let _ = std::fs::remove_dir_all(&tmp_root);
        return None;
    }
    Some(StagedGet {
        parent: tmp_root.clone(),
        base: base_str,
        tmp_root,
    })
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

/// Encode a file's mode the way Go's `os.FileMode` does, which is what docker's path-stat header carries
/// and the CLI decodes. Go keeps ONLY the low 9 permission bits (0o777) in the low region; the Unix
/// setuid/setgid/sticky bits (0o4000/0o2000/0o1000) and the file-type live in dedicated HIGH bits. Leaving
/// the raw 0o7000 bits in the low region (as the old `& 0o7777` did) mis-encodes a setuid/sticky file:
/// the CLI would neither see ModeSetuid nor a clean perm value. Mirror the Go bit layout exactly.
pub(crate) fn go_filemode(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    let unix = md.permissions().mode();
    let mut m = unix & 0o777; // Go os.FileMode: low 9 bits are the Unix perm bits, and ONLY those
    let ft = md.file_type();
    if ft.is_dir() {
        m |= 1 << 31; // ModeDir
    }
    if ft.is_symlink() {
        m |= 1 << 27; // ModeSymlink
    }
    if ft.is_fifo() {
        m |= 1 << 25; // ModeNamedPipe
    }
    if ft.is_socket() {
        m |= 1 << 24; // ModeSocket
    }
    if ft.is_block_device() {
        m |= 1 << 26; // ModeDevice
    }
    if ft.is_char_device() {
        m |= (1 << 26) | (1 << 21); // ModeDevice | ModeCharDevice
    }
    if unix & 0o4000 != 0 {
        m |= 1 << 23; // ModeSetuid
    }
    if unix & 0o2000 != 0 {
        m |= 1 << 22; // ModeSetgid
    }
    if unix & 0o1000 != 0 {
        m |= 1 << 20; // ModeSticky
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
        Mount { typ: "volume".into(), source: "vol".into(), target: target.into(), read_only: true, bind_options: None }
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
            read_only: false, bind_options: None }];
        let t = readonly_mount_targets(&binds, &mounts);
        assert!(t.contains(&"/data".to_string()), "ro bind target");
        assert!(t.contains(&"/named".to_string()), "ro,z bind target");
        assert!(t.contains(&"/mnt/ro".to_string()), "ro --mount target");
        assert!(!t.contains(&"/rw".to_string()), "rw bind not included");
        assert!(!t.contains(&"/mnt/rw".to_string()), "rw mount not included");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "dd-overlay-test-{}-{}-{}",
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

    // Finding 24 (P1 correctness): `docker cp container:/dir -` must tar the MERGED overlay view, not just
    // the physical upper dir. A file that lives only in the lower image (`/etc/alpine-release`) plus a file
    // the container wrote to the upper (`/etc/hosts`) must BOTH appear; `.wh.NAME` whiteouts hide lower
    // entries (and never leak the marker); `.wh..wh..opq` makes the upper opaque (all lower dropped).
    #[test]
    fn stage_overlay_merge_unions_layers_and_honors_whiteouts_and_opaque() {
        let root = scratch("merge");
        let upper = root.join("upper/etc");
        let lower = root.join("lower/etc");
        std::fs::create_dir_all(&upper).unwrap();
        std::fs::create_dir_all(&lower).unwrap();
        // lower-only, upper-only, a shadowed file, a whited-out file
        std::fs::write(lower.join("alpine-release"), b"3.19\n").unwrap();
        std::fs::write(lower.join("shadowed"), b"lower\n").unwrap();
        std::fs::write(lower.join("secret"), b"lower-secret\n").unwrap();
        std::fs::write(upper.join("hosts"), b"127.0.0.1\n").unwrap();
        std::fs::write(upper.join("shadowed"), b"upper\n").unwrap();
        std::fs::write(upper.join(".wh.secret"), b"").unwrap(); // whiteout hides lower `secret`

        let dest = scratch("merge-out").join("etc");
        stage_overlay_merge(&upper, &lower, &dest).unwrap();

        assert!(dest.join("alpine-release").exists(), "lower-only entry dropped from merge");
        assert!(dest.join("hosts").exists(), "upper-only entry missing from merge");
        assert_eq!(std::fs::read(dest.join("shadowed")).unwrap(), b"upper\n", "upper must shadow lower");
        assert!(!dest.join("secret").exists(), "whiteout did not hide lower entry");
        assert!(!dest.join(".wh.secret").exists(), "whiteout control file leaked into tar view");

        // Opaque upper dir hides ALL lower entries at that level.
        let uo = root.join("opq-upper/d");
        let lo = root.join("opq-lower/d");
        std::fs::create_dir_all(&uo).unwrap();
        std::fs::create_dir_all(&lo).unwrap();
        std::fs::write(lo.join("fromlower"), b"x").unwrap();
        std::fs::write(uo.join("fromupper"), b"y").unwrap();
        std::fs::write(uo.join(".wh..wh..opq"), b"").unwrap();
        let od = scratch("opq-out").join("d");
        stage_overlay_merge(&uo, &lo, &od).unwrap();
        assert!(od.join("fromupper").exists(), "opaque upper entry missing");
        assert!(!od.join("fromlower").exists(), "opaque dir must hide all lower entries");
        assert!(!od.join(".wh..wh..opq").exists(), "opaque marker leaked into tar view");

        let _ = std::fs::remove_dir_all(&root);
    }

    // Finding 26: docker's path-stat FileMode must follow Go's `os.FileMode` bit layout. Setuid/setgid/
    // sticky belong in the HIGH bits (ModeSetuid=1<<23, ...), NOT left in the raw 0o7000 low region; the
    // perm value is the clean low 9 bits.
    #[test]
    fn go_filemode_encodes_setuid_in_go_high_bits_not_low() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("suid");
        let f = root.join("suid");
        std::fs::write(&f, b"").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o4755)).unwrap();
        let md = std::fs::symlink_metadata(&f).unwrap();
        let m = go_filemode(&md);
        assert_ne!(m & (1 << 23), 0, "ModeSetuid high bit must be set");
        assert_eq!(m & 0o777, 0o755, "perm bits must be the clean low 9 bits");
        assert_eq!(m & 0o7000, 0, "raw setuid/setgid/sticky must NOT remain in the low region");
        let _ = std::fs::remove_dir_all(&root);
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
