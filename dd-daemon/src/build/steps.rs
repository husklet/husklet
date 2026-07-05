#![allow(unused_imports, dead_code)]
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

/// Execute a `RUN` instruction in the JIT against the current stage's rootfs. stdout/stderr are appended
/// to `log` (as they occur, before any failure) so a failing build still reports the command's output;
/// returns `Err(message)` on a non-zero exit / spawn failure for the caller to surface via `build_err`.
pub(super) async fn run_step(
    arch: Guest,
    workdir: &str,
    rootfs: &std::path::Path,
    env: &[String],
    args: &str,
    log: &mut Vec<String>,
) -> Result<(), String> {
    // Build the RUN step's container spec through the typed dd-jit API. CRITICAL: route the step's env
    // (loose `K=V` lines: image ENV + Dockerfile ENV/ARG) through `.guest_env`, which encodes it into
    // `DD_GUEST_ENV` — the launch_config mapper only translates known `DD_*`/`DDJIT_*` keys and would drop
    // arbitrary RUN env otherwise, so a plain `.env()` per pair would silently lose the step's environment.
    let container = ddjit::Container::builder(
        ddjit::Image::from_rootfs(rootfs.to_string_lossy().into_owned()).guest(arch),
    )
    .host_workdir(workdir.to_string())
    .cmd(vec!["/bin/sh".to_string(), "-c".to_string(), args.to_string()])
    .guest_env(env, false);
    let container = match container.build() {
        Ok(c) => c,
        Err(e) => return Err(format!("RUN: {e}")),
    };
    let rt = match ddjit::Runtime::new() {
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
                    "The command '/bin/sh -c {}' returned a non-zero code: {}",
                    args, code
                ));
            }
        }
        Err(e) => return Err(format!("RUN failed to start: {e}")),
    }
    Ok(())
}

/// Execute a `COPY`/`ADD` instruction: copy each source (from the build context, or `--from=<stage>`'s
/// rootfs) into the current stage's rootfs at the resolved destination. Only touches the filesystem;
/// returns `Err(message)` for the caller to surface via `build_err`.
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
        if !matches!(std::process::Command::new("cp").arg("-a").arg(&src_host).arg(&dst_host).status(), Ok(s) if s.success())
        {
            return Err(format!("{inst} {src}: not found"));
        }
    }
    Ok(())
}
