//! The `POST /build` axum handler: request/`--build-arg`/`--target`/`--no-cache`/`--label` parsing, the
//! multi-stage step-loop driver with the content-addressed build layer cache, and final image
//! registration + progress/response streaming. Per-instruction execution (RUN/COPY/ADD) and the
//! cache-descriptor live in `steps`; shared helpers/types come from `mod.rs` via `use super::*`.
use super::*;

/// Unpack the build-context tar (`body`) into `ctx`. The tar is staged OUTSIDE `ctx` (a sibling temp file
/// under `images_dir`) and deleted after extraction, so `.context.tar` never lands inside the context
/// tree — otherwise a `COPY . /app` would sweep the raw context archive into the image (wrong contents +
/// bloat). Returns `Err(msg)` on write/extraction failure (the caller then cleans up `ctx`).
fn unpack_build_context(
    body: &[u8],
    ctx: &std::path::Path,
    _images_dir: &str,
) -> Result<(), String> {
    // Stage the tar as a sibling of `ctx` (never inside it), deriving its name from the per-request
    // unique `ctx` dir name so two concurrent builds don't collide on one `.build-ctx-<pid>.tar`.
    let ctar = std::path::PathBuf::from(format!("{}.tar", ctx.to_string_lossy()));
    let _ = std::fs::remove_file(&ctar);
    if std::fs::write(&ctar, body).is_err() {
        return Err("cannot write context".into());
    }
    // Refuse a traversal-laden context (absolute / `..` members) before extracting; `--no-same-owner`
    // avoids chowning extracted files to arbitrary uids.
    if let Err(e) = crate::util::tar_members_contained(&ctar) {
        let _ = std::fs::remove_file(&ctar);
        return Err(e);
    }
    let ok = matches!(
        std::process::Command::new("tar").arg("--no-same-owner").arg("-xf").arg(&ctar).arg("-C").arg(ctx).status(),
        Ok(s) if s.success()
    );
    let _ = std::fs::remove_file(&ctar);
    if !ok {
        return Err("cannot unpack build context".into());
    }
    Ok(())
}

/// Read the Dockerfile named `dfname` from the build context `ctx`, REFUSING one that (via symlinks)
/// resolves outside `ctx`. The Dockerfile source must be deterministic from the submitted context, never
/// read from an arbitrary host path a symlinked tar member (`Dockerfile -> ../outside/x`) points at.
/// Returns `None` if it is missing or escapes the context. An in-context symlink is allowed (it still
/// canonicalizes under `ctx`).
fn read_context_dockerfile(ctx: &std::path::Path, dfname: &str) -> Option<String> {
    let df = ctx.join(dfname);
    let root = std::fs::canonicalize(ctx).ok()?;
    let real = std::fs::canonicalize(&df).ok()?;
    if !real.starts_with(&root) {
        return None;
    }
    std::fs::read_to_string(&real).ok()
}

/// Normalize an (absolute) Dockerfile path lexically: collapse `.`/empty segments and resolve `..`
/// WITHOUT touching the filesystem. Used for `WORKDIR` so `Config.WorkingDir` matches the directory
/// actually created (`WORKDIR ../c` from `/a/b` -> `/a/c`, not the un-normalized `/a/b/../c`) (finding 15).
fn normalize_abs_path(p: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

/// Resolve a RUN/CMD/ENTRYPOINT argument to its argv: JSON exec-form (`["a","b"]`) is used verbatim;
/// otherwise it is SHELL-form, wrapped in the current `shell` (default `/bin/sh -c`, overridable by the
/// `SHELL` instruction) (finding 9).
fn exec_or_shell(args: &str, shell: &[String]) -> Vec<String> {
    let a = args.trim();
    if a.starts_with('[') {
        if let Ok(serde_json::Value::Array(v)) = serde_json::from_str::<serde_json::Value>(a) {
            return v
                .into_iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect();
        }
    }
    let mut out = shell.to_vec();
    out.push(a.to_string());
    out
}

/// Match one `.dockerignore` pattern against a context-relative path. Supports Docker ignore semantics:
/// `*`/`?` within a path segment, `**` spanning segments, and a directory pattern excluding its whole
/// subtree (the pattern matches any path PREFIX). Leading/trailing `/` are stripped by the caller.
fn di_wildcard_seg(pat: &str, s: &str) -> bool {
    // glob match of a single path segment: `*` (any run, no `/`), `?` (one char), literals.
    fn m(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        match p[0] {
            b'*' => m(&p[1..], s) || (!s.is_empty() && m(p, &s[1..])),
            b'?' => !s.is_empty() && m(&p[1..], &s[1..]),
            c => !s.is_empty() && s[0] == c && m(&p[1..], &s[1..]),
        }
    }
    m(pat.as_bytes(), s.as_bytes())
}

fn di_seg_match(p: &[&str], t: &[&str]) -> bool {
    if p.is_empty() {
        return t.is_empty();
    }
    if p[0] == "**" {
        // `**` matches zero or more path segments.
        (0..=t.len()).any(|i| di_seg_match(&p[1..], &t[i..]))
    } else if t.is_empty() {
        false
    } else if di_wildcard_seg(p[0], t[0]) {
        di_seg_match(&p[1..], &t[1..])
    } else {
        false
    }
}

/// True if `rel` (a context-relative slash path) is matched by ignore pattern `pat`, either directly or
/// because one of its parent directories matches (so a dir pattern excludes its subtree).
fn di_pattern_matches(pat: &str, rel: &str) -> bool {
    let p: Vec<&str> = pat.split('/').filter(|s| !s.is_empty()).collect();
    let t: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
    if p.is_empty() {
        return false;
    }
    // Match the pattern against every path prefix, so `node_modules` excludes `node_modules/x/y`.
    (1..=t.len()).any(|k| di_seg_match(&p, &t[..k]))
}

/// A parsed `.dockerignore`: `(negated, pattern)` in file order. `!` re-includes (last match wins).
fn parse_dockerignore(ctx: &std::path::Path) -> Vec<(bool, String)> {
    let Ok(content) = std::fs::read_to_string(ctx.join(".dockerignore")) else {
        return Vec::new();
    };
    content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.strip_prefix('!') {
            Some(rest) => (true, rest.trim().trim_matches('/').to_string()),
            None => (false, l.trim_matches('/').to_string()),
        })
        .filter(|(_, p)| !p.is_empty())
        .collect()
}

