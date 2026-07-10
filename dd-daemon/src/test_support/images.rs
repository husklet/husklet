//! Image list/tag/rmi tests, `system_df` wire shape, and image-refcount multi-step flows.
use super::*;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;

/// Push a minimal image into `Inner.images` (there is no shared seed helper for images).
async fn seed_image(app: &App, name: &str, created: i64) {
    let mut g = app.inner.lock().await;
    g.images.push(Image {
        name: name.to_string(),
        created,
        ..Default::default()
    });
}

/// Serialize `images_json`'s list and collect the flattened set of RepoTags strings.
async fn image_repotags(app: &App) -> std::collections::HashSet<String> {
    let axum::Json(list) = crate::images::images_json(State(app.clone())).await;
    let v = serde_json::to_value(&list).unwrap();
    let mut set = std::collections::HashSet::new();
    for img in v.as_array().unwrap() {
        if let Some(tags) = img["RepoTags"].as_array() {
            for t in tags {
                if let Some(s) = t.as_str() {
                    set.insert(s.to_string());
                }
            }
        }
    }
    set
}

// ---- Finding 17: save/load round-trips lifecycle metadata (stop_signal/volumes/healthcheck) ----
#[tokio::test]
async fn save_load_round_trips_lifecycle_metadata() {
    let app = test_app();
    // A real rootfs dir under the store so `docker save` can tar it (the store tars a dir literally
    // named `rootfs` from its parent).
    let rootfs = std::path::PathBuf::from(&app.images_dir).join("lifecycle").join("rootfs");
    std::fs::create_dir_all(&rootfs).unwrap();
    std::fs::write(rootfs.join("marker"), b"x").unwrap();
    {
        let mut g = app.inner.lock().await;
        g.images.push(Image {
            name: "lifecycle:latest".into(),
            rootfs: rootfs.to_string_lossy().into_owned(),
            stop_signal: "SIGQUIT".into(),
            img_volumes: vec!["/data".into(), "/cache".into()],
            healthcheck: Some(crate::model::HealthConfig {
                test: vec!["CMD".into(), "true".into()],
                interval: 5,
                retries: 3,
                ..Default::default()
            }),
            ..Default::default()
        });
    }
    // docker save -> tar bytes.
    let q: crate::images::SaveQ =
        serde_json::from_value(serde_json::json!({"names": "lifecycle:latest"})).unwrap();
    let resp = crate::images::image_save(State(app.clone()), Query(q)).await;
    assert_eq!(resp.status(), StatusCode::OK, "save succeeds");
    let tar = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();

    // Fresh daemon; docker load the archive back.
    let app2 = test_app();
    let resp = crate::images::image_load(State(app2.clone()), axum::body::Bytes::from(tar)).await;
    assert_eq!(resp.status(), StatusCode::OK, "load succeeds");
    let loaded = app2
        .inner
        .lock()
        .await
        .images
        .iter()
        .find(|i| i.name == "lifecycle:latest")
        .cloned()
        .expect("loaded image present");
    assert_eq!(loaded.stop_signal, "SIGQUIT", "stop signal round-trips");
    assert_eq!(loaded.img_volumes, vec!["/data", "/cache"], "volumes round-trip");
    let hc = loaded.healthcheck.expect("healthcheck round-trips");
    assert_eq!(hc.test, vec!["CMD", "true"]);
    assert_eq!(hc.interval, 5);
}

// ---- Finding 18: image id is a stable sha256 shared by list + inspect --------------------------
#[tokio::test]
async fn image_id_is_stable_sha256_shared_by_list_and_inspect() {
    let app = test_app();
    seed_image_rootfs(&app, "idimg:1.0", "/store/idimg-rootfs").await;

    // list id
    let axum::Json(list) = crate::images::images_json(State(app.clone())).await;
    let list_id = serde_json::to_value(&list).unwrap()[0]["Id"].as_str().unwrap().to_string();
    // inspect id
    let resp =
        crate::images::image_inspect(State(app.clone()), Path("idimg:1.0".into())).await;
    let inspect = to_body_json(resp).await;
    let inspect_id = inspect["Id"].as_str().unwrap().to_string();

    assert_eq!(list_id, inspect_id, "list id == inspect id for the same image");
    // sha256:<64 lowercase hex> — a real digest shape, not a tiled/synthetic placeholder.
    let hex = list_id.strip_prefix("sha256:").expect("sha256: prefix");
    assert_eq!(hex.len(), 64, "64 hex chars: {list_id}");
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "lowercase hex");
    // Deterministic for the same content.
    let axum::Json(list2) = crate::images::images_json(State(app.clone())).await;
    let list_id2 = serde_json::to_value(&list2).unwrap()[0]["Id"].as_str().unwrap().to_string();
    assert_eq!(list_id, list_id2, "stable across calls");
}

