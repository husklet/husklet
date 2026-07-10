//! `POST /build` (`docker build`) integration tests. A build whose steps are FROM + config-only
//! (ENV/LABEL/CMD/USER/WORKDIR/SHELL/ONBUILD) plus COPY/ADD never reaches the JIT (only `RUN` does), so
//! these drive the real `images_build` handler end-to-end: seed a base image on disk, submit a context
//! tar, and assert the resulting in-memory image config / `docker history`. `nocache=1` keeps each build
//! independent of the process-global build-layer cache.
use super::*;
use crate::model::Image;
use axum::extract::{Query, State};
use std::path::{Path, PathBuf};

/// Create a base image rootfs dir on disk (with a marker file) and register it in the store, letting a
/// `mutate` hook set config fields (labels/env/onbuild/…). Returns the rootfs path.
async fn seed_base(app: &App, name: &str, mutate: impl FnOnce(&mut Image)) -> PathBuf {
    let rootfs = PathBuf::from(&app.images_dir).join(format!("base-{}", safe(name)));
    std::fs::create_dir_all(&rootfs).unwrap();
    std::fs::write(rootfs.join("base-marker"), name.as_bytes()).unwrap();
    let mut img = Image {
        name: name.to_string(),
        rootfs: rootfs.to_string_lossy().into_owned(),
        ..Default::default()
    };
    mutate(&mut img);
    app.inner.lock().await.images.push(img);
    rootfs
}

fn safe(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// Build a context tar (files given as `(relative_path, contents)`), including the Dockerfile.
fn context_tar(files: &[(&str, &str)]) -> Vec<u8> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("dd-buildctx-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).unwrap();
    let mut names: Vec<String> = Vec::new();
    for (rel, contents) in files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, contents.as_bytes()).unwrap();
        names.push((*rel).to_string());
    }
    let tarp = dir.join("_ctx.tar");
    let mut cmd = std::process::Command::new("tar");
    cmd.arg("cf").arg(&tarp).arg("-C").arg(&dir);
    for n in &names {
        cmd.arg(n);
    }
    assert!(cmd.status().unwrap().success(), "build the context tar");
    let bytes = std::fs::read(&tarp).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

fn build_q(tag: &str, target: Option<&str>) -> crate::build::BuildQ {
    let mut v = serde_json::json!({"t": tag, "nocache": "1"});
    if let Some(t) = target {
        v["target"] = serde_json::Value::String(t.to_string());
    }
    serde_json::from_value(v).unwrap()
}

/// Drive `images_build`; returns (whole NDJSON body, whether an error line was emitted).
async fn run_build(
    app: &App,
    tag: &str,
    dockerfile: &str,
    extra_files: &[(&str, &str)],
    target: Option<&str>,
) -> (String, bool) {
    let mut files = vec![("Dockerfile", dockerfile)];
    files.extend_from_slice(extra_files);
    let body = axum::body::Bytes::from(context_tar(&files));
    let resp =
        crate::build::images_build(State(app.clone()), Query(build_q(tag, target)), body).await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8_lossy(&bytes).into_owned();
    let errored = body.lines().any(|l| {
        serde_json::from_str::<serde_json::Value>(l)
            .ok()
            .and_then(|v| v.get("error").map(|_| ()))
            .is_some()
    });
    (body, errored)
}

/// The built image with the given repo:tag, cloned out of the store.
async fn built(app: &App, tag: &str) -> Option<Image> {
    app.inner.lock().await.images.iter().find(|i| i.name == tag).cloned()
}