/// Whether a context-relative path is excluded by the parsed ignore patterns (last match wins; `!`
/// re-includes). The Dockerfile in use and `.dockerignore` itself are never excluded.
fn di_excluded(rel: &str, patterns: &[(bool, String)]) -> bool {
    let mut ex = false;
    for (neg, pat) in patterns {
        if di_pattern_matches(pat, rel) {
            ex = !neg;
        }
    }
    ex
}

/// Apply `.dockerignore` to the extracted build context: delete every file matching an ignore pattern
/// so a later `COPY .`/`ADD .` cannot include it (finding 6). The Dockerfile in use and `.dockerignore`
/// are always preserved; emptied directories are pruned best-effort.
fn apply_dockerignore(ctx: &std::path::Path, dfname: &str) {
    let patterns = parse_dockerignore(ctx);
    if patterns.is_empty() {
        return;
    }
    // Collect leaf files (relative slash paths); decide exclusion per file so `!` negation works at file
    // granularity even under an excluded directory.
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && !p.is_symlink() {
                    walk(&p, files);
                } else {
                    files.push(p);
                }
            }
        }
    }
    walk(ctx, &mut files);
    for f in files {
        let Ok(rel) = f.strip_prefix(ctx) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel == dfname || rel == ".dockerignore" {
            continue;
        }
        if di_excluded(&rel, &patterns) {
            let _ = std::fs::remove_file(&f);
        }
    }
    // Prune now-empty directories (deepest first), best-effort; never remove ctx itself.
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    fn walk_dirs(dir: &std::path::Path, dirs: &mut Vec<std::path::PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && !p.is_symlink() {
                    walk_dirs(&p, dirs);
                    dirs.push(p);
                }
            }
        }
    }
    walk_dirs(ctx, &mut dirs);
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(&d); // only succeeds if empty
    }
}

/// The base image config a `FROM` carries onto a new stage: rootfs to copy plus the OCI config a child
/// inherits (arch/cmd/entrypoint/env/workdir/labels/user) and the base's ONBUILD triggers to replay.
struct BaseCfg {
    rootfs: String,
    arch: Guest,
    cmd: Vec<String>,
    entrypoint: Vec<String>,
    env: Vec<String>,
    workdir: String,
    labels: HashMap<String, String>,
    user: String,
    onbuild: Vec<String>,
}

#[derive(Deserialize)]
pub(crate) struct BuildQ {
    t: Option<String>,
    dockerfile: Option<String>,
    // `docker build --build-arg K=V` -> a URL-encoded JSON object, e.g. {"VERSION":"1.2"}
    buildargs: Option<String>,
    // `docker build --target <stage>` -> stop after this stage in a multi-stage build
    target: Option<String>,
    // `docker build --no-cache` -> "1"/"true"; bypasses the build layer cache entirely (see images_build)
    nocache: Option<String>,
    // `docker build --label K=V` -> a URL-encoded JSON object, e.g. {"team":"infra"}; applied over
    // any `LABEL` instructions in the Dockerfile.
    labels: Option<String>,
}