// ---- 11. images_json wire shape --------------------------------------------------------------
#[tokio::test]
async fn images_json_wire_shape() {
    let app = test_app();
    seed_image(&app, "alpine:3.19", 12345).await;
    let axum::Json(list) = crate::images::images_json(State(app.clone())).await;
    assert_eq!(list.len(), 1);
    let v = serde_json::to_value(&list).unwrap();
    let img = &v.as_array().unwrap()[0];
    let obj = img.as_object().unwrap();
    for key in [
        "Id", "RepoTags", "Created", "Size", "VirtualSize", "ParentId", "RepoDigests",
        "SharedSize", "Labels", "Containers",
    ] {
        assert!(obj.contains_key(key), "image summary missing wire key {key}");
    }
    assert!(
        img["Id"].as_str().unwrap().starts_with("sha256:"),
        "Id must be a sha256: digest"
    );
    assert_eq!(
        img["RepoTags"].as_array().unwrap()[0], "alpine:3.19",
        "RepoTags preserves the explicit tag"
    );
    assert_eq!(img["Created"], 12345);
    assert!(img["Size"].is_i64(), "Size is a number");
}

// ---- 12. system_df: both flat lists + nested *Usage envelopes, counts match seed ------------
#[tokio::test]
async fn system_df_wire_shape_and_counts() {
    let app = test_app();
    seed_container(&app, "df-run000000", "running").await;
    seed_container(&app, "df-exit00000", "exited").await;
    seed_image(&app, "alpine:3.19", 1).await;
    seed_volume(&app, "dfvol", /*in_use=*/ false).await;

    let axum::Json(df) = crate::system::system_df(State(app.clone())).await;
    let v = serde_json::to_value(&df).unwrap();
    let obj = v.as_object().unwrap();
    // Top-level flat arrays + scalars.
    for key in [
        "LayersSize", "Images", "Containers", "Volumes", "BuildCache", "BuilderSize",
        "ImageUsage", "ContainerUsage", "VolumeUsage", "BuildCacheUsage",
    ] {
        assert!(obj.contains_key(key), "system df missing top-level key {key}");
    }
    assert_eq!(v["Images"].as_array().unwrap().len(), 1, "one image seeded");
    assert_eq!(
        v["Containers"].as_array().unwrap().len(),
        2,
        "df lists ALL containers (running + exited)"
    );
    assert_eq!(v["Volumes"].as_array().unwrap().len(), 1, "one volume seeded");
    assert!(v["BuildCache"].is_array());
    // Nested *Usage envelopes mirror the counts current clients read.
    assert_eq!(v["ImageUsage"]["TotalCount"], 1);
    assert_eq!(v["ContainerUsage"]["TotalCount"], 2);
    assert_eq!(v["ContainerUsage"]["ActiveCount"], 1, "one running container");
    assert_eq!(v["VolumeUsage"]["TotalCount"], 1);
    for key in ["ActiveCount", "TotalCount", "Reclaimable", "TotalSize", "Items"] {
        assert!(
            v["ImageUsage"].as_object().unwrap().contains_key(key),
            "Usage envelope missing {key}"
        );
    }
}

// ---- 15. image_tag: alias an image under a new repo:tag -> 201 + new RepoTag present -----------
#[tokio::test]
async fn image_tag_creates_new_repotag_sharing_rootfs() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine:3.19", "/store/alpine-rootfs").await;
    let q: crate::images::TagQ =
        serde_json::from_value(serde_json::json!({"repo": "myalpine", "tag": "v2"})).unwrap();
    let resp = crate::images::image_tag(State(app.clone()), Path("alpine:3.19".into()), Query(q))
        .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let g = app.inner.lock().await;
    let tagged = g
        .images
        .iter()
        .find(|i| i.name == "myalpine:v2")
        .expect("new repo:tag must be present");
    assert_eq!(
        tagged.rootfs, "/store/alpine-rootfs",
        "the alias shares the source image's rootfs"
    );
    assert!(
        g.images.iter().any(|i| i.name == "alpine:3.19"),
        "source tag must remain"
    );
}

