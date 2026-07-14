//! Per-instruction executors extracted verbatim from the `images_build` step loop, plus the build
//! layer-cache descriptor helper. Shared types/helpers come from `mod.rs` via `use super::*`; each helper
//! is a self-contained unit of the former inline loop-body, so behavior is unchanged.
use super::*;

/// The content-addressed **cache descriptor** for one build step: a normalized rendering of the
/// instruction. `COPY`/`ADD` fold in a content digest of each source (so a changed build context or
/// source stage invalidates); `ARG` folds in its *resolved* value (so a `--build-arg` change invalidates
/// the rest of the build even when the arg is unreferenced). `nonce` is mixed in when a COPY/ADD source
/// digest is unavailable, forcing a miss rather than risking a stale layer.
pub(super) fn cache_desc(
    inst: &str,
    args: &str,
    stage_names: &HashMap<String, usize>,
    stages: &[std::path::PathBuf],
    ctx: &std::path::Path,
    nonce: &str,
    buildargs: &HashMap<String, String>,
) -> String {
    match inst {
        "COPY" | "ADD" => {
            let from_stage = args
                .split_whitespace()
                .find_map(|p| p.strip_prefix("--from="));
            let parts: Vec<&str> = args
                .split_whitespace()
                .filter(|p| !p.starts_with("--"))
                .collect();
            let mut d = format!("{inst} {args}");
            if parts.len() >= 2 {
                let src_root = match from_stage {
                    Some(s) => stage_names.get(s).map(|&idx| stages[idx].clone()),
                    None => Some(ctx.to_path_buf()),
                };
                match src_root {
                    Some(root) => {
                        for src in &parts[..parts.len() - 1] {
                            let sp = if from_stage.is_some() {
                                archive_host_path(&root.to_string_lossy(), &[], "", src)
                            } else {
                                root.join(src)
                            };
                            let dg = path_digest(&sp);
                            d.push('\n');
                            d.push_str(if dg.is_empty() { nonce } else { &dg });
                        }
                    }
                    None => d.push_str("\n?unknown-stage"),
                }
            }
            d
        }
        "ARG" => {
            let spec = args.split_whitespace().next().unwrap_or("");
            let kv = match spec.split_once('=') {
                Some((k, v)) => format!(
                    "{k}={}",
                    buildargs.get(k).cloned().unwrap_or_else(|| v.to_string())
                ),
                None => match buildargs.get(spec) {
                    Some(v) => format!("{spec}={v}"),
                    None => spec.to_string(),
                },
            };
            format!("ARG {kv}")
        }
        _ => format!("{inst} {args}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    // Helper: exercise cache_desc for the PURE branches only (ARG + default). The COPY/ADD-only
    // params (stage_names/stages/ctx/nonce) are unused by these branches, so they are passed empty.
    fn desc(inst: &str, args: &str, buildargs: &HashMap<String, String>) -> String {
        let stage_names: HashMap<String, usize> = HashMap::new();
        let stages: Vec<PathBuf> = Vec::new();
        let ctx = Path::new("/nonexistent");
        cache_desc(inst, args, &stage_names, &stages, ctx, "NONCE", buildargs)
    }

    #[test]
    fn arg_with_default_no_override() {
        // `ARG FOO=bar` with no matching --build-arg resolves to the declared default.
        assert_eq!(desc("ARG", "FOO=bar", &HashMap::new()), "ARG FOO=bar");
    }
    #[test]
    fn arg_default_overridden_by_buildarg() {
        // A --build-arg override replaces the declared default in the resolved descriptor.
        let mut ba = HashMap::new();
        ba.insert("FOO".to_string(), "baz".to_string());
        assert_eq!(desc("ARG", "FOO=bar", &ba), "ARG FOO=baz");
    }
    #[test]
    fn arg_bare_no_default_not_in_buildargs() {
        // `ARG FOO` with no default and no override: descriptor is just the bare name.
        assert_eq!(desc("ARG", "FOO", &HashMap::new()), "ARG FOO");
    }
    #[test]
    fn arg_bare_resolved_from_buildargs() {
        // `ARG FOO` (no default) but a --build-arg supplies the value.
        let mut ba = HashMap::new();
        ba.insert("FOO".to_string(), "xyz".to_string());
        assert_eq!(desc("ARG", "FOO", &ba), "ARG FOO=xyz");
    }
    #[test]
    fn arg_only_first_token_considered() {
        // Only the first whitespace token is the arg spec.
        assert_eq!(desc("ARG", "FOO=bar EXTRA", &HashMap::new()), "ARG FOO=bar");
    }

    #[test]
    fn default_branch_passthrough() {
        // ENV/CMD/LABEL/... fall through to the verbatim `"{inst} {args}"` descriptor.
        assert_eq!(desc("ENV", "FOO=bar", &HashMap::new()), "ENV FOO=bar");
        assert_eq!(desc("CMD", "[\"sh\"]", &HashMap::new()), "CMD [\"sh\"]");
        assert_eq!(desc("USER", "root", &HashMap::new()), "USER root");
    }

    // --- copy_step filesystem-behavior regression tests (shell out to tar/cp) ---

    fn scratch(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir()
            .join(format!("dd-steps-test-{}-{}-{}", label, std::process::id(), nanos));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // Dockerfile `ADD archive.tar /out/` must EXTRACT the local archive into the destination, not copy the
    // tar file itself (a common Docker compatibility expectation).
    #[test]
    fn add_local_tar_is_extracted_into_destination() {
        let base = scratch("add-tar");
        let ctx = base.join("ctx");
        let rootfs = base.join("rootfs");
        let inner = base.join("inner");
        std::fs::create_dir_all(&ctx).unwrap();
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("inside.txt"), b"extracted!\n").unwrap();
        let ok = std::process::Command::new("tar")
            .arg("cf").arg(ctx.join("payload.tar"))
            .arg("-C").arg(&inner).arg("inside.txt")
            .status().unwrap().success();
        assert!(ok, "build the fixture tar");

        let sn: HashMap<String, usize> = HashMap::new();
        let stages: Vec<PathBuf> = Vec::new();
        copy_step("ADD", "payload.tar /out/", &rootfs, "/", &sn, &stages, &ctx)
            .expect("ADD extract");

        assert!(rootfs.join("out/inside.txt").is_file(), "archive contents extracted into dest");
        assert!(!rootfs.join("out/payload.tar").exists(), "archive file itself must not be copied");
        let _ = std::fs::remove_dir_all(&base);
    }

    // Plain `COPY` still copies a tar verbatim (only ADD auto-extracts).
    #[test]
    fn copy_local_tar_is_not_extracted() {
        let base = scratch("copy-tar");
        let ctx = base.join("ctx");
        let rootfs = base.join("rootfs");
        let inner = base.join("inner");
        std::fs::create_dir_all(&ctx).unwrap();
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("inside.txt"), b"x\n").unwrap();
        assert!(std::process::Command::new("tar")
            .arg("cf").arg(ctx.join("payload.tar")).arg("-C").arg(&inner).arg("inside.txt")
            .status().unwrap().success());

        let sn: HashMap<String, usize> = HashMap::new();
        let stages: Vec<PathBuf> = Vec::new();
        copy_step("COPY", "payload.tar /out/", &rootfs, "/", &sn, &stages, &ctx)
            .expect("COPY tar");

        assert!(rootfs.join("out/payload.tar").is_file(), "COPY copies the tar verbatim");
        assert!(!rootfs.join("out/inside.txt").exists(), "COPY must not extract");
        let _ = std::fs::remove_dir_all(&base);
    }

    // A pre-existing symlink at the COPY destination must NOT be followed: the copy must land at the
    // literal rootfs path, never write through the symlink to an outside directory.
    #[test]
    fn copy_does_not_follow_symlinked_destination() {
        use std::os::unix::fs::symlink;
        let base = scratch("copy-symdst");
        let ctx = base.join("ctx");
        let rootfs = base.join("rootfs");
        let outside = base.join("outside");
        std::fs::create_dir_all(&ctx).unwrap();
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(ctx.join("payload"), b"data\n").unwrap();
        // rootfs/dstlink -> outside (a directory outside the requested path).
        symlink(&outside, rootfs.join("dstlink")).unwrap();

        let sn: HashMap<String, usize> = HashMap::new();
        let stages: Vec<PathBuf> = Vec::new();
        copy_step("COPY", "payload dstlink", &rootfs, "/", &sn, &stages, &ctx)
            .expect("COPY to symlinked dest");

        assert!(!outside.join("payload").exists(), "copy must not write through the dest symlink");
        let md = std::fs::symlink_metadata(rootfs.join("dstlink")).unwrap();
        assert!(!md.file_type().is_symlink(), "dest symlink replaced by a real entry");
        assert!(rootfs.join("dstlink").is_file(), "payload landed at the literal dest path");
        let _ = std::fs::remove_dir_all(&base);
    }

    // Finding 1: `COPY --chmod=0755 file /bin/tool` applies the mode to the copied destination.
    #[test]
    fn copy_chmod_applies_mode_to_destination() {
        use std::os::unix::fs::PermissionsExt;
        let base = scratch("copy-chmod");
        let ctx = base.join("ctx");
        let rootfs = base.join("rootfs");
        std::fs::create_dir_all(&ctx).unwrap();
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::write(ctx.join("file"), b"tool\n").unwrap();
        // A deliberately-different source mode, so a preserved 0755 proves --chmod took effect.
        std::fs::set_permissions(ctx.join("file"), std::fs::Permissions::from_mode(0o600)).unwrap();

        let sn: HashMap<String, usize> = HashMap::new();
        let stages: Vec<PathBuf> = Vec::new();
        copy_step("COPY", "--chmod=0755 file /bin/tool", &rootfs, "/", &sn, &stages, &ctx)
            .expect("COPY --chmod");

        let mode = std::fs::metadata(rootfs.join("bin/tool")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "destination has the requested 0755 mode");
        let _ = std::fs::remove_dir_all(&base);
    }

    // Finding 1: `COPY --chown=U:G` issues a numeric ownership request. Real chown needs root; here we
    // assert the copy still lands and (when running privileged) that ownership is applied, else that the
    // chown parser resolves the numeric ids that get requested via lchown.
    #[test]
    fn copy_chown_numeric_is_parsed_and_copy_succeeds() {
        let base = scratch("copy-chown");
        let ctx = base.join("ctx");
        let rootfs = base.join("rootfs");
        std::fs::create_dir_all(&ctx).unwrap();
        std::fs::create_dir_all(&rootfs).unwrap();
        std::fs::write(ctx.join("file"), b"x\n").unwrap();

        // The numeric chown parse is what feeds the (best-effort, root-only) lchown request.
        assert_eq!(parse_numeric_chown("1000:1000"), (Some(1000), Some(1000)));
        assert_eq!(parse_numeric_chown("1000"), (Some(1000), None));
        assert_eq!(parse_numeric_chown("root"), (None, None), "symbolic names aren't numeric-resolved");

        let sn: HashMap<String, usize> = HashMap::new();
        let stages: Vec<PathBuf> = Vec::new();
        copy_step("COPY", "--chown=1000:1000 file /tool", &rootfs, "/", &sn, &stages, &ctx)
            .expect("COPY --chown");
        assert!(rootfs.join("tool").is_file(), "the file is copied regardless of chown privilege");
        // If privileged, ownership actually applied; otherwise lchown is a no-op (needs root) — asserted
        // conditionally so the test is meaningful as root and passes unprivileged.
        if unsafe { libc::geteuid() } == 0 {
            use std::os::unix::fs::MetadataExt;
            let md = std::fs::metadata(rootfs.join("tool")).unwrap();
            assert_eq!(md.uid(), 1000, "root run applies the requested uid");
            assert_eq!(md.gid(), 1000, "root run applies the requested gid");
        }
        let _ = std::fs::remove_dir_all(&base);
    }
}

/// Execute a `RUN` instruction in the JIT against the current stage's rootfs. stdout/stderr are appended
/// to `log` (as they occur, before any failure) so a failing build still reports the command's output;
/// returns `Err(message)` on a non-zero exit / spawn failure for the caller to surface via `build_err`.
pub(super) async fn run_step(
    arch: Guest,
    workdir: &str,
    rootfs: &std::path::Path,
    env: &[String],
    args: &str,
    shell: &[String],
    log: &mut Vec<String>,
) -> Result<(), String> {
    // Shell-form RUN uses the current SHELL (`SHELL ["/bin/bash","-c"]`); exec-form RUN (`["a","b"]`)
    // runs argv verbatim (finding 9). Default shell is `/bin/sh -c`.
    let argv: Vec<String> = {
        let a = args.trim();
        if a.starts_with('[') {
            if let Ok(serde_json::Value::Array(v)) = serde_json::from_str::<serde_json::Value>(a) {
                v.into_iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            } else {
                let mut s = shell.to_vec();
                s.push(a.to_string());
                s
            }
        } else {
            let mut s = shell.to_vec();
            s.push(a.to_string());
            s
        }
    };
    let shell_desc = shell.join(" ");
    // Build the RUN step's container spec through the typed dd-jit API. CRITICAL: route the step's env
    // (loose `K=V` lines: image ENV + Dockerfile ENV/ARG) through `.guest_env`, which encodes it into
    // `HL_GUEST_ENV` — the launch_config mapper only translates known `DD_*`/`DDJIT_*` keys and would drop
    // arbitrary RUN env otherwise, so a plain `.env()` per pair would silently lose the step's environment.
    let mut builder = hl_jit::Container::builder(
        hl_jit::Image::from_rootfs(rootfs.to_string_lossy().into_owned()).guest(arch),
    )
    .host_workdir(workdir.to_string())
    .cmd(argv)
    .guest_env(env, false);
    // WORKDIR must set the GUEST cwd for the RUN, not just host-side path resolution: `WORKDIR /app`
    // followed by `RUN pwd` should print `/app`. `.host_workdir` only feeds host ADD/COPY resolution;
    // `.workdir` populates LaunchConfig.cwd. Empty workdir leaves the guest's default cwd untouched.
    if !workdir.is_empty() {
        builder = builder.workdir(workdir.to_string());
    }
    let container = builder;
    let container = match container.build() {
        Ok(c) => c,
        Err(e) => return Err(format!("RUN: {e}")),
    };
    let rt = match hl_jit::Runtime::new() {
        Ok(r) => r,
        Err(e) => return Err(format!("RUN: {e}")),
    };
    // One-shot: run to completion, combined stdout+stderr, no timeout (a build RUN runs to the end).
    match rt.output(&container, None).await {
        Ok((code, bytes)) => {
            if !bytes.is_empty() {
                log.push(json!({"stream": String::from_utf8_lossy(&bytes)}).to_string());
            }
            if code != 0 {
                return Err(format!(
                    "The command '{} {}' returned a non-zero code: {}",
                    shell_desc, args, code
                ));
            }
        }
        Err(e) => return Err(format!("RUN failed to start: {e}")),
    }
    Ok(())
}

/// Whether `p` is a local archive that Dockerfile `ADD` auto-extracts (identity tar, gzip, bzip2, xz,
/// zstd). Detected by leading magic bytes plus the `ustar` magic at offset 257 for an uncompressed tar,
/// matching Docker's `archive.DecompressStream` sniffing. Only regular local files are candidates.
fn is_local_archive(p: &std::path::Path) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(p) else {
        return false;
    };
    let mut buf = [0u8; 262];
    let n = f.read(&mut buf).unwrap_or(0);
    if n >= 2 && buf[0] == 0x1f && buf[1] == 0x8b {
        return true; // gzip
    }
    if n >= 3 && &buf[0..3] == b"BZh" {
        return true; // bzip2
    }
    if n >= 6 && &buf[0..6] == b"\xfd7zXZ\x00" {
        return true; // xz
    }
    if n >= 4 && buf[0] == 0x28 && buf[1] == 0xb5 && buf[2] == 0x2f && buf[3] == 0xfd {
        return true; // zstd
    }
    // Uncompressed tar: POSIX ustar magic ("ustar\0" or "ustar  ") at offset 257.
    n >= 262 && &buf[257..262] == b"ustar"
}