pub(crate) async fn images_build(
    State(a): State<App>,
    Query(q): Query<BuildQ>,
    body: axum::body::Bytes,
) -> Response {
    let raw_tag =
        q.t.clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "built:latest".into());
    // Register under the FULL normalized tag (keeping any namespace + an explicit/implicit tag), exactly
    // as `pull` stores images, so a namespaced/tagged build (`-t org/app:v2`) round-trips and a later
    // `docker run org/app:v2` finds it. `ref_name` alone would collapse it to a bare `app`, dropping the
    // tag from `RepoTags` and colliding distinct images that share a short name.
    let name = image_ref(&raw_tag, "").short();
    let dfname = q
        .dockerfile
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "Dockerfile".into());
    let mut log: Vec<String> = Vec::new();

    // --build-arg: decode the JSON object (values may be null) into a name->value map.
    let buildargs: HashMap<String, String> = parse_build_args(q.buildargs.as_deref());
    // --target: name of the stage to stop at (empty = build every stage, as before).
    let target = q.target.clone().unwrap_or_default();
    // --no-cache: bypass the build layer cache entirely (never read, never write) — a from-scratch build
    // identical to the pre-cache behavior. Otherwise the per-step layer cache is active (see below).
    let nocache = matches!(q.nocache.as_deref(), Some("1") | Some("true"));
    let use_cache = !nocache;

    // unpack the build context (a tar in the request body). The staging dir is unique per request
    // (`.build-ctx-<pid>-<seq>`): a bare `<pid>` collides when two builds run concurrently in one daemon
    // process — one would wipe the other's context mid-build.
    let ctx = std::path::PathBuf::from(format!(
        "{}/.build-ctx-{}-{}",
        a.images_dir,
        std::process::id(),
        next_staging_seq()
    ));
    let _ = std::fs::remove_dir_all(&ctx);
    if std::fs::create_dir_all(&ctx).is_err() {
        return build_err(log, "cannot create build dir".into());
    }
    let cleanup = |ctx: &std::path::Path| {
        let _ = std::fs::remove_dir_all(ctx);
    };
    if let Err(e) = unpack_build_context(&body[..], &ctx, &a.images_dir) {
        cleanup(&ctx);
        return build_err(log, e);
    }
    let dockerfile = match read_context_dockerfile(&ctx, &dfname) {
        Some(d) => d,
        None => {
            cleanup(&ctx);
            return build_err(log, format!("Cannot locate specified Dockerfile: {dfname}"));
        }
    };
    // `.dockerignore` at the context root prunes matching files from the extracted context so a later
    // `COPY .`/`ADD .` cannot include them (finding 6).
    apply_dockerignore(&ctx, &dfname);
    // Steps are a MUTABLE work list (not a fixed iterator): a base image's ONBUILD triggers are spliced
    // in right after the FROM that pulls them (finding 10).
    let mut steps = parse_dockerfile(&dockerfile);

    // `--target NAME` that names no stage is an error (not a silent build of the last stage). Validate
    // up front against the `FROM … AS <name>` names so we fail before creating any image output
    // (finding 11).
    if !target.is_empty() {
        let known_stages: Vec<String> = steps
            .iter()
            .filter(|(inst, _)| inst == "FROM")
            .filter_map(|(_, a)| {
                let w: Vec<&str> = a.split_whitespace().collect();
                w.iter()
                    .position(|x| x.eq_ignore_ascii_case("AS"))
                    .and_then(|p| w.get(p + 1))
                    .map(|s| s.to_string())
            })
            .collect();
        if !known_stages.iter().any(|s| s == &target) {
            cleanup(&ctx);
            return build_err(log, format!("target stage \"{target}\" could not be found"));
        }
    }

    // Reject a Dockerfile whose exec-form (JSON array) instruction has a non-string element — e.g.
    // `CMD ["echo", 123]`. Docker fails the build; hl previously filtered the bad element and built a
    // truncated command. Only a *valid JSON array* with a non-string trips this: a `[`-prefixed shell
    // command like `RUN [ -f x ]` is not valid JSON and stays shell-form. Validate up front so we fail
    // before creating any image output.
    for (inst, raw) in &steps {
        if matches!(inst.as_str(), "RUN" | "CMD" | "ENTRYPOINT" | "SHELL") {
            if let Err(e) = parse_exec_form_checked(raw) {
                cleanup(&ctx);
                return build_err(log, format!("{inst}: {e}"));
            }
        }
    }

    // the new image's rootfs dir under HL_IMAGES — derived from the user's raw `-t` so a bare tag keeps a
    // predictable dir name (`scen-built`), while a namespaced/tagged build still gets a distinct dir.
    let safe: String = safe_dir_name(&raw_tag);
    let img_dir = std::path::PathBuf::from(format!("{}/{}", a.images_dir, safe));
    let _ = std::fs::remove_dir_all(&img_dir);
    // On any failure AFTER this point, remove the partial image output dir (and the context) so a failed
    // build never leaves a half-written `images/<tag>` behind (finding 12).
    let fail = |log: Vec<String>, msg: String, ctx: &std::path::Path| -> Response {
        let _ = std::fs::remove_dir_all(ctx);
        let _ = std::fs::remove_dir_all(&img_dir);
        build_err(log, msg)
    };
    let mut rootfs = img_dir.join("rootfs"); // the CURRENT stage's rootfs (reassigned at each FROM)
    let mut stages: Vec<std::path::PathBuf> = Vec::new(); // stage index -> its rootfs (multi-stage)
    let mut stage_names: HashMap<String, usize> = HashMap::new(); // name/index -> stage index

    // image config built up across the instructions (inherited from the base at FROM, then mutated)
    let (mut arch, mut cmd, mut entrypoint, mut workdir, mut env, mut from_done) = (
        Guest::LinuxAarch64,
        Vec::<String>::new(),
        Vec::<String>::new(),
        String::new(),
        Vec::<String>::new(),
        false,
    );
    // Config.User (USER instruction), inherited from the base at FROM, persisted into the image (finding 13).
    let mut user = String::new();
    // Current SHELL for shell-form RUN/CMD/ENTRYPOINT (`SHELL ["/bin/bash","-c"]`); default `/bin/sh -c`
    // (finding 9). Reset to the default at each FROM.
    let mut shell: Vec<String> = vec!["/bin/sh".to_string(), "-c".to_string()];
    // ONBUILD triggers collected for the image being built (`ONBUILD X` stores X), persisted so a child
    // `FROM` this image replays them (finding 10).
    let mut onbuild: Vec<String> = Vec::new();
    // Per-instruction build history (`docker history`): one row per executed instruction (finding 3).
    let mut history: Vec<crate::model::HistoryEntry> = Vec::new();

    // STAGE-scoped ARG map (Dockerfile `ARG` defaults overridden by --build-arg), reset at each FROM.
    let mut args_map: HashMap<String, String> = HashMap::new();
    // GLOBAL (pre-FROM) ARG map: usable by FROM lines only, NOT by later stage instructions unless the
    // ARG is re-declared after FROM (finding 8).
    let mut global_args: HashMap<String, String> = HashMap::new();
    // image labels accumulated from `LABEL` instructions; inherited from the base at FROM (finding 5),
    // child LABEL overriding matching keys. The `--label` build option is merged on top after the loop.
    let mut labels: HashMap<String, String> = HashMap::new();
    // Runtime-metadata instructions the builder previously DROPPED: EXPOSE (declared ports), VOLUME
    // (anon-volume dirs), and HEALTHCHECK. No rootfs effect but belong in the built image's config so
    // `docker inspect`/run honor them (USER is already tracked above).
    let mut exposed_ports: Vec<String> = Vec::new();
    let mut img_volumes: Vec<String> = Vec::new();
    let mut healthcheck: Option<serde_json::Value> = None;
    // set once the --target stage has been fully built, so the next FROM stops the build.
    let mut target_built = false;

    // --- build layer cache chain state (reset at each FROM) ---
    // The snapshot/restore + step-metadata store lives in hl-images (runtime-agnostic); root it at the
    // daemon's buildcache dir. The chain-state bookkeeping below stays here (it drives the step loop).
    let bc = BuildCache::new(crate::util::buildcache_dir());
    let mut parent_id = String::new(); // cache id of the previous step (seeded from the base at FROM)
    let mut cache_ok = false; // false after the first miss: no more hits this stage (Docker rule)
    let mut pending_fs: Option<String> = None; // a cache-hit fs layer whose rootfs restore is deferred (lazy)
                                               // a per-build nonce, mixed into a COPY/ADD key only when its source digest is unavailable, so we force
                                               // a miss instead of risking a stale layer we cannot prove identical.
    let nonce = format!(
        "nonce:{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );

    // Index-driven loop over a MUTABLE step list so ONBUILD triggers can be spliced in after a FROM
    // (finding 10). `i` advances at the end of the body and before every mid-loop `continue`.
    let mut i = 0usize;
    while i < steps.len() {
        let (inst, raw_args) = steps[i].clone();
        let inst = inst.as_str();
        // Variable expansion (${VAR}/$VAR). Docker expands using the current ENV (highest precedence)
        // over the in-scope ARGs (finding 7). FROM sees ONLY the GLOBAL pre-FROM ARGs — stage ARGs/ENV
        // are not in scope for it — while every other instruction sees the stage's ARGs + ENV, NOT the
        // pre-FROM globals (finding 8).
        let vars: HashMap<String, String> = if inst == "FROM" {
            global_args.clone()
        } else {
            let mut m = args_map.clone();
            for e in &env {
                if let Some((k, v)) = e.split_once('=') {
                    m.insert(k.to_string(), v.to_string());
                }
            }
            m
        };
        let args = substitute_args(&raw_args, &vars);
        log.push(
            serde_json::to_string(&crate::api::BuildStream {
                stream: format!("Step {}/{} : {} {}\n", i + 1, steps.len(), inst, args),
            })
            .unwrap(),
        );
        // Record a `docker history` row for every instruction (config-only steps carry no fs layer).
        history.push(crate::model::HistoryEntry {
            created: now_secs(),
            created_by: format!("{inst} {args}").trim_end().to_string(),
            empty_layer: !is_fs_inst(inst),
        });

        // ----- build layer cache: try to reuse this step's recorded layer -----
        // `current_cid` is set when we are going to EXECUTE the step (a miss) and must store the result.
        let mut current_cid: Option<String> = None;
        if use_cache && from_done && inst != "FROM" {
            // descriptor = normalized instruction; COPY/ADD fold in a content digest of each source so a
            // changed build context invalidates; ARG folds in its *resolved* value so --build-arg changes
            // invalidate the rest of the build even when the arg is unreferenced.
            let desc = cache_desc(inst, &args, &stage_names, &stages, &ctx, &nonce, &buildargs);
            let cid = cache_id(&parent_id, &desc);
            if inst == "ARG" {
                // ARG is transparent to the fs/config cache: it advances the chain but always runs (so
                // args_map stays live for downstream substitution) and stores no layer of its own.
                parent_id = cid;
            } else {
                let hit = if cache_ok { bc.load_layer(&cid) } else { None };
                if let Some(meta) = hit {
                    // HIT — replay the recorded config now; defer the rootfs restore (a run of consecutive
                    // hits costs zero copies). The rootfs is materialized on the first miss / stage finalize
                    // from `pending_fs` (the latest hit fs layer, whose snapshot is cumulative).
                    log.push(
                        serde_json::to_string(&crate::api::BuildStream {
                            stream: " ---> Using cache\n".into(),
                        })
                        .unwrap(),
                    );
                    let arr = |k: &str| {
                        meta.get(k)
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    };
                    cmd = arr("cmd");
                    entrypoint = arr("entrypoint");
                    env = arr("env");
                    workdir = meta
                        .get("workdir")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    labels = meta
                        .get("labels")
                        .and_then(|v| v.as_object())
                        .map(|o| {
                            o.iter()
                                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                                .collect()
                        })
                        .unwrap_or_default();
                    if is_fs_inst(inst) {
                        pending_fs = Some(cid.clone());
                    }
                    parent_id = cid;
                    i += 1;
                    continue; // skip executing the instruction
                }
                // MISS — invalidate the cache for the rest of the stage and restore the real rootfs (if a
                // prior hit deferred it) before executing this step.
                cache_ok = false;
                if let Some(fsid) = pending_fs.take() {
                    if !bc.materialize(&fsid, &rootfs) {
                        return fail(
                            log,
                            "build cache: failed to restore a cached layer".into(),
                            &ctx,
                        );
                    }
                }
                current_cid = Some(cid);
            }
        }

        match inst {
            "FROM" => {
                // --target: the target stage is fully built; don't start any later stage.
                if target_built {
                    break;
                }
                // finalize the previous stage's rootfs from any deferred cache layer before starting a new
                // one, so a later COPY --from=<that stage> sees its complete contents.
                if use_cache {
                    if let Some(fsid) = pending_fs.take() {
                        if !bc.materialize(&fsid, &rootfs) {
                            return fail(
                                log,
                                "build cache: failed to restore a stage layer".into(),
                                &ctx,
                            );
                        }
                    }
                }
                let base = args.split_whitespace().next().unwrap_or("").to_string();
                // Base config carried onto the new stage. `labels`/`user`/`onbuild` are inherited too
                // (findings 5/13/10); labels feed into the LABEL merge and the cache seed (finding 4).
                let pick = |im: &Image| BaseCfg {
                    rootfs: im.rootfs.clone(),
                    arch: im.arch,
                    cmd: im.cmd.clone(),
                    entrypoint: im.entrypoint.clone(),
                    env: im.env.clone(),
                    workdir: im.workdir.clone(),
                    labels: im.labels.clone(),
                    user: im.user.clone(),
                    onbuild: im.onbuild.clone(),
                };
                // Resolve a LOCAL base by full repository AND tag (finding 14): two tags of one repo must
                // resolve to their respective rootfs, not collapse to a bare-name match.
                let mut found = {
                    let g = a.inner.lock().await;
                    find_image(&g.images, &base).map(&pick)
                };
                if found.is_none() {
                    // not local -> auto-pull the base like real docker build (reuses the registry pull)
                    log.push(
                        serde_json::to_string(&crate::api::BuildStream {
                            stream: format!("Unable to find image '{base}' locally; pulling\n"),
                        })
                        .unwrap(),
                    );
                    let (n, t) = match base.rsplit_once(':') {
                        Some((n, t)) if !t.contains('/') => (n.to_string(), t.to_string()),
                        _ => (base.clone(), "latest".to_string()),
                    };
                    let (dir, archs) = (a.images_dir.clone(), platform_archs(None));
                    match tokio::task::spawn_blocking(move || {
                        pull_image(&dir, &n, &t, Credentials::default(), &archs, &mut |_| {})
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("pull task crashed: {e}")))
                    {
                        Ok(img) => {
                            found = Some(pick(&img));
                            a.inner.lock().await.images.push(img);
                        }
                        Err(e) => {
                            return fail(
                                log,
                                format!("pull of base image '{base}' failed: {e}"),
                                &ctx,
                            );
                        }
                    }
                }
                let Some(b) = found else {
                    return fail(log, format!("base image '{base}' unavailable"), &ctx);
                };
                arch = b.arch;
                cmd = b.cmd;
                entrypoint = b.entrypoint;
                env = b.env;
                workdir = b.workdir;
                user = b.user; // inherit base config (findings 5/13)
                               // Inherit base image labels; a later child `LABEL` overrides matching keys, keeps the rest
                               // (finding 5). (Previously cleared — base labels were dropped.)
                labels = b.labels.clone();
                // A fresh stage starts with the default shell; a base image's ONBUILD triggers are replayed
                // below (finding 10).
                shell = vec!["/bin/sh".to_string(), "-c".to_string()];
                // Reset stage-scoped ARGs so a prior stage's (or pre-FROM global) ARGs don't leak into the
                // new stage unless re-declared (finding 8).
                args_map.clear();
                // start a new build stage (its own rootfs); `FROM <base> AS <name>` names it.
                let sidx = stages.len();
                rootfs = img_dir.join(format!("_s{sidx}")).join("rootfs");
                stages.push(rootfs.clone());
                stage_names.insert(sidx.to_string(), sidx);
                let words: Vec<&str> = args.split_whitespace().collect();
                if let Some(nm) = words
                    .iter()
                    .position(|w| w.eq_ignore_ascii_case("AS"))
                    .and_then(|i| words.get(i + 1))
                {
                    stage_names.insert(nm.to_string(), sidx);
                    // mark this stage so the next FROM stops the build (--target reached).
                    if !target.is_empty() && *nm == target.as_str() {
                        target_built = true;
                    }
                }
                std::fs::create_dir_all(rootfs.parent().unwrap_or(&img_dir)).ok();
                if !matches!(std::process::Command::new("cp").arg("-a").arg(&b.rootfs).arg(&rootfs).status(), Ok(s) if s.success())
                {
                    return fail(log, "failed to copy base image rootfs".into(), &ctx);
                }
                from_done = true;
                // (re)seed the per-stage cache chain from the base image's *content* digest AND its config
                // (env/cmd/labels/entrypoint/workdir/user), so a base whose CONFIG changed (same rootfs)
                // still invalidates downstream cached config steps (finding 4). cache_ok re-enables hits.
                if use_cache {
                    let mut lbl: Vec<(&String, &String)> = b.labels.iter().collect();
                    lbl.sort();
                    let labels_seed = lbl
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let seed = format!(
                        "FROM {base}\n{}\ncfg:cmd={}\nep={}\nenv={}\nwd={}\nuser={}\nlabels:\n{}",
                        rootfs_digest(std::path::Path::new(&b.rootfs)),
                        cmd.join("\u{1}"),
                        entrypoint.join("\u{1}"),
                        env.join("\u{1}"),
                        workdir,
                        user,
                        labels_seed,
                    );
                    parent_id = cache_id("", &seed);
                    cache_ok = true;
                    pending_fs = None;
                }
                // Replay the base image's ONBUILD triggers immediately after FROM (finding 10): splice the
                // parsed trigger instructions into the work list right after this FROM step.
                if !b.onbuild.is_empty() {
                    let triggers: Vec<(String, String)> =
                        b.onbuild.iter().flat_map(|t| parse_dockerfile(t)).collect();
                    for (off, st) in triggers.into_iter().enumerate() {
                        steps.insert(i + 1 + off, st);
                    }
                }
            }
            "ARG" => {
                // `ARG NAME` or `ARG NAME=default`; --build-arg overrides the default. Allowed before FROM.
                // Pre-FROM ARGs are GLOBAL (usable only by FROM); post-FROM ARGs are stage-scoped. A bare
                // post-FROM `ARG NAME` re-declaration picks up the pre-FROM global default (finding 8).
                let spec = args.split_whitespace().next().unwrap_or("");
                let (key, default) = match spec.split_once('=') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (spec.to_string(), None),
                };
                if !key.is_empty() {
                    let val = buildargs.get(&key).cloned().or(default).or_else(|| {
                        if from_done {
                            global_args.get(&key).cloned()
                        } else {
                            None
                        }
                    });
                    if let Some(val) = val {
                        if from_done {
                            args_map.insert(key, val);
                        } else {
                            global_args.insert(key, val);
                        }
                    }
                }
            }
            _ if !from_done => {
                return fail(log, "no FROM before the first instruction".into(), &ctx);
            }
            "RUN" => {
                // Shell-form RUN uses the current SHELL (finding 9); exec-form runs argv verbatim.
                if let Err(e) =
                    run_step(arch, &workdir, &rootfs, &env, &args, &shell, &mut log).await
                {
                    return fail(log, e, &ctx);
                }
            }
            "COPY" | "ADD" => {
                // `--from=<ref>` first resolves a build STAGE; if it is not a stage, fall back to a LOCAL
                // image and copy from its rootfs (finding 2). Feed copy_step an augmented stage map so the
                // external image looks like a stage source.
                let mut eff_names = stage_names.clone();
                let mut eff_stages = stages.clone();
                if let Some(fromv) = args
                    .split_whitespace()
                    .find_map(|p| p.strip_prefix("--from="))
                {
                    if !eff_names.contains_key(fromv) {
                        let ext = {
                            let g = a.inner.lock().await;
                            find_image(&g.images, fromv).map(|im| im.rootfs.clone())
                        };
                        if let Some(rootfs) = ext {
                            let idx = eff_stages.len();
                            eff_stages.push(std::path::PathBuf::from(rootfs));
                            eff_names.insert(fromv.to_string(), idx);
                        }
                    }
                }
                if let Err(e) = copy_step(
                    inst,
                    &args,
                    &rootfs,
                    &workdir,
                    &eff_names,
                    &eff_stages,
                    &ctx,
                ) {
                    return fail(log, e, &ctx);
                }
            }
            "ENV" => {
                // `ENV K V` (legacy, value = rest of line) or one/more `K=V` pairs (quotes preserve
                // spaces) — same grammar as LABEL. An override REPLACES the existing entry IN PLACE
                // (preserving order); only genuinely new keys are appended (finding 16).
                for (k, v) in parse_env(&args) {
                    let entry = format!("{k}={v}");
                    match env
                        .iter_mut()
                        .find(|e| e.split_once('=').map(|(ek, _)| ek) == Some(k.as_str()))
                    {
                        Some(slot) => *slot = entry,
                        None => env.push(entry),
                    }
                }
            }
            "WORKDIR" => {
                let joined = if args.starts_with('/') {
                    args.clone()
                } else {
                    format!("{}/{}", workdir.trim_end_matches('/'), args)
                };
                // Normalize `..`/`.` so Config.WorkingDir matches the directory actually created
                // (`WORKDIR ../c` from `/a/b` -> `/a/c`) (finding 15).
                workdir = normalize_abs_path(&joined);
                let wh = archive_host_path(&rootfs.to_string_lossy(), &[], "", &workdir);
                std::fs::create_dir_all(&wh).ok();
            }
            // Shell-form CMD/ENTRYPOINT wrap the current SHELL (finding 9).
            "CMD" => cmd = exec_or_shell(&args, &shell),
            "ENTRYPOINT" => entrypoint = exec_or_shell(&args, &shell),
            "LABEL" => {
                for (k, v) in parse_labels(&args) {
                    labels.insert(k, v);
                }
            }
            // Persist USER into the image config so inspect/run see it.
            "USER" => user = args.trim().to_string(),
            "EXPOSE" => {
                // `EXPOSE 8080 443/udp` -> port keys, defaulting the protocol to tcp.
                for tok in args.split_whitespace() {
                    let key = if tok.contains('/') {
                        tok.to_string()
                    } else {
                        format!("{tok}/tcp")
                    };
                    if !exposed_ports.contains(&key) {
                        exposed_ports.push(key);
                    }
                }
            }
            "VOLUME" => {
                // `VOLUME ["/a","/b"]` (JSON) or `VOLUME /a /b` (shell form).
                let dirs: Vec<String> = serde_json::from_str::<Vec<String>>(args.trim())
                    .unwrap_or_else(|_| args.split_whitespace().map(str::to_string).collect());
                for d in dirs {
                    if !d.is_empty() && !img_volumes.contains(&d) {
                        img_volumes.push(d);
                    }
                }
            }
            "HEALTHCHECK" => {
                // `HEALTHCHECK NONE` disables; `HEALTHCHECK [opts] CMD <cmd>` sets a shell probe. The CMD
                // tail becomes a CMD-SHELL test (docker's default for the shell form); options default.
                let a = args.trim();
                if a.eq_ignore_ascii_case("NONE") {
                    healthcheck = None;
                } else if let Some(pos) = a.to_ascii_uppercase().find("CMD") {
                    let test = a[pos + 3..].trim().to_string();
                    healthcheck = Some(serde_json::json!({"Test": ["CMD-SHELL", test]}));
                }
            }
            // `SHELL ["/bin/bash","-c"]` overrides the shell for later shell-form RUN/CMD/ENTRYPOINT.
            // Ignore a malformed (non-array) SHELL.
            "SHELL" => {
                let sh = parse_exec_form(&args);
                if args.trim().starts_with('[') && !sh.is_empty() {
                    shell = sh;
                }
            }
            // Store an ONBUILD trigger on the image being built; a child `FROM` this image replays it.
            "ONBUILD" => {
                let trig = args.trim();
                if !trig.is_empty() {
                    onbuild.push(trig.to_string());
                }
            }
            _ => {} // MAINTAINER/etc — no rootfs or config effect in this builder
        }

        // Step executed (a cache miss): record its result as a layer for future rebuilds and advance the
        // chain. fs-mutating steps snapshot the live rootfs; config-only steps store just their metadata.
        if let Some(cid) = current_cid.take() {
            bc.store_layer(
                &cid,
                &parent_id,
                inst,
                &args,
                &rootfs,
                &cmd,
                &entrypoint,
                &workdir,
                &env,
                &labels,
            );
            parent_id = cid;
        }
        i += 1;
    }
    if !from_done {
        return fail(log, "Dockerfile had no FROM".into(), &ctx);
    }
    cleanup(&ctx);
    // finalize the final stage's rootfs from any deferred cache layer (a build that ended on a run of
    // cache hits never materialized it).
    if use_cache {
        if let Some(fsid) = pending_fs.take() {
            if !bc.materialize(&fsid, &rootfs) {
                let _ = std::fs::remove_dir_all(&img_dir);
                return build_err(log, "build cache: failed to restore the final layer".into());
            }
        }
    }

    // the LAST stage is the final image: move its rootfs to <img>/rootfs, drop the intermediate stages.
    let final_rootfs = stages.last().cloned().unwrap_or_else(|| rootfs.clone());
    let image_rootfs = img_dir.join("rootfs");
    if final_rootfs != image_rootfs {
        let _ = std::fs::remove_dir_all(&image_rootfs);
        if std::fs::rename(&final_rootfs, &image_rootfs).is_err() {
            let _ = std::process::Command::new("cp")
                .arg("-a")
                .arg(&final_rootfs)
                .arg(&image_rootfs)
                .status();
        }
    }
    for s in &stages {
        if let Some(p) = s.parent() {
            if p != img_dir {
                let _ = std::fs::remove_dir_all(p);
            }
        }
    }
    let rootfs = image_rootfs; // the registered image's rootfs

    // `docker build --label K=V` -> the `labels` query param (a JSON object), merged on top of any
    // `LABEL` instructions so a CLI flag wins over the Dockerfile, matching docker.
    if let Some(extra) = q
        .labels
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<HashMap<String, String>>(s).ok())
    {
        for (k, v) in extra {
            labels.insert(k, v);
        }
    }

    // register the built image (persist the full config so it survives a daemon restart)
    if cmd.is_empty() && entrypoint.is_empty() {
        cmd = default_shell(&rootfs);
    }
    // Persist the full config so it survives a daemon restart — including USER, the per-instruction
    // history, and any ONBUILD triggers (findings 13/3/10).
    let history_json: Vec<Value> = history
        .iter()
        .map(|h| {
            json!({"created": h.created, "created_by": h.created_by, "empty_layer": h.empty_layer})
        })
        .collect();
    std::fs::write(
        img_dir.join("hl-image.json"),
        json!({"name": name, "cmd": cmd, "entrypoint": entrypoint, "env": env, "workdir": workdir,
               "labels": labels, "arch": arch.arch(), "os": arch.os(), "user": user,
               "exposed_ports": exposed_ports, "img_volumes": img_volumes, "healthcheck": healthcheck,
               "onbuild": onbuild, "history": history_json})
        .to_string(),
    )
    .ok();

    // a real content digest for the image ID: sha256 over the image's defining content — the Dockerfile,
    // a deterministic content hash of the assembled rootfs, and the final config (incl. sorted labels, so
    // the digest is reproducible: HashMap iteration order must not leak in). Same inputs -> same ID.
    let id = {
        let mut lbl: Vec<(&String, &String)> = labels.iter().collect();
        lbl.sort();
        let labels_str = lbl
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("\n");
        let manifest = format!(
            "dockerfile:\n{dockerfile}\nrootfs:{}\ncmd:{}\nentrypoint:{}\nenv:{}\nworkdir:{workdir}\nlabels:\n{labels_str}",
            rootfs_digest(&rootfs), cmd.join("\u{1}"), entrypoint.join("\u{1}"), env.join("\u{1}"));
        let h = sha256_hex(manifest.as_bytes());
        if h.len() == 64 {
            h
        } else {
            fake_id(&manifest)
        } // fallback keeps a deterministic id if sha256sum is missing
    };
    {
        let mut g = a.inner.lock().await;
        g.images.retain(|im| repo_tag(&im.name) != repo_tag(&name));
        g.images.push(Image {
            name: name.clone(),
            rootfs: rootfs.to_string_lossy().into_owned(),
            arch,
            cmd,
            entrypoint,
            env,
            workdir,
            user,
            exposed_ports,
            img_volumes,
            healthcheck: healthcheck
                .and_then(|v| serde_json::from_value::<crate::model::HealthConfig>(v).ok()),
            labels,
            created: now_secs(),
            history,
            onbuild,
            ..Default::default()
        });
    }
    log.push(
        serde_json::to_string(&crate::api::BuildStream {
            stream: format!("Successfully built {}\n", &id[..12.min(id.len())]),
        })
        .unwrap(),
    );
    log.push(
        serde_json::to_string(&crate::api::BuildStream {
            stream: format!("Successfully tagged {raw_tag}\n"),
        })
        .unwrap(),
    );
    log.push(
        serde_json::to_string(&crate::api::BuildAux {
            aux: crate::api::BuildAuxId {
                id: format!("sha256:{id}"),
            },
        })
        .unwrap(),
    );
    build_stream(log)
}