// ---- Finding 3: per-instruction docker history ------------------------------------------------
#[tokio::test]
async fn build_records_per_instruction_history() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest\nENV FOO=bar\nLABEL a=b\nCMD [\"/bin/true\"]\n";
    let (_body, err) = run_build(&app, "histimg:latest", df, &[], None).await;
    assert!(!err, "build should succeed");

    let resp = crate::images::image_history(
        State(app.clone()),
        axum::extract::Path("histimg:latest".into()),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let rows = rows.as_array().expect("history is an array");
    // One row per instruction (FROM/ENV/LABEL/CMD), newest-first.
    let created_by: Vec<String> = rows
        .iter()
        .map(|r| r["CreatedBy"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        created_by.iter().any(|c| c == "FROM base:latest")
            && created_by.iter().any(|c| c == "ENV FOO=bar")
            && created_by.iter().any(|c| c == "LABEL a=b")
            && created_by.iter().any(|c| c == "CMD [\"/bin/true\"]"),
        "history has a row per instruction: {created_by:?}"
    );
    // Newest-first: the last Dockerfile instruction (CMD) is the top row.
    assert_eq!(created_by[0], "CMD [\"/bin/true\"]", "newest instruction first");
}

// A pulled/imported image with no recorded history still reports a single synthetic row.
#[tokio::test]
async fn history_without_recorded_history_is_single_row() {
    let app = test_app();
    seed_base(&app, "imported:latest", |_| {}).await;
    let resp = crate::images::image_history(
        State(app.clone()),
        axum::extract::Path("imported:latest".into()),
    )
    .await;
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 1, "single synthetic row");
    assert_eq!(rows[0]["CreatedBy"], "dd import");
}

// ---- Finding 5: LABEL merges base labels ------------------------------------------------------
#[tokio::test]
async fn build_inherits_base_labels_and_child_overrides() {
    let app = test_app();
    seed_base(&app, "base:latest", |i| {
        i.labels.insert("org.example.base".into(), "kept".into());
        i.labels.insert("shared".into(), "from-base".into());
    })
    .await;
    let df = "FROM base:latest\nLABEL shared=from-child child=yes\nCMD [\"/bin/true\"]\n";
    let (_b, err) = run_build(&app, "labimg:latest", df, &[], None).await;
    assert!(!err);
    let img = built(&app, "labimg:latest").await.expect("built image");
    assert_eq!(img.labels.get("org.example.base").map(String::as_str), Some("kept"), "base label survives");
    assert_eq!(img.labels.get("shared").map(String::as_str), Some("from-child"), "child overrides matching key");
    assert_eq!(img.labels.get("child").map(String::as_str), Some("yes"), "child label added");
}

// ---- Finding 7: ENV interpolation uses prior ENV ----------------------------------------------
#[tokio::test]
async fn build_env_interpolation_uses_prior_env() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest\nENV A=one\nENV B=${A}\nCMD [\"/bin/true\"]\n";
    let (_b, err) = run_build(&app, "envimg:latest", df, &[], None).await;
    assert!(!err);
    let img = built(&app, "envimg:latest").await.unwrap();
    assert!(img.env.contains(&"B=one".to_string()), "ENV B=${{A}} -> B=one; env={:?}", img.env);
}

// ---- Finding 8: pre-FROM ARG does not leak into stage scope -----------------------------------
#[tokio::test]
async fn build_pre_from_arg_does_not_leak_into_stage() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    // `${V}` after FROM (V declared only before FROM) must NOT expand to `x`: it becomes empty, so the
    // copy looks for `payload-.txt` (absent) and the build FAILS — proving V is out of stage scope.
    let df = "ARG V=x\nFROM base:latest\nCOPY payload-${V}.txt /out.txt\n";
    let (_b, err) = run_build(&app, "argleak:latest", df, &[("payload-x.txt", "hi")], None).await;
    assert!(err, "pre-FROM ARG must not expand after FROM (build should fail)");

    // Re-declaring `ARG V` after FROM brings the global default back into scope -> ${V}=x -> copies.
    let df2 = "ARG V=x\nFROM base:latest\nARG V\nCOPY payload-${V}.txt /out.txt\n";
    let (_b2, err2) =
        run_build(&app, "argredecl:latest", df2, &[("payload-x.txt", "hi")], None).await;
    assert!(!err2, "re-declared ARG V after FROM restores the value: {_b2}");
}

// ---- Finding 9: SHELL affects shell-form CMD --------------------------------------------------
#[tokio::test]
async fn build_shell_changes_shell_form_cmd() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest\nSHELL [\"/bin/bash\", \"-c\"]\nCMD echo hi\n";
    let (_b, err) = run_build(&app, "shellimg:latest", df, &[], None).await;
    assert!(!err);
    let img = built(&app, "shellimg:latest").await.unwrap();
    assert_eq!(img.cmd, vec!["/bin/bash", "-c", "echo hi"], "shell-form CMD uses the SHELL");
}