/// Execute a `COPY`/`ADD` instruction: copy each source (from the build context, or `--from=<stage>`'s
/// rootfs) into the current stage's rootfs at the resolved destination. `ADD` of a local archive source
/// is extracted into the destination (Docker semantics); a pre-existing symlink at the destination is
/// replaced rather than followed, so writes cannot escape the requested rootfs path. Only touches the
/// filesystem; returns `Err(message)` for the caller to surface via `build_err`.
pub(super) fn copy_step(
    inst: &str,
    args: &str,
    rootfs: &std::path::Path,
    workdir: &str,
    stage_names: &HashMap<String, usize>,
    stages: &[std::path::PathBuf],
    ctx: &std::path::Path,
) -> Result<(), String> {
    let from_stage = args
        .split_whitespace()
        .find_map(|p| p.strip_prefix("--from="));
    // `COPY/ADD --chmod=MODE --chown=U[:G]` apply permissions/ownership to the copied destination
    // (finding 1). chmod is applied via mode bits; chown is numeric best-effort (needs root).
    let chmod = args
        .split_whitespace()
        .find_map(|p| p.strip_prefix("--chmod="))
        .and_then(parse_octal_mode);
    let (chown_uid, chown_gid) = args
        .split_whitespace()
        .find_map(|p| p.strip_prefix("--chown="))
        .map(parse_numeric_chown)
        .unwrap_or((None, None));
    let parts: Vec<&str> = args
        .split_whitespace()
        .filter(|p| !p.starts_with("--"))
        .collect();
    if parts.len() < 2 {
        return Err(format!("{inst} needs a source and destination"));
    }
    let dst = parts[parts.len() - 1];
    let dst_guest = if dst.starts_with('/') {
        dst.to_string()
    } else {
        format!("{}/{}", workdir.trim_end_matches('/'), dst)
    };
    let dst_host = archive_host_path(&rootfs.to_string_lossy(), &[], "", &dst_guest);
    // A pre-existing symlink at the destination leaf must not be followed: an attacker-controlled base
    // image or earlier COPY could leave `dst -> ../../etc`, and `cp -a`/`tar` would then write THROUGH it
    // to a different (possibly outside-rootfs) path than the Dockerfile requested. Replace it with a real
    // entry at the literal rootfs path so the copy lands where asked.
    if std::fs::symlink_metadata(&dst_host).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        let _ = std::fs::remove_file(&dst_host);
    }
    let into_dir = dst.ends_with('/') || parts.len() > 2;
    if into_dir {
        std::fs::create_dir_all(&dst_host).ok();
    } else if let Some(p) = dst_host.parent() {
        std::fs::create_dir_all(p).ok();
    }
    // COPY --from=<stage>: source is a path inside that stage's rootfs; else the build context.
    let src_root = match from_stage {
        Some(s) => match stage_names.get(s) {
            Some(&idx) => stages[idx].clone(),
            None => return Err(format!("COPY --from: unknown stage '{s}'")),
        },
        None => ctx.to_path_buf(),
    };
    for src in &parts[..parts.len() - 1] {
        let src_host = if from_stage.is_some() {
            archive_host_path(&src_root.to_string_lossy(), &[], "", src)
        } else {
            src_root.join(src)
        };
        // Dockerfile `ADD` extracts a LOCAL archive source (tar/gzip/bzip2/xz/zstd) into the destination
        // directory instead of copying the archive file itself. `COPY` never extracts, and `--from` stage
        // sources are treated as plain files (Docker does not auto-extract those either).
        if inst == "ADD" && from_stage.is_none() && src_host.is_file() && is_local_archive(&src_host) {
            let _ = std::fs::create_dir_all(&dst_host);
            // Refuse a traversal-laden archive before extracting it into the stage rootfs.
            if let Err(e) = crate::util::tar_members_contained(&src_host) {
                return Err(format!("{inst} {src}: {e}"));
            }
            if !matches!(std::process::Command::new("tar").arg("--no-same-owner").arg("-xf").arg(&src_host).arg("-C").arg(&dst_host).status(), Ok(s) if s.success())
            {
                return Err(format!("{inst} {src}: failed to extract archive"));
            }
            apply_copy_perms(&dst_host, chmod, chown_uid, chown_gid);
            continue;
        }
        if !matches!(std::process::Command::new("cp").arg("-a").arg(&src_host).arg(&dst_host).status(), Ok(s) if s.success())
        {
            return Err(format!("{inst} {src}: not found"));
        }
        // The copied entry lands at `dst_host` (single-file dest) or `dst_host/<basename>` (into a dir).
        let target = if into_dir {
            match std::path::Path::new(src).file_name() {
                Some(name) => dst_host.join(name),
                None => dst_host.clone(),
            }
        } else {
            dst_host.clone()
        };
        apply_copy_perms(&target, chmod, chown_uid, chown_gid);
    }
    Ok(())
}