#[cfg(test)]
mod handler_tests {
    use super::{read_context_dockerfile, unpack_build_context};
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let d = std::env::temp_dir().join(format!(
            "hl-handler-test-{}-{}-{}",
            label,
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // The staged `.context.tar` must NOT be left inside the context tree (else `COPY .` copies it into the
    // image). After unpack, `ctx` holds exactly the submitted members and no `.context.tar`.
    #[test]
    fn unpack_build_context_excludes_context_tar() {
        let images_dir = scratch("ctxtar");
        // Build a context tar containing Dockerfile + hello.txt.
        let src = images_dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("Dockerfile"), b"FROM scratch\n").unwrap();
        std::fs::write(src.join("hello.txt"), b"hi\n").unwrap();
        let tar = images_dir.join("in.tar");
        assert!(std::process::Command::new("tar")
            .arg("cf")
            .arg(&tar)
            .arg("-C")
            .arg(&src)
            .arg("Dockerfile")
            .arg("hello.txt")
            .status()
            .unwrap()
            .success());
        let body = std::fs::read(&tar).unwrap();

        let ctx = images_dir.join(".build-ctx-x");
        std::fs::create_dir_all(&ctx).unwrap();
        unpack_build_context(&body, &ctx, images_dir.to_str().unwrap()).expect("unpack");

        assert!(ctx.join("Dockerfile").is_file());
        assert!(ctx.join("hello.txt").is_file());
        assert!(
            !ctx.join(".context.tar").exists(),
            ".context.tar must not be inside the context"
        );
        let entries: Vec<_> = std::fs::read_dir(&ctx)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "only the submitted members remain: {entries:?}"
        );
        let _ = std::fs::remove_dir_all(&images_dir);
    }

    // A real in-context Dockerfile reads; a symlink escaping the context is refused.
    #[test]
    fn read_context_dockerfile_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let base = scratch("dfsym");
        let ctx = base.join("ctx");
        std::fs::create_dir_all(&ctx).unwrap();

        // Regular Dockerfile inside the context: read succeeds.
        std::fs::write(ctx.join("Dockerfile"), b"FROM inside\n").unwrap();
        assert_eq!(
            read_context_dockerfile(&ctx, "Dockerfile").as_deref(),
            Some("FROM inside\n")
        );

        // A Dockerfile symlink pointing outside the context is refused.
        let outside = base.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("Dockerfile.external"), b"FROM external\n").unwrap();
        symlink(
            outside.join("Dockerfile.external"),
            ctx.join("Dockerfile.link"),
        )
        .unwrap();
        assert_eq!(
            read_context_dockerfile(&ctx, "Dockerfile.link"),
            None,
            "symlink escaping the context must be refused"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}
