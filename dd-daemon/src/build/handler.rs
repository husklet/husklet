//! The `POST /build` axum handler: request/`--build-arg`/`--target`/`--no-cache`/`--label` parsing, the
//! multi-stage step-loop driver with the content-addressed build layer cache, and final image
//! registration + progress/response streaming. Per-instruction execution (RUN/COPY/ADD) and the
//! cache-descriptor live in `steps`; shared helpers/types come from `mod.rs` via `use super::*`.
use super::*;

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
    let buildargs: HashMap<String, String> = q
        .buildargs
        .as_deref()
        .filter(|s| !s.is_empty())
        .and_then(|s| serde_json::from_str::<HashMap<String, Option<String>>>(s).ok())
        .map(|m| {
            m.into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v)))
                .collect()
        })
        .unwrap_or_default();
    // --target: name of the stage to stop at (empty = build every stage, as before).
    let target = q.target.clone().unwrap_or_default();
    // --no-cache: bypass the build layer cache entirely (never read, never write) — a from-scratch build
    // identical to the pre-cache behavior. Otherwise the per-step layer cache is active (see below).
    let nocache = matches!(q.nocache.as_deref(), Some("1") | Some("true"));
    let use_cache = !nocache;

    // unpack the build context (a tar in the request body)
    let ctx = std::path::PathBuf::from(format!(
        "{}/.build-ctx-{}",
        a.images_dir,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&ctx);
    if std::fs::create_dir_all(&ctx).is_err() {
        return build_err(log, "cannot create build dir".into());
    }
    let ctar = ctx.join(".context.tar");
    let cleanup = |ctx: &std::path::Path| {
        let _ = std::fs::remove_dir_all(ctx);
    };
    if std::fs::write(&ctar, &body).is_err() {
        cleanup(&ctx);
        return build_err(log, "cannot write context".into());
    }
    if !matches!(std::process::Command::new("tar").arg("xf").arg(&ctar).arg("-C").arg(&ctx).status(), Ok(s) if s.success())
    {
        cleanup(&ctx);
        return build_err(log, "cannot unpack build context".into());
    }
    let dockerfile = match std::fs::read_to_string(ctx.join(&dfname)) {
        Ok(d) => d,
        Err(_) => {
            cleanup(&ctx);
            return build_err(log, format!("Cannot locate specified Dockerfile: {dfname}"));
        }
    };
    let steps = parse_dockerfile(&dockerfile);
    let total = steps.len();

    // the new image's rootfs dir under DD_IMAGES — derived from the user's raw `-t` so a bare tag keeps a
    // predictable dir name (`scen-built`), while a namespaced/tagged build still gets a distinct dir.
    let safe: String = raw_tag
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let img_dir = std::path::PathBuf::from(format!("{}/{}", a.images_dir, safe));
    let _ = std::fs::remove_dir_all(&img_dir);
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

    // merged ARG map: Dockerfile `ARG` defaults, overridden by --build-arg values (filled as ARG steps run).
    let mut args_map: HashMap<String, String> = HashMap::new();
    // image labels accumulated from `LABEL` instructions (per-stage; cleared at each FROM). The
    // `--label` build option is merged on top after the loop.
    let mut labels: HashMap<String, String> = HashMap::new();
    // set once the --target stage has been fully built, so the next FROM stops the build.
    let mut target_built = false;

    // --- build layer cache chain state (reset at each FROM) ---
    // The snapshot/restore + step-metadata store lives in dd-images (runtime-agnostic); root it at the
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

    for (i, (inst, args)) in steps.iter().enumerate() {
        // expand ${ARG}/$ARG using the merged map before logging or executing the step.
        let args = substitute_args(args, &args_map);
        log.push(
            json!({"stream": format!("Step {}/{} : {} {}\n", i + 1, total, inst, args)})
                .to_string(),
        );

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
                    log.push(json!({"stream": " ---> Using cache\n"}).to_string());
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
                    continue; // skip executing the instruction
                }
                // MISS — invalidate the cache for the rest of the stage and restore the real rootfs (if a
                // prior hit deferred it) before executing this step.
                cache_ok = false;
                if let Some(fsid) = pending_fs.take() {
                    if !bc.materialize(&fsid, &rootfs) {
                        cleanup(&ctx);
                        return build_err(
                            log,
                            "build cache: failed to restore a cached layer".into(),
                        );
                    }
                }
                current_cid = Some(cid);
            }
        }

        match inst.as_str() {
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
                            cleanup(&ctx);
                            return build_err(
                                log,
                                "build cache: failed to restore a stage layer".into(),
                            );
                        }
                    }
                }
                let base = args.split_whitespace().next().unwrap_or("").to_string();
                let pick = |im: &Image| {
                    (
                        im.rootfs.clone(),
                        im.arch,
                        im.cmd.clone(),
                        im.entrypoint.clone(),
                        im.env.clone(),
                        im.workdir.clone(),
                    )
                };
                let mut found = {
                    let g = a.inner.lock().await;
                    g.images
                        .iter()
                        .find(|im| ref_name(&im.name) == ref_name(&base))
                        .map(&pick)
                };
                if found.is_none() {
                    // not local -> auto-pull the base like real docker build (reuses the registry pull)
                    log.push(json!({"stream": format!("Unable to find image '{base}' locally; pulling\n")}).to_string());
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
                            cleanup(&ctx);
                            return build_err(
                                log,
                                format!("pull of base image '{base}' failed: {e}"),
                            );
                        }
                    }
                }
                let Some((base_rootfs, base_arch, base_cmd, base_ep, base_env, base_wd)) = found
                else {
                    cleanup(&ctx);
                    return build_err(log, format!("base image '{base}' unavailable"));
                };
                arch = base_arch;
                cmd = base_cmd;
                entrypoint = base_ep;
                env = base_env;
                workdir = base_wd; // inherit base config
                labels.clear(); // labels are per-stage; base-image label inheritance is not modeled
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
                if !matches!(std::process::Command::new("cp").arg("-a").arg(&base_rootfs).arg(&rootfs).status(), Ok(s) if s.success())
                {
                    cleanup(&ctx);
                    return build_err(log, "failed to copy base image rootfs".into());
                }
                from_done = true;
                // (re)seed the per-stage cache chain from the base image's *content* digest, so a changed
                // base (re-pulled/rebuilt) invalidates the whole stage. cache_ok re-enables hits.
                if use_cache {
                    let seed = format!(
                        "FROM {base}\n{}",
                        rootfs_digest(std::path::Path::new(&base_rootfs))
                    );
                    parent_id = cache_id("", &seed);
                    cache_ok = true;
                    pending_fs = None;
                }
            }
            "ARG" => {
                // `ARG NAME` or `ARG NAME=default`; --build-arg overrides the default. Allowed before FROM.
                let spec = args.split_whitespace().next().unwrap_or("");
                if let Some((k, v)) = spec.split_once('=') {
                    let val = buildargs.get(k).cloned().unwrap_or_else(|| v.to_string());
                    if !k.is_empty() {
                        args_map.insert(k.to_string(), val);
                    }
                } else if !spec.is_empty() {
                    if let Some(v) = buildargs.get(spec) {
                        args_map.insert(spec.to_string(), v.clone());
                    }
                }
            }
            _ if !from_done => {
                cleanup(&ctx);
                return build_err(log, "no FROM before the first instruction".into());
            }
            "RUN" => {
                if let Err(e) = run_step(arch, &workdir, &rootfs, &env, &args, &mut log).await {
                    cleanup(&ctx);
                    return build_err(log, e);
                }
            }
            "COPY" | "ADD" => {
                if let Err(e) = copy_step(inst, &args, &rootfs, &workdir, &stage_names, &stages, &ctx)
                {
                    cleanup(&ctx);
                    return build_err(log, e);
                }
            }
            "ENV" => {
                // `ENV K V` or `ENV K=V`; stored as "K=V"
                let kv = if let Some((k, v)) = args.split_once('=') {
                    format!(
                        "{}={}",
                        k.trim(),
                        v.split_whitespace().next().unwrap_or("").trim_matches('"')
                    )
                } else if let Some((k, v)) = args.split_once(char::is_whitespace) {
                    format!("{}={}", k.trim(), v.trim().trim_matches('"'))
                } else {
                    String::new()
                };
                if !kv.is_empty() {
                    env.retain(|e| {
                        e.split_once('=').map(|(k, _)| k) != kv.split_once('=').map(|(k, _)| k)
                    });
                    env.push(kv);
                }
            }
            "WORKDIR" => {
                workdir = if args.starts_with('/') {
                    args.clone()
                } else {
                    format!("{}/{}", workdir.trim_end_matches('/'), args)
                };
                let wh = archive_host_path(&rootfs.to_string_lossy(), &[], "", &workdir);
                std::fs::create_dir_all(&wh).ok();
            }
            "CMD" => cmd = parse_exec_form(&args),
            "ENTRYPOINT" => entrypoint = parse_exec_form(&args),
            "LABEL" => {
                for (k, v) in parse_labels(&args) {
                    labels.insert(k, v);
                }
            }
            _ => {} // EXPOSE/MAINTAINER/USER/VOLUME/HEALTHCHECK — no rootfs effect in this builder
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
    }
    if !from_done {
        cleanup(&ctx);
        return build_err(log, "Dockerfile had no FROM".into());
    }
    cleanup(&ctx);
    // finalize the final stage's rootfs from any deferred cache layer (a build that ended on a run of
    // cache hits never materialized it).
    if use_cache {
        if let Some(fsid) = pending_fs.take() {
            if !bc.materialize(&fsid, &rootfs) {
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
    std::fs::write(
        img_dir.join("dd-image.json"),
        json!({"name": name, "cmd": cmd, "entrypoint": entrypoint, "env": env, "workdir": workdir,
               "labels": labels, "arch": arch.arch(), "os": arch.os()})
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
            labels,
            created: now_secs(),
            ..Default::default()
        });
    }
    log.push(
        json!({"stream": format!("Successfully built {}\n", &id[..12.min(id.len())])}).to_string(),
    );
    log.push(json!({"stream": format!("Successfully tagged {raw_tag}\n")}).to_string());
    log.push(json!({"aux": {"ID": format!("sha256:{id}")}}).to_string());
    build_stream(log)
}