/// Parse a `--chmod=` octal mode (`0755`/`755`) into permission bits, or `None` if malformed.
fn parse_octal_mode(s: &str) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return None;
    }
    u32::from_str_radix(t, 8).ok()
}

/// Parse a `--chown=` value into NUMERIC (uid, gid). Symbolic names can't be resolved against the target
/// rootfs here, so only numeric ids are honored (best-effort, matching the finding); a bare `U` sets uid.
fn parse_numeric_chown(s: &str) -> (Option<u32>, Option<u32>) {
    let (u, g) = match s.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (s, None),
    };
    (
        u.parse::<u32>().ok(),
        g.and_then(|g| g.parse::<u32>().ok()),
    )
}

/// Apply `--chmod`/`--chown` to a copied destination, recursing into directories. chmod uses mode bits
/// (works unprivileged); chown is a best-effort numeric `lchown` (a no-op without root — `-1` leaves a
/// component unchanged), so the correct request is always issued even where privilege is unavailable.
fn apply_copy_perms(path: &std::path::Path, mode: Option<u32>, uid: Option<u32>, gid: Option<u32>) {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::PermissionsExt;
    if mode.is_none() && uid.is_none() && gid.is_none() {
        return;
    }
    let is_symlink = std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false);
    if let Some(m) = mode {
        if !is_symlink {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(m));
        }
    }
    if uid.is_some() || gid.is_some() {
        if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
            // -1 (u32::MAX cast) leaves that id unchanged.
            let u = uid.unwrap_or(u32::MAX);
            let g = gid.unwrap_or(u32::MAX);
            unsafe {
                libc::lchown(c.as_ptr(), u, g);
            }
        }
    }
    if !is_symlink && path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                apply_copy_perms(&e.path(), mode, uid, gid);
            }
        }
    }
}