#[tokio::test]
async fn image_tag_missing_source_is_404() {
    let app = test_app();
    let q: crate::images::TagQ =
        serde_json::from_value(serde_json::json!({"repo": "dst"})).unwrap();
    let resp =
        crate::images::image_tag(State(app.clone()), Path("ghost".into()), Query(q)).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn image_tag_empty_repo_is_400() {
    let app = test_app();
    seed_image_rootfs(&app, "alpine:3.19", "/store/alpine-rootfs").await;
    // repo param absent -> unwrap_or_default() == "" -> bad_request("repo required").
    let q: crate::images::TagQ = serde_json::from_value(serde_json::json!({})).unwrap();
    let resp = crate::images::image_tag(State(app.clone()), Path("alpine:3.19".into()), Query(q))
        .await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        app.inner.lock().await.images.len(),
        1,
        "no new tag on a rejected tag"
    );
}

// ---- 16. image_delete (rmi): shared-rootfs refcount ------------------------------------------
#[tokio::test]
async fn image_rmi_shared_rootfs_untags_only_and_keeps_sibling() {
    let app = test_app();
    // Two tags of the SAME rootfs (a `docker tag` alias).
    seed_image_rootfs(&app, "alpine:3.19", "/store/shared-rootfs").await;
    seed_image_rootfs(&app, "myalpine:latest", "/store/shared-rootfs").await;

    let resp =
        crate::images::image_delete(State(app.clone()), Path("alpine:3.19".into()), Query(empty_q())).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let report = to_body_json(resp).await;
    let arr = report.as_array().expect("rmi report is an array");
    // Only an Untagged record — the shared rootfs is still referenced, so NO Deleted record.
    assert!(
        arr.iter().any(|r| r.get("Untagged").is_some()),
        "expected an Untagged record: {report}"
    );
    assert!(
        !arr.iter().any(|r| r.get("Deleted").is_some()),
        "shared rootfs must NOT be deleted while a sibling tag references it: {report}"
    );
    let g = app.inner.lock().await;
    assert!(
        !g.images.iter().any(|i| i.name == "alpine:3.19"),
        "the rmi'd tag is gone"
    );
    assert!(
        g.images.iter().any(|i| i.name == "myalpine:latest"),
        "the sibling tag sharing the rootfs must survive"
    );
}

#[tokio::test]
async fn image_rmi_last_ref_removes_image_and_reports_deleted() {
    let app = test_app();
    // Sole tag of this rootfs; rootfs lives OUTSIDE the temp images dir so remove_image_dir is a
    // store-guarded no-op (no real fs deletion), letting us assert the Deleted record cleanly.
    seed_image_rootfs(&app, "solo:1.0", "/store/solo-rootfs/rootfs").await;
    let resp = crate::images::image_delete(State(app.clone()), Path("solo:1.0".into()), Query(empty_q())).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let report = to_body_json(resp).await;
    let arr = report.as_array().unwrap();
    assert!(
        arr.iter().any(|r| r.get("Untagged").is_some()),
        "expected Untagged: {report}"
    );
    assert!(
        arr.iter().any(|r| {
            r.get("Deleted")
                .and_then(|d| d.as_str())
                .is_some_and(|s| s.starts_with("sha256:"))
        }),
        "last-ref rmi reports a Deleted sha256 record: {report}"
    );
    assert!(
        app.inner.lock().await.images.is_empty(),
        "the only image is removed"
    );
}