// ---- Finding 10: ONBUILD triggers replay on child FROM ----------------------------------------
#[tokio::test]
async fn build_replays_base_onbuild_triggers() {
    let app = test_app();
    seed_base(&app, "onbuildbase:latest", |i| {
        i.onbuild.push("ENV TRIGGERED=yes".into());
    })
    .await;
    let df = "FROM onbuildbase:latest\nCMD [\"/bin/true\"]\n";
    let (_b, err) = run_build(&app, "childimg:latest", df, &[], None).await;
    assert!(!err);
    let img = built(&app, "childimg:latest").await.unwrap();
    assert!(img.env.contains(&"TRIGGERED=yes".to_string()), "ONBUILD ENV replayed; env={:?}", img.env);
}

// ---- Finding 11: unknown --target is an error -------------------------------------------------
#[tokio::test]
async fn build_unknown_target_errors() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest AS first\nCMD [\"/bin/true\"]\n";
    let (body, err) = run_build(&app, "tgt:latest", df, &[], Some("nope")).await;
    assert!(err, "unknown --target must fail the build: {body}");
    assert!(built(&app, "tgt:latest").await.is_none(), "no image registered for an unknown target");
}

// ---- Finding 12: a failed build leaves no partial image dir -----------------------------------
#[tokio::test]
async fn build_failure_cleans_partial_image_dir() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest\nCOPY missing.txt /dst\n";
    let (_b, err) = run_build(&app, "partial:latest", df, &[], None).await;
    assert!(err, "COPY of a missing file must fail");
    let img_dir = PathBuf::from(&app.images_dir).join(crate::build::safe_dir_name("partial:latest"));
    assert!(!img_dir.exists(), "failed build must not leave an images/<tag> dir: {img_dir:?}");
}

// ---- Finding 13: USER persisted into the image config -----------------------------------------
#[tokio::test]
async fn build_persists_user() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest\nUSER 1000\nCMD [\"/bin/true\"]\n";
    let (_b, err) = run_build(&app, "userimg:latest", df, &[], None).await;
    assert!(!err);
    let img = built(&app, "userimg:latest").await.unwrap();
    assert_eq!(img.user, "1000", "USER persisted into Config.User");
}

// ---- Finding 14: FROM local lookup honors the tag ---------------------------------------------
#[tokio::test]
async fn build_from_local_honors_tag() {
    let app = test_app();
    let r1 = seed_base(&app, "app:v1", |_| {}).await;
    let r2 = seed_base(&app, "app:v2", |_| {}).await;
    // distinct marker contents in each tag's rootfs.
    std::fs::write(r1.join("which"), b"v1").unwrap();
    std::fs::write(r2.join("which"), b"v2").unwrap();

    let (_b, err) = run_build(&app, "fromv2:latest", "FROM app:v2\nCMD [\"/bin/true\"]\n", &[], None).await;
    assert!(!err);
    let img = built(&app, "fromv2:latest").await.unwrap();
    let which = std::fs::read_to_string(PathBuf::from(&img.rootfs).join("which")).unwrap_or_default();
    assert_eq!(which, "v2", "FROM app:v2 resolves to the v2 rootfs, not v1");
}

// ---- Finding 15: relative WORKDIR `..` normalized ---------------------------------------------
#[tokio::test]
async fn build_workdir_dotdot_normalized() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest\nWORKDIR /a/b\nWORKDIR ../c\nCMD [\"/bin/true\"]\n";
    let (_b, err) = run_build(&app, "wdimg:latest", df, &[], None).await;
    assert!(!err);
    let img = built(&app, "wdimg:latest").await.unwrap();
    assert_eq!(img.workdir, "/a/c", "WORKDIR ../c from /a/b normalizes to /a/c");
}

// ---- Finding 16: ENV override replaces in place (order preserved) ------------------------------
#[tokio::test]
async fn build_env_override_preserves_order() {
    let app = test_app();
    seed_base(&app, "base:latest", |i| {
        i.env = vec!["A=1".into(), "B=2".into(), "Z=26".into()];
    })
    .await;
    let df = "FROM base:latest\nENV A=new C=child\nCMD [\"/bin/true\"]\n";
    let (_b, err) = run_build(&app, "ordimg:latest", df, &[], None).await;
    assert!(!err);
    let img = built(&app, "ordimg:latest").await.unwrap();
    assert_eq!(
        img.env,
        vec!["A=new", "B=2", "Z=26", "C=child"],
        "overridden A stays in place; new C appended"
    );
}