#[tokio::test]
async fn image_rmi_missing_is_404() {
    let app = test_app();
    let resp = crate::images::image_delete(State(app.clone()), Path("ghost".into()), Query(empty_q())).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---- FLOW 8: image tag/untag/delete refcount lifecycle across a shared rootfs ------------------
// docker: `docker tag a:1 a:2` aliases the SAME layers under a second repo:tag; `rmi a:1` while a:2
// still points at those layers is an UNTAG only (layers survive); `rmi a:2` (now the last reference)
// truly DELETES the image; the store is then empty. Drives tag -> list -> rmi -> rmi -> list.
#[tokio::test]
async fn flow_image_tag_untag_delete_shared_rootfs_lifecycle() {
    let app = test_app();
    // Rootfs lives OUTSIDE the temp images dir so the store-guarded on-disk removal is a safe no-op.
    seed_image_rootfs(&app, "a:1", "/store/shared-R/rootfs").await;

    // Step 1: `docker tag a:1 a:2` — 201, a:2 present and sharing a:1's rootfs.
    let q: crate::images::TagQ =
        serde_json::from_value(serde_json::json!({"repo": "a", "tag": "2"})).unwrap();
    let r = crate::images::image_tag(State(app.clone()), Path("a:1".into()), Query(q)).await;
    assert_eq!(r.status(), StatusCode::CREATED);
    {
        let g = app.inner.lock().await;
        let a2 = g.images.iter().find(|i| i.name == "a:2").expect("a:2 aliased");
        assert_eq!(a2.rootfs, "/store/shared-R/rootfs", "alias shares the source rootfs");
    }

    // Step 2: images_json lists BOTH tags.
    let tags = image_repotags(&app).await;
    assert!(tags.contains("a:1") && tags.contains("a:2"), "both tags listed: {tags:?}");

    // Step 3: `rmi a:1` — 200, an UNTAG only (a:2 still references the rootfs) — no Deleted record.
    let r = crate::images::image_delete(State(app.clone()), Path("a:1".into()), Query(empty_q())).await;
    assert_eq!(r.status(), StatusCode::OK);
    let report = to_body_json(r).await;
    let arr = report.as_array().unwrap();
    assert!(arr.iter().any(|x| x.get("Untagged").is_some()), "a:1 untag: {report}");
    assert!(
        !arr.iter().any(|x| x.get("Deleted").is_some()),
        "shared rootfs must NOT be Deleted while a:2 references it: {report}"
    );
    {
        let g = app.inner.lock().await;
        assert!(!g.images.iter().any(|i| i.name == "a:1"), "a:1 gone");
        assert!(g.images.iter().any(|i| i.name == "a:2"), "sibling a:2 survives");
    }

    // Step 4: `rmi a:2` — now the LAST reference, so a true Deleted (sha256) record.
    let r = crate::images::image_delete(State(app.clone()), Path("a:2".into()), Query(empty_q())).await;
    assert_eq!(r.status(), StatusCode::OK);
    let report = to_body_json(r).await;
    let arr = report.as_array().unwrap();
    assert!(arr.iter().any(|x| x.get("Untagged").is_some()), "a:2 untag: {report}");
    assert!(
        arr.iter().any(|x| {
            x.get("Deleted")
                .and_then(|d| d.as_str())
                .is_some_and(|s| s.starts_with("sha256:"))
        }),
        "last-ref rmi reports a Deleted sha256: {report}"
    );

    // Step 5: images_json is empty.
    assert!(image_repotags(&app).await.is_empty(), "store empty after both tags removed");
}

// ---- FLOW 9: rmi an image a container still references ----------------------------------------
// docker contract: `docker rmi <img>` while a container references that image is a 409 Conflict
// ("conflict: unable to delete <id> (must be forced) - image is being used by container <cid>");
// the image survives unless `--force` is used. This drives create-from-image THEN rmi and asserts
// dd's ACTUAL behavior. ***DIVERGENCE (bug):*** dd's `image_delete` never consults `Inner.containers`,
// so it happily UNTAGS+DELETES an in-use image and returns 200 — the referencing container is left
// dangling on a now-absent image. Flagged, not fixed.
#[tokio::test]
async fn flow_rmi_image_in_use_by_container_diverges_from_docker_409() {
    let app = test_app();
    seed_image_rootfs(&app, "busy:1", "/store/busy-R/rootfs").await;

    // A container created FROM busy:1 (engine-free create; records state only).
    let r = crate::containers::containers_create(
        State(app.clone()),
        Query(create_q(serde_json::json!({}))),
        create_body(serde_json::json!({"Image":"busy:1"})),
    )
    .await;
    assert_eq!(r.status(), StatusCode::CREATED);
    let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();
    assert_eq!(
        app.inner.lock().await.containers[&cid].image, "busy:1",
        "the container references busy:1"
    );

    // rmi of the last tag while a container references it is a 409 (docker image-in-use rule) — the
    // image survives.
    let r =
        crate::images::image_delete(State(app.clone()), Path("busy:1".into()), Query(empty_q()))
            .await;
    assert_eq!(r.status(), StatusCode::CONFLICT, "rmi of an in-use image is 409");
    assert!(
        app.inner.lock().await.images.iter().any(|i| i.name == "busy:1"),
        "the in-use image survives the refused rmi"
    );

    // `docker rmi -f` forces it: image removed, the container left dangling (docker's behavior).
    let forced: crate::images::RmiQ =
        serde_json::from_value(serde_json::json!({"force": "true"})).unwrap();
    let r =
        crate::images::image_delete(State(app.clone()), Path("busy:1".into()), Query(forced)).await;
    assert_eq!(r.status(), StatusCode::OK, "forced rmi succeeds");
    let g = app.inner.lock().await;
    assert!(!g.images.iter().any(|i| i.name == "busy:1"), "forced rmi removed the image");
    assert!(g.containers.contains_key(&cid), "the container remains (now dangling)");
}

// ---- FLOW 21: 3-tag rmi chain deleting the MIDDLE tag — rootfs survives until the LAST tag -----
// docker: `tag a:1 a:2`, `tag a:1 a:3` (all three share rootfs R); `rmi a:2` and `rmi a:1` are each
// UNTAG-only (a sibling still references R); `rmi a:3` is the LAST reference and truly DELETES R.
// Extends FLOW 8 (2 tags, in order) with a THIRD tag and a MIDDLE-first deletion order.
#[tokio::test]
async fn flow_image_three_tag_chain_delete_middle_first() {
    let app = test_app();
    seed_image_rootfs(&app, "a:1", "/store/shared-R3/rootfs").await;

    // Step 1: tag a:1 as a:2 and a:3 — 201 each; all three share R.
    for t in ["2", "3"] {
        let q: crate::images::TagQ =
            serde_json::from_value(serde_json::json!({"repo": "a", "tag": t})).unwrap();
        let r = crate::images::image_tag(State(app.clone()), Path("a:1".into()), Query(q)).await;
        assert_eq!(r.status(), StatusCode::CREATED, "tag a:{t} created");
    }
    {
        let g = app.inner.lock().await;
        for name in ["a:1", "a:2", "a:3"] {
            let i = g.images.iter().find(|i| i.name == name).expect("tag present");
            assert_eq!(i.rootfs, "/store/shared-R3/rootfs", "{name} shares R");
        }
    }
    let tags = image_repotags(&app).await;
    assert!(
        tags.contains("a:1") && tags.contains("a:2") && tags.contains("a:3"),
        "all three tags listed: {tags:?}"
    );

    // Step 2: `rmi a:2` (the MIDDLE tag) — UNTAG only; a:1 + a:3 (and R) survive.
    let r = crate::images::image_delete(State(app.clone()), Path("a:2".into()), Query(empty_q())).await;
    assert_eq!(r.status(), StatusCode::OK);
    let arr = to_body_json(r).await;
    let arr = arr.as_array().unwrap();
    assert!(arr.iter().any(|x| x.get("Untagged").is_some()), "a:2 untagged: {arr:?}");
    assert!(
        !arr.iter().any(|x| x.get("Deleted").is_some()),
        "rmi of a MIDDLE tag must NOT delete the shared rootfs: {arr:?}"
    );
    {
        let g = app.inner.lock().await;
        assert!(!g.images.iter().any(|i| i.name == "a:2"), "a:2 gone");
        assert!(g.images.iter().any(|i| i.name == "a:1"), "a:1 survives");
        assert!(g.images.iter().any(|i| i.name == "a:3"), "a:3 survives");
    }

    // Step 3: `rmi a:1` — still UNTAG only (a:3 keeps R alive).
    let r = crate::images::image_delete(State(app.clone()), Path("a:1".into()), Query(empty_q())).await;
    assert_eq!(r.status(), StatusCode::OK);
    let arr = to_body_json(r).await;
    let arr = arr.as_array().unwrap();
    assert!(arr.iter().any(|x| x.get("Untagged").is_some()), "a:1 untagged: {arr:?}");
    assert!(
        !arr.iter().any(|x| x.get("Deleted").is_some()),
        "with a:3 still referencing R, a:1 rmi is untag-only: {arr:?}"
    );
    assert!(
        app.inner.lock().await.images.iter().any(|i| i.name == "a:3"),
        "a:3 (the last tag) survives"
    );

    // Step 4: `rmi a:3` — the LAST reference; now a true Deleted(sha256) record and an empty store.
    let r = crate::images::image_delete(State(app.clone()), Path("a:3".into()), Query(empty_q())).await;
    assert_eq!(r.status(), StatusCode::OK);
    let arr = to_body_json(r).await;
    let arr = arr.as_array().unwrap();
    assert!(arr.iter().any(|x| x.get("Untagged").is_some()), "a:3 untagged: {arr:?}");
    assert!(
        arr.iter().any(|x| {
            x.get("Deleted")
                .and_then(|d| d.as_str())
                .is_some_and(|s| s.starts_with("sha256:"))
        }),
        "the LAST tag's rmi reports a Deleted sha256 (rootfs R finally removed): {arr:?}"
    );
    assert!(image_repotags(&app).await.is_empty(), "store empty after the last tag is removed");
}