// ---- Finding 2: COPY --from=<local image> copies from that image's rootfs ----------------------
#[tokio::test]
async fn build_copy_from_external_local_image() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    // A separate local image whose rootfs holds the file to copy.
    let ext = seed_base(&app, "assets:latest", |_| {}).await;
    std::fs::write(ext.join("payload.bin"), b"external-content").unwrap();

    let df = "FROM base:latest\nCOPY --from=assets:latest /payload.bin /got.bin\nCMD [\"/bin/true\"]\n";
    let (body, err) = run_build(&app, "extcopy:latest", df, &[], None).await;
    assert!(!err, "COPY --from a local image should succeed: {body}");
    let img = built(&app, "extcopy:latest").await.unwrap();
    let got = std::fs::read_to_string(PathBuf::from(&img.rootfs).join("got.bin")).unwrap_or_default();
    assert_eq!(got, "external-content", "file copied from the external image's rootfs");
}

// ---- Finding 6: .dockerignore excludes matching paths from COPY . -----------------------------
#[tokio::test]
async fn build_dockerignore_excludes_secret() {
    let app = test_app();
    seed_base(&app, "base:latest", |_| {}).await;
    let df = "FROM base:latest\nCOPY . /out/\nCMD [\"/bin/true\"]\n";
    let files = [("secret.txt", "TOPSECRET"), ("keep.txt", "public"), (".dockerignore", "secret.txt\n")];
    let (body, err) = run_build(&app, "diimg:latest", df, &files, None).await;
    assert!(!err, "build should succeed: {body}");
    let img = built(&app, "diimg:latest").await.unwrap();
    let out = PathBuf::from(&img.rootfs).join("out");
    assert!(out.join("keep.txt").exists(), "non-ignored file is copied");
    assert!(!out.join("secret.txt").exists(), ".dockerignore'd secret.txt must NOT be copied");
}

// ---- Finding 4: base config change invalidates downstream cached config -----------------------
// Uses the REAL (process-global) build cache: build with base ENV FOO=one, then rebuild after changing
// the base's ENV to FOO=two (rootfs unchanged). The child's inherited FOO must reflect the NEW base.
#[tokio::test]
async fn build_base_config_change_invalidates_cache() {
    let app = test_app();
    // First build: base carries FOO=one.
    seed_base(&app, "cfgbase:latest", |i| {
        i.env = vec!["FOO=one".into()];
    })
    .await;
    // A cache-ENABLED build (no nocache) so a stale cache COULD leak the old value.
    let df = "FROM cfgbase:latest\nLABEL marker=x\nCMD [\"/bin/true\"]\n";
    let body = context_tar(&[("Dockerfile", df)]);
    let q: crate::build::BuildQ =
        serde_json::from_value(serde_json::json!({"t": "cfgchild:latest"})).unwrap();
    let _ = crate::build::images_build(State(app.clone()), Query(q), axum::body::Bytes::from(body)).await;
    let first = built(&app, "cfgchild:latest").await.unwrap();
    assert!(first.env.contains(&"FOO=one".to_string()), "first build inherits FOO=one: {:?}", first.env);

    // Change the base config (same rootfs) to FOO=two and rebuild the identical Dockerfile.
    {
        let mut g = app.inner.lock().await;
        for im in g.images.iter_mut().filter(|im| im.name == "cfgbase:latest") {
            im.env = vec!["FOO=two".into()];
        }
    }
    let body2 = context_tar(&[("Dockerfile", df)]);
    let q2: crate::build::BuildQ =
        serde_json::from_value(serde_json::json!({"t": "cfgchild:latest"})).unwrap();
    let _ = crate::build::images_build(State(app.clone()), Query(q2), axum::body::Bytes::from(body2)).await;
    let second = built(&app, "cfgchild:latest").await.unwrap();
    assert!(
        second.env.contains(&"FOO=two".to_string()) && !second.env.contains(&"FOO=one".to_string()),
        "a base CONFIG change must invalidate the cached config (no stale FOO=one): {:?}",
        second.env
    );
}

// Ensure the marker file / path helpers compile against the module.
#[allow(dead_code)]
fn _touch(_p: &Path) {}
