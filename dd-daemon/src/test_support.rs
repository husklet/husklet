//! Test-only support harness for the stateful async handlers.
//!
//! The daemon's HTTP handlers take axum extractors and mutate the shared [`App`] state behind an async
//! `Mutex`. Their *pure-state* and *error-path* branches (the ones that reject/short-circuit BEFORE
//! touching `dd_jit::Runtime`/spawn) are exercised here by constructing the extractors directly and
//! asserting the returned status code plus the resulting in-memory state. This locks the
//! Docker-API-compliance fixes (kill-on-exited 409, network-delete 403, stop/start 304, exec 409,
//! ps-includes-paused, …) that were previously untestable because they live inside stateful handlers.
//!
//! Engine-reaching branches (a *successful* start/stop that spawns a guest) are deliberately NOT
//! exercised — there is no JIT engine on the CI/Linux host, and those paths call into `dd_jit`.
#![cfg(test)]

use crate::events::new_bus;
use crate::model::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Build a fresh [`App`] backed by empty [`Inner`] state and a real event bus, with unique temp dirs so
/// concurrent tests never collide on `state.json`/volumes. The dirs are created eagerly so the handlers'
/// `save_state` / volume-`create_dir_all` best-effort writes succeed. Temp dirs are tiny and left behind
/// (best-effort; a RAII guard across the async lock is not worth the complexity).
pub(crate) fn test_app() -> App {
    let base = std::env::temp_dir().join(format!(
        "dd-daemon-test-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let volumes_dir = base.join("volumes");
    let images_dir = base.join("images");
    let _ = std::fs::create_dir_all(&volumes_dir);
    let _ = std::fs::create_dir_all(&images_dir);
    let _ = std::fs::create_dir_all(&base);
    App {
        inner: Arc::new(Mutex::new(Inner::default())),
        state_path: base.join("state.json").to_string_lossy().into_owned(),
        volumes_dir: volumes_dir.to_string_lossy().into_owned(),
        images_dir: images_dir.to_string_lossy().into_owned(),
        events: new_bus(),
    }
}

/// Insert a minimal container with the given id + lifecycle status (e.g. "running"/"paused"/"exited").
/// Seeds `finished_at` for exited containers so a "no rewrite" assertion has a value to compare against.
pub(crate) async fn seed_container(app: &App, id: &str, status: &str) {
    let mut g = app.inner.lock().await;
    let finished_at = if status == "exited" { 1000 } else { 0 };
    g.containers.insert(
        id.to_string(),
        Container {
            id: id.to_string(),
            image: "alpine".into(),
            status: status.to_string(),
            finished_at,
            started_at: if status == "running" || status == "paused" {
                500
            } else {
                0
            },
            ..Default::default()
        },
    );
}

/// Push a user-defined network. `with_container` adds a connected container id to `containers`, which is
/// what `network_delete` inspects to refuse removal (403 "has active endpoints").
pub(crate) async fn seed_network(app: &App, name: &str, with_container: bool) {
    let mut g = app.inner.lock().await;
    g.networks.push(Net {
        id: format!("netid-{name}"),
        name: name.to_string(),
        driver: "bridge".into(),
        scope: "local".into(),
        containers: if with_container {
            vec!["c-connected".into()]
        } else {
            vec![]
        },
        created: 0,
        subnet: "172.20.0.0/16".into(),
        gateway: "172.20.0.1".into(),
        endpoints: std::collections::HashMap::new(),
    });
}

/// Seed the predefined `bridge` network (whose removal is always refused with 403).
pub(crate) async fn seed_predefined_bridge(app: &App) {
    let mut g = app.inner.lock().await;
    g.networks = crate::networks::default_networks();
}

/// Push a named volume. `in_use` also seeds a container binding it by name, so `volume_delete`/prune see
/// it as referenced (409 "volume is in use").
pub(crate) async fn seed_volume(app: &App, name: &str, in_use: bool) {
    let mut g = app.inner.lock().await;
    g.volumes.push(Vol {
        name: name.to_string(),
        mountpoint: format!("/mp/{name}"),
        created_at: 0,
        driver: "local".into(),
        options: std::collections::HashMap::new(),
        labels: std::collections::HashMap::new(),
    });
    if in_use {
        g.containers.insert(
            "vol-user".into(),
            Container {
                id: "vol-user".into(),
                status: "running".into(),
                binds: vec![format!("{name}:/data")],
                ..Default::default()
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Path, Query, State};
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    // ---- 1. containers_kill on an exited container -> 409, state UNCHANGED ------------------------
    #[tokio::test]
    async fn kill_exited_container_is_409_and_unchanged() {
        let app = test_app();
        seed_container(&app, "c1", "exited").await;
        let resp = crate::containers::containers_kill(
            State(app.clone()),
            Path("c1".into()),
            Query(empty_q()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let g = app.inner.lock().await;
        let c = &g.containers["c1"];
        assert_eq!(c.status, "exited", "status must be untouched");
        assert_eq!(c.finished_at, 1000, "finished_at must not be rewritten");
    }

    // ---- 2. network_delete: connected user net -> 403, predefined -> 403, empty user -> 204 ------
    #[tokio::test]
    async fn network_delete_with_connected_container_is_403() {
        let app = test_app();
        seed_network(&app, "mynet", /*with_container=*/ true).await;
        let resp =
            crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(
            app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
            "network must remain"
        );
    }

    #[tokio::test]
    async fn network_delete_predefined_bridge_is_403() {
        let app = test_app();
        seed_predefined_bridge(&app).await;
        let resp =
            crate::networks::network_delete(State(app.clone()), Path("bridge".into())).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(app.inner.lock().await.networks.iter().any(|n| n.name == "bridge"));
    }

    #[tokio::test]
    async fn network_delete_empty_user_net_is_204_and_removed() {
        let app = test_app();
        seed_network(&app, "mynet", /*with_container=*/ false).await;
        let resp =
            crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(
            !app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
            "network must be gone"
        );
    }

    // ---- 3. containers_stop (do_stop) on an already-stopped container -> 304, state unchanged -----
    #[tokio::test]
    async fn stop_already_exited_is_304_and_unchanged() {
        let app = test_app();
        seed_container(&app, "c1", "exited").await;
        let resp = crate::containers::containers_stop(
            State(app.clone()),
            Path("c1".into()),
            Query(empty_q()),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        let g = app.inner.lock().await;
        let c = &g.containers["c1"];
        assert_eq!(c.status, "exited");
        assert_eq!(c.finished_at, 1000, "finished_at must not be rewritten");
    }

    // ---- 4. containers_start on already-running / already-paused -> 304, started_at not reset -----
    #[tokio::test]
    async fn start_already_running_is_304_and_unchanged() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;
        let resp =
            crate::containers::containers_start(State(app.clone()), Path("c1".into())).await;
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        let g = app.inner.lock().await;
        let c = &g.containers["c1"];
        assert_eq!(c.status, "running");
        assert_eq!(c.started_at, 500, "started_at must not be reset");
    }

    #[tokio::test]
    async fn start_already_paused_is_304_and_unchanged() {
        let app = test_app();
        seed_container(&app, "c1", "paused").await;
        let resp =
            crate::containers::containers_start(State(app.clone()), Path("c1".into())).await;
        assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
        let g = app.inner.lock().await;
        let c = &g.containers["c1"];
        assert_eq!(c.status, "paused");
        assert_eq!(c.started_at, 500);
    }

    // ---- 5. exec_create on paused -> 409 "is paused"; on exited -> 409 "is not running" -----------
    #[tokio::test]
    async fn exec_create_on_paused_is_409_is_paused() {
        let app = test_app();
        seed_container(&app, "c1", "paused").await;
        let body = axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap());
        let resp =
            crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = to_body_string(resp).await;
        assert!(body.contains("is paused"), "got: {body}");
        assert!(
            app.inner.lock().await.execs.is_empty(),
            "no exec must be recorded"
        );
    }

    #[tokio::test]
    async fn exec_create_on_exited_is_409_not_running() {
        let app = test_app();
        seed_container(&app, "c1", "exited").await;
        let body = axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap());
        let resp =
            crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = to_body_string(resp).await;
        assert!(body.contains("is not running"), "got: {body}");
        assert!(app.inner.lock().await.execs.is_empty());
    }

    // ---- 6. containers_json (no `all`) INCLUDES a paused container (ps-paused fix) ----------------
    #[tokio::test]
    async fn containers_json_includes_paused_without_all() {
        let app = test_app();
        seed_container(&app, "prunning", "running").await;
        seed_container(&app, "ppaused", "paused").await;
        seed_container(&app, "pexited", "exited").await;
        let axum::Json(list) =
            crate::containers::containers_json(State(app.clone()), Query(empty_q())).await;
        let ids: Vec<&str> = list.iter().map(|c| c.id.as_str()).collect();
        assert!(ids.contains(&"prunning"), "running must be listed");
        assert!(ids.contains(&"ppaused"), "paused must be listed (ps-paused fix)");
        assert!(
            !ids.contains(&"pexited"),
            "exited must NOT be listed without all"
        );
    }

    // ---- 7. networks_create -> 201 + present; duplicate name -> 409 ------------------------------
    #[tokio::test]
    async fn network_create_then_duplicate() {
        let app = test_app();
        let body = axum::Json(serde_json::from_value(serde_json::json!({"Name":"net1"})).unwrap());
        let resp = crate::networks::networks_create(State(app.clone()), body).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(app.inner.lock().await.networks.iter().any(|n| n.name == "net1"));

        let body2 = axum::Json(serde_json::from_value(serde_json::json!({"Name":"net1"})).unwrap());
        let resp2 = crate::networks::networks_create(State(app.clone()), body2).await;
        assert_eq!(resp2.status(), StatusCode::CONFLICT);
        assert_eq!(
            app.inner.lock().await.networks.iter().filter(|n| n.name == "net1").count(),
            1,
            "duplicate must not be inserted"
        );
    }

    // ---- 8. volumes_create -> 201; volume_delete in-use -> 409; free -> 204 ----------------------
    #[tokio::test]
    async fn volume_create_is_201_and_present() {
        let app = test_app();
        let body = axum::Json(serde_json::from_value(serde_json::json!({"Name":"vol1"})).unwrap());
        let resp = crate::volumes::volumes_create(State(app.clone()), body).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(app.inner.lock().await.volumes.iter().any(|v| v.name == "vol1"));
    }

    #[tokio::test]
    async fn volume_delete_in_use_is_409() {
        let app = test_app();
        seed_volume(&app, "vol1", /*in_use=*/ true).await;
        let resp = crate::volumes::volume_delete(State(app.clone()), Path("vol1".into())).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == "vol1"),
            "in-use volume must remain"
        );
    }

    #[tokio::test]
    async fn volume_delete_free_is_204_and_removed() {
        let app = test_app();
        seed_volume(&app, "vol1", /*in_use=*/ false).await;
        let resp = crate::volumes::volume_delete(State(app.clone()), Path("vol1".into())).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert!(
            !app.inner.lock().await.volumes.iter().any(|v| v.name == "vol1"),
            "free volume must be removed"
        );
    }

    // ==============================================================================================
    // READ / LIST handlers — assert the exact JSON WIRE SHAPE docker clients (CLI/bollard) parse.
    // These drive the handler, extract the response BODY, and characterize the emitted keys/casing
    // against the seeded state. All handlers here are state-only (none reach `dd_jit::Runtime`).
    // ==============================================================================================

    /// Push a minimal image into `Inner.images` (there is no shared seed helper for images).
    async fn seed_image(app: &App, name: &str, created: i64) {
        let mut g = app.inner.lock().await;
        g.images.push(Image {
            name: name.to_string(),
            created,
            ..Default::default()
        });
    }

    // ---- 9. containers_json wire shape: per-row docker keys + Names leading-slash ----------------
    #[tokio::test]
    async fn containers_json_wire_shape_and_all_filter() {
        let app = test_app();
        seed_container(&app, "crunning0000", "running").await;
        seed_container(&app, "cpaused00000", "paused").await;
        seed_container(&app, "cexited00000", "exited").await;

        // Default (no `all`): running + paused only (exited hidden).
        let axum::Json(def) =
            crate::containers::containers_json(State(app.clone()), Query(empty_q())).await;
        assert_eq!(def.len(), 2, "default ps lists running+paused, not exited");

        // `all=true`: every container.
        let all_q: crate::containers::PsQ =
            serde_json::from_value(serde_json::json!({"all": "true"})).unwrap();
        let axum::Json(all) =
            crate::containers::containers_json(State(app.clone()), Query(all_q)).await;
        assert_eq!(all.len(), 3, "all=true lists every container");

        // Serialize to the wire and assert the exact docker keys/casing on each row.
        let v = serde_json::to_value(&all).unwrap();
        let row = &v.as_array().unwrap()[0];
        let obj = row.as_object().unwrap();
        for key in [
            "Id", "Names", "Image", "State", "Status", "Ports", "Labels", "Command", "Created",
            "Mounts", "ExitCode",
        ] {
            assert!(obj.contains_key(key), "row missing wire key {key}: {row}");
        }
        // `--size` was NOT requested → SizeRw/SizeRootFs omitted (docker omits the keys).
        assert!(!obj.contains_key("SizeRw"), "SizeRw must be omitted without --size");
        assert!(!obj.contains_key("SizeRootFs"));
        // Names is an array whose sole entry carries a leading '/'.
        let names = row["Names"].as_array().unwrap();
        assert_eq!(names.len(), 1);
        assert!(
            names[0].as_str().unwrap().starts_with('/'),
            "Names entry must start with '/': {}",
            names[0]
        );
        assert!(row["Ports"].is_array(), "Ports is an array");
        assert!(row["Image"].as_str().unwrap() == "alpine");
    }

    // ---- 10. containers_inspect wire shape: top-level + nested State keys, Name '/' -------------
    #[tokio::test]
    async fn containers_inspect_wire_shape() {
        let app = test_app();
        seed_container(&app, "inspectme000", "exited").await;
        let resp = crate::containers::containers_inspect(State(app.clone()), Path("inspectme000".into()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = to_body_json(resp).await;
        let obj = v.as_object().unwrap();
        for key in [
            "Id", "Name", "State", "Config", "HostConfig", "NetworkSettings", "Mounts", "Image",
            "Created", "RestartCount",
        ] {
            assert!(obj.contains_key(key), "inspect missing top-level key {key}");
        }
        assert!(
            v["Name"].as_str().unwrap().starts_with('/'),
            "inspect Name must start with '/'"
        );
        // Nested State carries the docker booleans + Status/ExitCode for an exited container.
        let state = v["State"].as_object().unwrap();
        for key in [
            "Status", "Running", "Paused", "Restarting", "OOMKilled", "Dead", "ExitCode", "Pid",
            "StartedAt", "FinishedAt", "Error",
        ] {
            assert!(state.contains_key(key), "State missing key {key}");
        }
        assert_eq!(v["State"]["Status"], "exited");
        assert_eq!(v["State"]["Running"], false, "exited container is not Running");
        // Config/HostConfig/NetworkSettings are objects (not null).
        assert!(v["Config"].is_object());
        assert!(v["HostConfig"].is_object());
        assert!(v["NetworkSettings"]["Ports"].is_object() || v["NetworkSettings"]["Ports"].is_null());
    }

    #[tokio::test]
    async fn containers_inspect_missing_is_404() {
        let app = test_app();
        let resp = crate::containers::containers_inspect(State(app.clone()), Path("nope".into()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
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

    // ---- 13. network_inspect: object key fields; missing -> 404 ---------------------------------
    #[tokio::test]
    async fn network_inspect_wire_shape() {
        let app = test_app();
        seed_network(&app, "mynet", /*with_container=*/ false).await;
        let resp = crate::networks::network_inspect(State(app.clone()), Path("mynet".into()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = to_body_json(resp).await;
        for key in [
            "Id", "Name", "Driver", "Scope", "Containers", "Created", "EnableIPv6", "Internal",
            "IPAM",
        ] {
            assert!(v.as_object().unwrap().contains_key(key), "network missing key {key}");
        }
        assert_eq!(v["Name"], "mynet");
        assert_eq!(v["Driver"], "bridge");
        assert_eq!(v["Scope"], "local");
        assert_eq!(v["IPAM"]["Config"][0]["Subnet"], "172.20.0.0/16");
        assert_eq!(v["IPAM"]["Config"][0]["Gateway"], "172.20.0.1");
    }

    #[tokio::test]
    async fn network_inspect_missing_is_404() {
        let app = test_app();
        let resp = crate::networks::network_inspect(State(app.clone()), Path("ghost".into()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ---- 14. volume_inspect: object key fields; missing -> 404 ----------------------------------
    #[tokio::test]
    async fn volume_inspect_wire_shape() {
        let app = test_app();
        seed_volume(&app, "vol1", /*in_use=*/ false).await;
        let resp = crate::volumes::volume_inspect(State(app.clone()), Path("vol1".into()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = to_body_json(resp).await;
        for key in ["Name", "Driver", "Mountpoint", "CreatedAt", "Scope", "Labels", "Options"] {
            assert!(v.as_object().unwrap().contains_key(key), "volume missing key {key}");
        }
        assert_eq!(v["Name"], "vol1");
        assert_eq!(v["Driver"], "local");
        assert_eq!(v["Mountpoint"], "/mp/vol1");
        assert_eq!(v["Scope"], "local");
    }

    #[tokio::test]
    async fn volume_inspect_missing_is_404() {
        let app = test_app();
        let resp = crate::volumes::volume_inspect(State(app.clone()), Path("ghost".into()))
            .await
            .into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ==============================================================================================
    // STATE-ONLY MUTATION / INSPECT handlers not covered above. Each drives a handler and asserts the
    // emitted status + wire shape + resulting in-memory state. None reach `dd_jit::Runtime`/spawn:
    //  - image_tag/image_delete mutate `Inner.images` only (rmi's on-disk removal is store-guarded and a
    //    safe no-op for a rootfs OUTSIDE the temp images dir — see remove_image_dir).
    //  - exec_create/exec_inspect mutate/read `Inner.execs` only (exec_START, which hijacks/streams to the
    //    engine, is deliberately NOT tested).
    //  - archive_head/archive_get resolve the container first; the missing-container 404 branch is
    //    engine/fs-free (the success path reads real rootfs/upper via tar — not exercised).
    // ==============================================================================================

    /// Push an image with an explicit `rootfs` (the shared field `image_delete`'s refcount rule keys on).
    async fn seed_image_rootfs(app: &App, name: &str, rootfs: &str) {
        let mut g = app.inner.lock().await;
        g.images.push(Image {
            name: name.to_string(),
            rootfs: rootfs.to_string(),
            ..Default::default()
        });
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

    // ---- 17. exec_create on a RUNNING container -> 201 + exec recorded; empty cmd -> 400 ----------
    #[tokio::test]
    async fn exec_create_on_running_records_exec() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;
        let body =
            axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls","-la"]})).unwrap());
        let resp = crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = to_body_json(resp).await;
        let exec_id = v["Id"].as_str().expect("exec create returns an Id").to_string();
        assert!(!exec_id.is_empty());
        let g = app.inner.lock().await;
        let exec = g.execs.get(&exec_id).expect("exec must be recorded under the returned Id");
        assert_eq!(exec.container_id, "c1");
        assert_eq!(exec.cmd, vec!["ls".to_string(), "-la".to_string()]);
        assert!(!exec.started, "a freshly created exec has not started");
    }

    #[tokio::test]
    async fn exec_create_empty_cmd_is_400() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;
        let body = axum::Json(serde_json::from_value(serde_json::json!({"Cmd":[]})).unwrap());
        let resp = crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let msg = to_body_string(resp).await;
        assert!(msg.contains("No exec command specified"), "got: {msg}");
        assert!(app.inner.lock().await.execs.is_empty(), "no exec on a rejected create");
    }

    // ---- 18. exec_inspect: docker exec-inspect JSON shape; missing -> 404 ------------------------
    #[tokio::test]
    async fn exec_inspect_wire_shape() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;
        let body = axum::Json(
            serde_json::from_value(serde_json::json!({"Cmd":["ls","-la"],"Tty":true})).unwrap(),
        );
        let created =
            crate::containers::exec_create(State(app.clone()), Path("c1".into()), body).await;
        let exec_id = to_body_json(created).await["Id"].as_str().unwrap().to_string();

        let resp = crate::containers::exec_inspect(State(app.clone()), Path(exec_id.clone())).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let v = to_body_json(resp).await;
        let obj = v.as_object().unwrap();
        for key in ["ID", "Running", "ExitCode", "ContainerID", "ProcessConfig"] {
            assert!(obj.contains_key(key), "exec inspect missing key {key}: {v}");
        }
        assert_eq!(v["ID"], exec_id);
        // No Live for this exec (start never ran) -> Running=false, ExitCode falls back to the record's 0.
        assert_eq!(v["Running"], false, "un-started exec is not Running");
        assert_eq!(v["ExitCode"], 0);
        assert_eq!(v["ContainerID"], "c1");
        // Nested ProcessConfig: docker's lowercase keys; entrypoint = argv[0], arguments = argv[1..].
        let pc = v["ProcessConfig"].as_object().unwrap();
        for key in ["tty", "privileged", "entrypoint", "arguments"] {
            assert!(pc.contains_key(key), "ProcessConfig missing key {key}");
        }
        assert_eq!(v["ProcessConfig"]["tty"], true, "Tty:true was recorded");
        assert_eq!(v["ProcessConfig"]["entrypoint"], "ls");
        assert_eq!(v["ProcessConfig"]["arguments"], serde_json::json!(["-la"]));
    }

    #[tokio::test]
    async fn exec_inspect_missing_is_404() {
        let app = test_app();
        let resp =
            crate::containers::exec_inspect(State(app.clone()), Path("noexec".into())).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let msg = to_body_string(resp).await;
        assert!(msg.contains("no such exec"), "got: {msg}");
    }

    // ---- 19. archive HEAD/GET on a MISSING container -> 404 (engine/fs-free branch only) ----------
    #[tokio::test]
    async fn archive_head_missing_container_is_404() {
        let app = test_app();
        let q: crate::archive::ArchiveQ =
            serde_json::from_value(serde_json::json!({"path": "/etc/hosts"})).unwrap();
        let resp =
            crate::archive::archive_head(State(app.clone()), Path("ghost".into()), Query(q)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn archive_get_missing_container_is_404() {
        let app = test_app();
        let q: crate::archive::ArchiveQ =
            serde_json::from_value(serde_json::json!({"path": "/etc/hosts"})).unwrap();
        let resp =
            crate::archive::archive_get(State(app.clone()), Path("ghost".into()), Query(q)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ==============================================================================================
    // MULTI-STEP INTEGRATION FLOWS — drive SEVERAL handlers IN SEQUENCE against ONE shared `test_app`
    // and assert the state carried ACROSS calls. These target INTERACTION / state-machine bugs the
    // single-handler tests above cannot see: an endpoint refcount that's set on connect but not cleared
    // on disconnect, a volume freed while still bound, a name-conflict that half-commits, etc. For each
    // flow the docker contract is stated inline, then dd's actual behavior is asserted; a clean run is
    // itself the signal that the cross-handler state machine is sound.
    //
    // Only engine-free handlers are used (none reach `dd_jit::Runtime`/spawn): networks_create /
    // network_connect / network_disconnect / network_delete / networks_prune, volumes_create /
    // volume_delete, containers_create (records state + allocates the overlay upper dir, no spawn) and
    // containers_delete (of a non-running container: state + fs reclaim only, no kill_group).
    // ==============================================================================================

    /// Build a `NetworkCreateBody` from `{"Name": name}`.
    fn net_create_body(name: &str) -> axum::Json<crate::networks::NetCreateBody> {
        axum::Json(serde_json::from_value(serde_json::json!({ "Name": name })).unwrap())
    }
    /// Build a `NetAttachBody` (connect/disconnect) from `{"Container": cref}`.
    fn net_attach_body(cref: &str) -> axum::Json<crate::networks::NetAttachBody> {
        axum::Json(serde_json::from_value(serde_json::json!({ "Container": cref })).unwrap())
    }
    /// Directly insert a container that binds a volume by name (`-v <vol>:/data`) — the shape
    /// `volume_in_use` scans. Kept minimal so a flow can assert refcount without going through create.
    async fn seed_container_binding_volume(app: &App, id: &str, vol: &str) {
        let mut g = app.inner.lock().await;
        g.containers.insert(
            id.to_string(),
            Container {
                id: id.to_string(),
                status: "exited".into(),
                binds: vec![format!("{vol}:/data")],
                ..Default::default()
            },
        );
    }
    /// Snapshot a network's membership list (`Net.containers`) by name.
    async fn net_members(app: &App, name: &str) -> Vec<String> {
        app.inner
            .lock()
            .await
            .networks
            .iter()
            .find(|n| n.name == name)
            .map(|n| n.containers.clone())
            .unwrap_or_default()
    }
    /// How many endpoints the named network currently holds (the IPAM side of membership).
    async fn net_endpoint_count(app: &App, name: &str) -> usize {
        app.inner
            .lock()
            .await
            .networks
            .iter()
            .find(|n| n.name == name)
            .map(|n| n.endpoints.len())
            .unwrap_or(0)
    }

    // ---- FLOW 1: network endpoint refcount, connect → 403-on-delete → disconnect → 204 -------------
    // docker: a user network with a connected endpoint refuses removal (403 "has active endpoints")
    // until every container is disconnected; then delete is a 204. Locks the endpoint-refcount fix.
    #[tokio::test]
    async fn network_connect_missing_container_is_404_no_phantom() {
        // Regression: connecting a NONEXISTENT container must 404 (docker "No such container"), not
        // silently insert a phantom endpoint — which used to return 200 and make the network permanently
        // undeletable (403 forever). Also: a missing NETWORK still 404s first.
        let app = test_app();
        let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
        assert_eq!(r.status(), StatusCode::CREATED);

        let r = crate::networks::network_connect(
            State(app.clone()),
            Path("mynet".into()),
            net_attach_body("ghost"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "connect of a missing container is 404");
        assert_eq!(net_members(&app, "mynet").await.len(), 0, "no phantom membership");
        assert_eq!(net_endpoint_count(&app, "mynet").await, 0, "no phantom endpoint");

        // The network stays deletable (the phantom endpoint had made this a permanent 403).
        let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT, "net with no real endpoints deletes");

        // A missing NETWORK is still resolved first -> 404 network.
        let r = crate::networks::network_connect(
            State(app.clone()),
            Path("nope".into()),
            net_attach_body("also-ghost"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "connect to a missing network is 404");
    }

    #[tokio::test]
    async fn flow_network_endpoint_refcount_lifecycle() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;

        // Step 1: create the network — 201, present, no members yet.
        let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        assert_eq!(net_members(&app, "mynet").await.len(), 0, "fresh net has no members");
        assert_eq!(net_endpoint_count(&app, "mynet").await, 0);

        // Step 2: connect c1 — 200, c1 now in membership AND endpoints.
        let r = crate::networks::network_connect(
            State(app.clone()),
            Path("mynet".into()),
            net_attach_body("c1"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(net_members(&app, "mynet").await, vec!["c1".to_string()], "c1 joined");
        assert_eq!(net_endpoint_count(&app, "mynet").await, 1, "endpoint allocated");

        // Step 3: delete while an endpoint is attached — 403, network survives (the refcount gate).
        let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN, "delete with an endpoint is 403");
        assert!(
            app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
            "refused delete must leave the network in place"
        );

        // Step 4: disconnect c1 — 200, membership AND endpoints both cleared.
        let r = crate::networks::network_disconnect(
            State(app.clone()),
            Path("mynet".into()),
            net_attach_body("c1"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(net_members(&app, "mynet").await.len(), 0, "membership cleared on disconnect");
        assert_eq!(
            net_endpoint_count(&app, "mynet").await,
            0,
            "endpoint freed on disconnect (not just membership) — the refcount must reach zero"
        );

        // Step 5: delete now that the last endpoint is gone — 204, network removed.
        let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT, "delete after disconnect is 204");
        assert!(
            !app.inner.lock().await.networks.iter().any(|n| n.name == "mynet"),
            "network gone after its last endpoint left"
        );
    }

    // ---- FLOW 2: volume in-use refcount, 409-while-bound → 204-once-free --------------------------
    // docker: removing a volume a container still references is a 409 ("volume is in use"); once no
    // container references it, remove is a 204.
    #[tokio::test]
    async fn flow_volume_in_use_then_free() {
        let app = test_app();

        // Step 1: create v1 — 201, present.
        let body = axum::Json(serde_json::from_value(serde_json::json!({"Name":"v1"})).unwrap());
        let r = crate::volumes::volumes_create(State(app.clone()), body).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        assert!(app.inner.lock().await.volumes.iter().any(|v| v.name == "v1"));

        // Step 2: a container binds v1 by name.
        seed_container_binding_volume(&app, "user1", "v1").await;

        // Step 3: delete while bound — 409, volume survives.
        let r = crate::volumes::volume_delete(State(app.clone()), Path("v1".into())).await;
        assert_eq!(r.status(), StatusCode::CONFLICT, "in-use volume delete is 409");
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == "v1"),
            "a refused (in-use) delete must not drop the volume"
        );

        // Step 4: drop the referencing container.
        app.inner.lock().await.containers.remove("user1");

        // Step 5: delete now that nothing binds it — 204, volume removed.
        let r = crate::volumes::volume_delete(State(app.clone()), Path("v1".into())).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT, "freed volume delete is 204");
        assert!(
            !app.inner.lock().await.volumes.iter().any(|v| v.name == "v1"),
            "volume gone once its last reference is removed"
        );
    }

    // ---- FLOW 3: container name conflict — second `--name web` is 409, state stays single ---------
    // docker: `create --name web` twice is a 409 Conflict; the daemon does NOT record a second
    // container. `containers_create` is engine-free (records state + allocates the overlay upper dir,
    // never spawns), so it is driven directly here.
    #[tokio::test]
    async fn flow_container_name_conflict_keeps_single() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-rootfs").await;

        let create_web = |app: App| async move {
            let q: crate::containers::CreateQ =
                serde_json::from_value(serde_json::json!({"name":"web"})).unwrap();
            let body =
                axum::Json(serde_json::from_value(serde_json::json!({"Image":"alpine"})).unwrap());
            crate::containers::containers_create(State(app), Query(q), body).await
        };

        // Step 1: first create — 201, exactly one container named `web`.
        let r = create_web(app.clone()).await;
        assert_eq!(r.status(), StatusCode::CREATED, "first --name web creates");
        assert_eq!(
            app.inner.lock().await.containers.values().filter(|c| c.name == "web").count(),
            1,
            "one container named web after the first create"
        );

        // Step 2: second create with the same name — 409, still exactly one.
        let r = create_web(app.clone()).await;
        assert_eq!(r.status(), StatusCode::CONFLICT, "duplicate --name web is 409");
        let msg = to_body_string(r).await;
        assert!(msg.contains("already in use"), "409 body names the conflict: {msg}");
        assert_eq!(
            app.inner.lock().await.containers.values().filter(|c| c.name == "web").count(),
            1,
            "a rejected duplicate must NOT record a second container"
        );
    }

    // ---- FLOW 4: duplicate net (409), predefined delete (403), prune selectivity -----------------
    // docker: creating a network name twice is a 409; the predefined bridge/host/none can never be
    // removed (403); `network prune` reclaims ONLY empty user networks — never a predefined one, never
    // one with an attached endpoint.
    #[tokio::test]
    async fn flow_network_duplicate_predefined_and_prune_selectivity() {
        let app = test_app();
        // Start from the three predefined networks (bridge/host/none), as a live daemon would.
        seed_predefined_bridge(&app).await;

        // Duplicate create: first `mynet` 201, second 409, count stays 1.
        let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
        assert_eq!(r.status(), StatusCode::CONFLICT, "duplicate network name is 409");
        assert_eq!(
            app.inner.lock().await.networks.iter().filter(|n| n.name == "mynet").count(),
            1,
            "the duplicate must not be inserted"
        );

        // Predefined delete: `bridge` is 403 and stays.
        let r = crate::networks::network_delete(State(app.clone()), Path("bridge".into())).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN, "predefined bridge delete is 403");
        assert!(app.inner.lock().await.networks.iter().any(|n| n.name == "bridge"));

        // Add an EMPTY user net (prunable) and a BUSY user net (an endpoint -> not prunable).
        crate::networks::networks_create(State(app.clone()), net_create_body("emptyuser"))
            .await;
        seed_network(&app, "busyuser", /*with_container=*/ true).await;

        // Prune: reclaims `emptyuser` + `mynet` only; keeps bridge/host/none AND the busy net.
        let axum::Json(report) = crate::networks::networks_prune(State(app.clone())).await;
        let pruned: std::collections::HashSet<&str> =
            report.networks_deleted.iter().map(|s| s.as_str()).collect();
        assert!(pruned.contains("emptyuser"), "empty user net is pruned: {pruned:?}");
        assert!(pruned.contains("mynet"), "the other empty user net is pruned: {pruned:?}");
        assert!(!pruned.contains("busyuser"), "a net with an endpoint is NOT pruned");
        for p in ["bridge", "host", "none"] {
            assert!(!pruned.contains(p), "predefined {p} is never pruned");
        }
        let names: std::collections::HashSet<String> = app
            .inner
            .lock()
            .await
            .networks
            .iter()
            .map(|n| n.name.clone())
            .collect();
        assert!(names.contains("bridge") && names.contains("host") && names.contains("none"));
        assert!(names.contains("busyuser"), "busy net survives prune");
        assert!(!names.contains("emptyuser") && !names.contains("mynet"), "empty user nets gone");
    }

    // ---- FLOW 5: lifecycle EVENTS fire across create/delete (bus read in-test via try_recv) -------
    // docker: create/destroy publish `network create` / `network destroy` on the events bus.
    // `emit_event` short-circuits when `receiver_count()==0`, so we subscribe FIRST; a broadcast
    // Receiver + non-blocking `try_recv` drains without the /events streaming handler (no deadlock).
    #[tokio::test]
    async fn flow_events_emitted_across_network_lifecycle() {
        let app = test_app();
        let mut rx = app.events.subscribe(); // must precede any emit (bus skips with 0 receivers)

        let r = crate::networks::networks_create(State(app.clone()), net_create_body("evnet")).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let r = crate::networks::network_delete(State(app.clone()), Path("evnet".into())).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);

        // Drain everything the bus buffered for us.
        let mut seen: Vec<(String, String)> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            seen.push((
                ev["Type"].as_str().unwrap_or("").to_string(),
                ev["Action"].as_str().unwrap_or("").to_string(),
            ));
        }
        assert!(
            seen.contains(&("network".into(), "create".into())),
            "a network/create event must fire: {seen:?}"
        );
        assert!(
            seen.contains(&("network".into(), "destroy".into())),
            "a network/destroy event must fire: {seen:?}"
        );
    }

    // ---- FLOW 6: container joins a net on create, and is DROPPED from it on rm (leave_network) ----
    // docker: `run --network mynet` adds the container to the net's membership; `rm` removes that
    // endpoint. Exercises create's join + delete's leave in one shared state — an interaction unit
    // tests miss (a container could be left dangling in a net's membership after removal).
    #[tokio::test]
    async fn flow_container_network_membership_join_on_create_leave_on_rm() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-rootfs").await;
        crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;

        // Create a container attached to `mynet` (NetworkMode).
        let q: crate::containers::CreateQ = serde_json::from_value(serde_json::json!({})).unwrap();
        let body = axum::Json(
            serde_json::from_value(serde_json::json!({
                "Image":"alpine",
                "HostConfig": {"NetworkMode":"mynet"}
            }))
            .unwrap(),
        );
        let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();

        // The container is now a member of `mynet`.
        assert!(
            net_members(&app, "mynet").await.contains(&cid),
            "create --network mynet must add the container to the net membership"
        );
        assert_eq!(net_endpoint_count(&app, "mynet").await, 1, "endpoint allocated on create");

        // Remove the container (created state -> engine-free delete).
        let dq: crate::containers::DeleteQ =
            serde_json::from_value(serde_json::json!({})).unwrap();
        let r = crate::containers::containers_delete(
            State(app.clone()),
            Path(cid.clone()),
            Query(dq),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);

        // rm must drop the endpoint from the net (no dangling membership).
        assert!(
            !net_members(&app, "mynet").await.contains(&cid),
            "rm must remove the container from the net membership"
        );
        assert_eq!(
            net_endpoint_count(&app, "mynet").await,
            0,
            "rm must free the endpoint too — the net is now empty and deletable"
        );
        // And the now-empty net is deletable (204), proving the leave was a true refcount decrement.
        let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
    }

    // ---- FLOW 7: anonymous volume is GC'd by `rm -v` but survives a plain `rm` (the leak fix) ------
    // docker: an anonymous volume (bare `-v /data`) is reclaimed by `rm -v`, but a plain `rm` (no -v)
    // leaves it behind (Moby removes only anonymous volumes, and only on -v). This is the exact class
    // of the just-fixed anon-volume-leak-on-`--rm`; drive create -> rm both ways and assert the volume
    // store transitions.
    #[tokio::test]
    async fn flow_anon_volume_reclaimed_by_rm_v_but_kept_by_plain_rm() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-rootfs").await;

        // Helper: create a container with a bare `-v /data` anonymous volume, return (cid, anon_name).
        async fn create_with_anon(app: &App) -> (String, String) {
            let q: crate::containers::CreateQ =
                serde_json::from_value(serde_json::json!({})).unwrap();
            let body = axum::Json(
                serde_json::from_value(serde_json::json!({
                    "Image":"alpine",
                    "HostConfig": {"Binds": ["/data"]}
                }))
                .unwrap(),
            );
            let r = crate::containers::containers_create(State(app.clone()), Query(q), body).await;
            assert_eq!(r.status(), StatusCode::CREATED);
            let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();
            let anon = {
                let g = app.inner.lock().await;
                let c = &g.containers[&cid];
                assert_eq!(c.anon_volumes.len(), 1, "a bare -v /data yields one anon volume");
                c.anon_volumes[0].clone()
            };
            // The anon volume is registered in the volume store.
            assert!(
                app.inner.lock().await.volumes.iter().any(|v| v.name == anon),
                "anon volume must be present in the store after create"
            );
            (cid, anon)
        }

        // Case A: plain `rm` (no -v) — the anonymous volume SURVIVES (docker keeps it).
        let (cid_a, anon_a) = create_with_anon(&app).await;
        let dq: crate::containers::DeleteQ =
            serde_json::from_value(serde_json::json!({})).unwrap();
        let r =
            crate::containers::containers_delete(State(app.clone()), Path(cid_a), Query(dq)).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == anon_a),
            "plain rm (no -v) must LEAVE the anonymous volume behind"
        );

        // Case B: `rm -v` — the anonymous volume is RECLAIMED (the leak fix).
        let (cid_b, anon_b) = create_with_anon(&app).await;
        let dq: crate::containers::DeleteQ =
            serde_json::from_value(serde_json::json!({"v":"true"})).unwrap();
        let r =
            crate::containers::containers_delete(State(app.clone()), Path(cid_b), Query(dq)).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert!(
            !app.inner.lock().await.volumes.iter().any(|v| v.name == anon_b),
            "rm -v must reclaim the container's anonymous volume (no leak)"
        );
    }

    // ==============================================================================================
    // MULTI-STEP INTEGRATION FLOWS — WAVE 2. Fresh handler SEQUENCES not exercised above. Same rules:
    // engine-free handlers only; state asserted after EVERY step; docker's contract stated inline, then
    // dd's actual behavior asserted. A clean run is a real signal; a divergence is flagged in the report.
    // ==============================================================================================

    /// Build a `RenameQ` query (`?name=...`) for `containers_rename`.
    fn rename_q(name: &str) -> crate::containers::RenameQ {
        serde_json::from_value(serde_json::json!({ "name": name })).unwrap()
    }
    /// Build a `CreateQ` from an object (e.g. `{"name":"web"}` or `{}`).
    fn create_q(v: serde_json::Value) -> crate::containers::CreateQ {
        serde_json::from_value(v).unwrap()
    }
    /// Build a create-body `CreateBody` from a JSON object.
    fn create_body(v: serde_json::Value) -> axum::Json<crate::containers::CreateBody> {
        axum::Json(serde_json::from_value(v).unwrap())
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

    // ---- FLOW 10: exec lifecycle — two execs recorded distinctly; pre-start Running=false; 404 -----
    // docker: each `exec create` mints a DISTINCT exec id (both persisted); `exec inspect` before any
    // start reports Running=false; inspecting a bogus id is a 404. Drives create -> inspect -> create ->
    // inspect(both) -> inspect(bogus). (exec_start hijacks/streams to the engine and is NOT driven.)
    #[tokio::test]
    async fn flow_exec_lifecycle_two_distinct_execs_prestart_and_404() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;

        // Step 1: first exec_create -> 201 + id1.
        let r = crate::containers::exec_create(
            State(app.clone()),
            Path("c1".into()),
            axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let id1 = to_body_json(r).await["Id"].as_str().unwrap().to_string();

        // Step 2: inspect id1 BEFORE any start -> Running=false, ContainerID=c1.
        let r = crate::containers::exec_inspect(State(app.clone()), Path(id1.clone())).await;
        assert_eq!(r.status(), StatusCode::OK);
        let v = to_body_json(r).await;
        assert_eq!(v["Running"], false, "un-started exec is not Running");
        assert_eq!(v["ContainerID"], "c1");

        // Step 3: a SECOND exec_create -> a DIFFERENT id; both are recorded.
        let r = crate::containers::exec_create(
            State(app.clone()),
            Path("c1".into()),
            axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["pwd"]})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let id2 = to_body_json(r).await["Id"].as_str().unwrap().to_string();
        assert_ne!(id1, id2, "each exec create mints a distinct id");
        {
            let g = app.inner.lock().await;
            assert_eq!(g.execs.len(), 2, "both execs are recorded");
            assert_eq!(g.execs[&id1].cmd, vec!["ls".to_string()]);
            assert_eq!(g.execs[&id2].cmd, vec!["pwd".to_string()]);
        }

        // Step 4: both inspect cleanly and independently.
        for id in [&id1, &id2] {
            let r = crate::containers::exec_inspect(State(app.clone()), Path(id.clone())).await;
            assert_eq!(r.status(), StatusCode::OK, "exec {id} inspects");
        }

        // Step 5: a bogus exec id is a 404.
        let r = crate::containers::exec_inspect(State(app.clone()), Path("bogusexec".into())).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
    }

    // ---- FLOW 11: create --name -> rename -> inspect/lookup by new name; update returns 200 --------
    // docker: `rename web app` frees `web` and binds `app`; inspect shows `/app`; lookup by `app` works;
    // lookup by the OLD `web` fails. `docker update` returns 200 (a `{Warnings}` envelope). *** dd NOTE:
    // update is a documented no-op — it does NOT persist the new resource limit; docker DOES reflect the
    // new Memory in a later inspect. Asserted below as a soft divergence (flagged, not a hard bug). ***
    #[tokio::test]
    async fn flow_container_rename_then_inspect_lookup_and_update_noop() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

        // Step 1: create --name web -> 201.
        let r = crate::containers::containers_create(
            State(app.clone()),
            Query(create_q(serde_json::json!({"name":"web"}))),
            create_body(serde_json::json!({"Image":"alpine"})),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();
        assert_eq!(app.inner.lock().await.containers[&cid].name, "web");

        // Step 2: rename web -> app (204); the name field updates.
        let r = crate::containers::containers_rename(
            State(app.clone()),
            Path("web".into()),
            Query(rename_q("app")),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert_eq!(app.inner.lock().await.containers[&cid].name, "app", "name rebound to app");

        // Step 3: inspect (by new name AND by id) shows the new Name with the leading '/'.
        let r = crate::containers::containers_inspect(State(app.clone()), Path("app".into()))
            .await
            .into_response();
        assert_eq!(r.status(), StatusCode::OK, "lookup by the NEW name resolves");
        assert_eq!(to_body_json(r).await["Name"], "/app");
        // The OLD name no longer resolves (docker frees it on rename) -> 404.
        let r = crate::containers::containers_inspect(State(app.clone()), Path("web".into()))
            .await
            .into_response();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "old name `web` is freed by rename");

        // Step 4: containers_json (all) finds it by the new name.
        let all_q: crate::containers::PsQ =
            serde_json::from_value(serde_json::json!({"all":"true"})).unwrap();
        let axum::Json(list) =
            crate::containers::containers_json(State(app.clone()), Query(all_q)).await;
        let v = serde_json::to_value(&list).unwrap();
        let found = v.as_array().unwrap().iter().any(|row| {
            row["Names"]
                .as_array()
                .map(|ns| ns.iter().any(|n| n == "/app"))
                .unwrap_or(false)
        });
        assert!(found, "ps lists the container under its new name /app");

        // Step 5: update with a Memory limit -> 200. dd does NOT persist it (documented no-op).
        let r = crate::containers::containers_update(
            State(app.clone()),
            Path(cid.clone()),
            axum::body::Bytes::from_static(b"{\"Memory\":16000000}"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK, "update returns 200 {{Warnings}}");
        let r = crate::containers::containers_inspect(State(app.clone()), Path(cid.clone()))
            .await
            .into_response();
        let v = to_body_json(r).await;
        assert_eq!(
            v["HostConfig"]["Memory"], 0,
            "NOTE/divergence: dd's update is a no-op — Memory stays 0; docker would reflect 16000000"
        );
    }

    // ---- FLOW 12: rename onto an EXISTING name — dd accepts a DUPLICATE (docker rejects with 409) ---
    // docker contract: `docker rename web app` when `app` is already taken is a 409 Conflict; the rename
    // is refused and both names stay distinct. *** DIVERGENCE (bug): dd's `containers_rename` never checks
    // for a target-name collision — it silently overwrites, leaving TWO containers named `app`. A later
    // `resolve_cid("app")` then resolves to an arbitrary one. Flagged, not fixed. ***
    #[tokio::test]
    async fn flow_rename_onto_existing_name_creates_duplicate_divergence() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

        // Two distinct containers: `web` and `app`.
        let mk = |app: App, name: &'static str| async move {
            let r = crate::containers::containers_create(
                State(app.clone()),
                Query(create_q(serde_json::json!({ "name": name }))),
                create_body(serde_json::json!({"Image":"alpine"})),
            )
            .await;
            assert_eq!(r.status(), StatusCode::CREATED);
            to_body_json(r).await["Id"].as_str().unwrap().to_string()
        };
        let cid_web = mk(app.clone(), "web").await;
        let _cid_app = mk(app.clone(), "app").await;

        // Rename `web` -> `app` (already taken) is a 409 (docker keeps names unique); `web` is unchanged.
        let r = crate::containers::containers_rename(
            State(app.clone()),
            Path("web".into()),
            Query(rename_q("app")),
        )
        .await;
        assert_eq!(
            r.status(),
            StatusCode::CONFLICT,
            "rename onto a taken name is 409"
        );
        // No duplicate: still exactly one `app`, and `web` kept its name.
        let g = app.inner.lock().await;
        assert_eq!(g.containers.values().filter(|c| c.name == "app").count(), 1, "no duplicate name");
        assert_eq!(g.containers[&cid_web].name, "web", "the refused rename left web unchanged");
    }

    // ---- FLOW 13: cross-resource teardown — named volume + network, then `rm` (no -v) --------------
    // docker: `run -v myvol:/data --network mynet` binds a NAMED volume and joins the net; a plain `rm`
    // (no -v) frees the network endpoint but KEEPS the named volume (docker removes only ANONYMOUS
    // volumes, and only on `-v`). Drives create -> assert both refs -> rm -> assert endpoint freed +
    // named volume kept + volume now deletable.
    #[tokio::test]
    async fn flow_teardown_named_volume_kept_network_endpoint_freed_on_rm() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;
        // A pre-existing NAMED volume and a user network.
        let r = crate::volumes::volumes_create(
            State(app.clone()),
            axum::Json(serde_json::from_value(serde_json::json!({"Name":"myvol"})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;

        // Create a container bound to myvol AND attached to mynet.
        let r = crate::containers::containers_create(
            State(app.clone()),
            Query(create_q(serde_json::json!({}))),
            create_body(serde_json::json!({
                "Image":"alpine",
                "HostConfig": {"Binds":["myvol:/data"], "NetworkMode":"mynet"}
            })),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();

        // Both cross-resource refs are live: net membership + endpoint, and the named volume is in-use.
        assert!(net_members(&app, "mynet").await.contains(&cid), "joined mynet");
        assert_eq!(net_endpoint_count(&app, "mynet").await, 1, "endpoint allocated");
        // In-use proof: delete of the bound named volume is refused (409).
        let r = crate::volumes::volume_delete(State(app.clone()), Path("myvol".into())).await;
        assert_eq!(r.status(), StatusCode::CONFLICT, "bound named volume delete is 409");
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == "myvol"),
            "the refused delete leaves the volume in place"
        );

        // Plain `rm` (no -v) on the created (non-running) container -> 204, engine-free.
        let r = crate::containers::containers_delete(
            State(app.clone()),
            Path(cid.clone()),
            Query::<crate::containers::DeleteQ>(serde_json::from_value(serde_json::json!({})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);

        // Network endpoint freed (no dangling membership); NAMED volume KEPT (docker keeps it on plain rm).
        assert!(!net_members(&app, "mynet").await.contains(&cid), "left mynet on rm");
        assert_eq!(net_endpoint_count(&app, "mynet").await, 0, "endpoint freed on rm");
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == "myvol"),
            "a NAMED volume must survive a plain rm (only anon volumes are GC'd, and only on -v)"
        );

        // The now-unreferenced named volume is freely deletable (204) — proving the ref was truly released.
        let r = crate::volumes::volume_delete(State(app.clone()), Path("myvol".into())).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT, "freed named volume deletes");
    }

    // ---- FLOW 14: volume prune selectivity — in-use kept, free reclaimed; free the user, prune again -
    // docker: `volume prune` reclaims ONLY volumes no container references; an in-use volume is kept; once
    // its last referencing container is gone, a second prune reclaims it. Drives create x2 -> bind v1 ->
    // prune -> drop user -> prune.
    #[tokio::test]
    async fn flow_volume_prune_in_use_kept_then_reclaimed() {
        let app = test_app();
        for name in ["v1", "v2"] {
            let r = crate::volumes::volumes_create(
                State(app.clone()),
                axum::Json(serde_json::from_value(serde_json::json!({ "Name": name })).unwrap()),
            )
            .await;
            assert_eq!(r.status(), StatusCode::CREATED);
        }
        // A container binds v1 by name; v2 is free.
        seed_container_binding_volume(&app, "user1", "v1").await;

        // Prune #1: only v2 reclaimed; v1 kept (in use).
        let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
        let pruned: std::collections::HashSet<&str> =
            rep.volumes_deleted.iter().map(|s| s.as_str()).collect();
        assert!(pruned.contains("v2"), "free v2 reclaimed: {pruned:?}");
        assert!(!pruned.contains("v1"), "in-use v1 kept: {pruned:?}");
        {
            let g = app.inner.lock().await;
            assert!(g.volumes.iter().any(|v| v.name == "v1"), "v1 survives prune #1");
            assert!(!g.volumes.iter().any(|v| v.name == "v2"), "v2 gone after prune #1");
        }

        // Drop the referencing container, then prune #2: v1 now reclaimed.
        app.inner.lock().await.containers.remove("user1");
        let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
        assert!(
            rep.volumes_deleted.iter().any(|s| s == "v1"),
            "v1 reclaimed once its last reference is gone: {:?}",
            rep.volumes_deleted
        );
        assert!(
            !app.inner.lock().await.volumes.iter().any(|v| v.name == "v1"),
            "v1 removed after prune #2"
        );
    }

    // ---- FLOW 15: restart-policy container, `stop` sets the DURABLE manual-stop flag -----------------
    // docker: stopping a container that has a `--restart` policy records a manual-stop so the supervisor
    // won't auto-restart it (Moby's HasBeenManuallyStopped). Engine-free: a "running" container with NO
    // live process — `do_stop` finds no pid, skips the signal/wait, and just flips the recorded state.
    #[tokio::test]
    async fn flow_restart_policy_stop_sets_manual_stop_flag() {
        let app = test_app();
        // Seed a RUNNING container with `--restart always` and NO live IO plumbing (so stop is engine-free).
        {
            let mut g = app.inner.lock().await;
            g.containers.insert(
                "rc".into(),
                Container {
                    id: "rc".into(),
                    image: "alpine".into(),
                    status: "running".into(),
                    started_at: 500,
                    restart_policy: crate::model::RestartPolicy {
                        name: "always".into(),
                        max_retry: 0,
                    },
                    ..Default::default()
                },
            );
        }
        // Pre-state: not manually stopped.
        assert!(!app.inner.lock().await.containers["rc"].manually_stopped);

        // stop -> 204, no live pid to signal, so it just records the stop.
        let r = crate::containers::containers_stop(
            State(app.clone()),
            Path("rc".into()),
            Query(empty_q()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        let g = app.inner.lock().await;
        let c = &g.containers["rc"];
        assert_eq!(c.status, "exited", "stop flips a running container to exited");
        assert!(
            c.manually_stopped,
            "stop sets the DURABLE manual-stop flag so `--restart always` won't auto-resurrect it"
        );
        assert_ne!(c.finished_at, 0, "finished_at recorded on stop");
        // The restart policy itself is preserved (a future explicit start still honors it).
        assert_eq!(c.restart_policy.name, "always", "restart policy is not cleared by stop");
    }

    // ==============================================================================================
    // MULTI-STEP INTEGRATION FLOWS — WAVE 3. Fresh handler SEQUENCES on branches the prior two waves
    // did not drive: `containers_prune`, network connect/disconnect IDEMPOTENCY, an exec whose
    // container stops out from under it, `volumes_prune` vs a still-referenced ANON volume, name reuse
    // after a REMOVED container, and a 3-tag rmi chain deleting the MIDDLE tag. Same rules: engine-free
    // handlers only; state asserted after EVERY step; docker's contract stated inline, then dd's actual
    // behavior asserted. A clean run is real signal; a divergence is flagged (concrete repro) not fixed.
    // ==============================================================================================

    /// Force a seeded container's lifecycle status (e.g. flip "running" -> "exited") in place, mirroring
    /// the engine reaper writing back the exit without going through a spawn.
    async fn set_status(app: &App, id: &str, status: &str) {
        let mut g = app.inner.lock().await;
        if let Some(c) = g.containers.get_mut(id) {
            c.status = status.to_string();
        }
    }

    // ---- FLOW 16: `container prune` removes ONLY the exited/created ones, keeps the running one -------
    // docker: `POST /containers/prune` (== `docker container prune`) removes every non-running,
    // non-paused container and returns their ids in `ContainersDeleted`; a running container is kept.
    // Drives seed(3) -> prune -> assert the deleted SET -> ps(all=1) reflects the removal.
    #[tokio::test]
    async fn flow_containers_prune_removes_only_stopped_keeps_running() {
        let app = test_app();
        seed_container(&app, "runid0000000", "running").await;
        seed_container(&app, "exitidA00000", "exited").await;
        seed_container(&app, "exitidB00000", "exited").await;

        // Prune: reclaims exactly the two exited ids; the running one is untouched.
        let axum::Json(rep) = crate::containers::containers_prune(State(app.clone())).await;
        let deleted: std::collections::HashSet<&str> =
            rep.containers_deleted.iter().map(|s| s.as_str()).collect();
        assert_eq!(deleted.len(), 2, "exactly two containers pruned: {deleted:?}");
        assert!(deleted.contains("exitidA00000"), "exited A reported deleted: {deleted:?}");
        assert!(deleted.contains("exitidB00000"), "exited B reported deleted: {deleted:?}");
        assert!(
            !deleted.contains("runid0000000"),
            "a RUNNING container must never be in the pruned set: {deleted:?}"
        );

        // In-memory state: only the running container remains.
        {
            let g = app.inner.lock().await;
            assert!(g.containers.contains_key("runid0000000"), "running container kept");
            assert!(!g.containers.contains_key("exitidA00000"), "exited A gone");
            assert!(!g.containers.contains_key("exitidB00000"), "exited B gone");
        }

        // ps (all=1) reflects the removal: a single row, the running container.
        let all_q: crate::containers::PsQ =
            serde_json::from_value(serde_json::json!({"all": "true"})).unwrap();
        let axum::Json(list) =
            crate::containers::containers_json(State(app.clone()), Query(all_q)).await;
        assert_eq!(list.len(), 1, "ps all=1 lists only the surviving running container");
        assert_eq!(list[0].id, "runid0000000");
    }

    // ---- FLOW 17: network connect IDEMPOTENCY + double disconnect --------------------------------
    // docker contract: a SECOND `network connect` of the same container is a 403/500 ("endpoint ...
    // already exists in network"); a `network disconnect` of a container that is NOT attached is a
    // 500 ("is not connected to network"). dd is LENIENT on both (join/leave are idempotent). The
    // SAFETY property under test is the refcount: a double-connect must NOT leak a second endpoint that
    // a single disconnect then cannot clear. Drives connect x2 -> disconnect -> disconnect(again).
    #[tokio::test]
    async fn flow_network_connect_idempotent_no_refcount_leak() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;
        let r = crate::networks::networks_create(State(app.clone()), net_create_body("mynet")).await;
        assert_eq!(r.status(), StatusCode::CREATED);

        // Step 1: connect c1 — 200, one endpoint.
        let r = crate::networks::network_connect(
            State(app.clone()),
            Path("mynet".into()),
            net_attach_body("c1"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(net_endpoint_count(&app, "mynet").await, 1, "first connect allocates one endpoint");
        assert_eq!(net_members(&app, "mynet").await, vec!["c1".to_string()]);

        // Step 2: connect c1 AGAIN. docker would 403/500; dd is idempotent (200). CRITICAL: the endpoint
        // count must STAY 1 — a leak here (count 2) would need two disconnects to clear (a refcount bug).
        let r = crate::networks::network_connect(
            State(app.clone()),
            Path("mynet".into()),
            net_attach_body("c1"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK, "dd re-connect is idempotent 200 (docker would 403/500)");
        assert_eq!(
            net_endpoint_count(&app, "mynet").await,
            1,
            "re-connect must NOT leak a second endpoint (idempotent join keyed by cid)"
        );
        assert_eq!(
            net_members(&app, "mynet").await,
            vec!["c1".to_string()],
            "re-connect must NOT duplicate the membership entry"
        );

        // Step 3: disconnect once — fully clears (endpoint AND membership reach zero in ONE call).
        let r = crate::networks::network_disconnect(
            State(app.clone()),
            Path("mynet".into()),
            net_attach_body("c1"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(
            net_endpoint_count(&app, "mynet").await,
            0,
            "a SINGLE disconnect fully clears the endpoint (no leaked refcount from the double-connect)"
        );
        assert_eq!(net_members(&app, "mynet").await.len(), 0, "membership cleared in one disconnect");

        // Step 4: disconnect AGAIN, already gone. docker: 500 "is not connected"; dd: idempotent 200,
        // state unchanged. Soft divergence (lenient) — flagged, not a state-corruption bug.
        let r = crate::networks::network_disconnect(
            State(app.clone()),
            Path("mynet".into()),
            net_attach_body("c1"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::OK, "dd double-disconnect is idempotent 200 (docker would 500)");
        assert_eq!(net_endpoint_count(&app, "mynet").await, 0, "still zero endpoints");

        // The now-empty net is deletable (proving the refcount truly reached zero).
        let r = crate::networks::network_delete(State(app.clone()), Path("mynet".into())).await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT, "empty net deletes (refcount reached zero)");
    }

    // ---- FLOW 18: exec whose container STOPS between create and inspect --------------------------
    // docker: an `exec create` on a running container returns an id; if the container then stops, an
    // `exec inspect` of that id still succeeds (Running=false); a FRESH `exec create` on the now-stopped
    // container is a 409 ("is not running"). Drives create -> stop-the-container -> inspect -> create(409).
    #[tokio::test]
    async fn flow_exec_survives_container_stop_then_fresh_exec_409() {
        let app = test_app();
        seed_container(&app, "c1", "running").await;

        // Step 1: exec_create while running -> 201 + id.
        let r = crate::containers::exec_create(
            State(app.clone()),
            Path("c1".into()),
            axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["sleep","1"]})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let exec_id = to_body_json(r).await["Id"].as_str().unwrap().to_string();

        // Step 2: the container stops (reaper writes back exited) out from under the recorded exec.
        set_status(&app, "c1", "exited").await;

        // Step 3: inspect the pre-existing exec id — still 200 with a sane shape (no panic/500). The exec
        // record outlives the container's running state; Running=false, ContainerID still points at c1.
        let r = crate::containers::exec_inspect(State(app.clone()), Path(exec_id.clone())).await;
        assert_eq!(r.status(), StatusCode::OK, "inspecting a pre-created exec survives the container stop");
        let v = to_body_json(r).await;
        assert_eq!(v["ID"], exec_id);
        assert_eq!(v["Running"], false, "the un-started exec is not Running");
        assert_eq!(v["ExitCode"], 0);
        assert_eq!(v["ContainerID"], "c1", "the exec still references its container");
        assert_eq!(v["ProcessConfig"]["entrypoint"], "sleep");

        // Step 4: a FRESH exec_create on the now-exited container is a 409 ("is not running"); no record.
        let before = app.inner.lock().await.execs.len();
        let r = crate::containers::exec_create(
            State(app.clone()),
            Path("c1".into()),
            axum::Json(serde_json::from_value(serde_json::json!({"Cmd":["ls"]})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CONFLICT, "exec on a stopped container is 409");
        let msg = to_body_string(r).await;
        assert!(msg.contains("is not running"), "409 body says not running: {msg}");
        assert_eq!(
            app.inner.lock().await.execs.len(),
            before,
            "a rejected exec must NOT record a second exec"
        );
    }

    // ---- FLOW 19: `volume prune` keeps a still-referenced ANON volume, reclaims it after `rm` -------
    // docker: `volume prune` (no filter) reclaims UNUSED volumes including dangling anonymous ones, but
    // NEVER one still referenced by a container — even a STOPPED one. Drives create(-v /data) -> stop ->
    // prune(kept) -> rm(no -v) -> prune(reclaimed). The anon vol survives while the container exists.
    #[tokio::test]
    async fn flow_volume_prune_keeps_referenced_anon_reclaims_after_rm() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

        // Create a container with a bare `-v /data` anonymous volume, then stop it (exited).
        let r = crate::containers::containers_create(
            State(app.clone()),
            Query(create_q(serde_json::json!({}))),
            create_body(serde_json::json!({
                "Image":"alpine",
                "HostConfig": {"Binds": ["/data"]}
            })),
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let cid = to_body_json(r).await["Id"].as_str().unwrap().to_string();
        let anon = {
            let g = app.inner.lock().await;
            let c = &g.containers[&cid];
            assert_eq!(c.anon_volumes.len(), 1, "a bare -v /data yields one anon volume");
            c.anon_volumes[0].clone()
        };
        set_status(&app, &cid, "exited").await;
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == anon),
            "the anon volume is registered in the store"
        );

        // Prune #1 while a (stopped) container still references the anon volume — it is KEPT.
        let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
        assert!(
            !rep.volumes_deleted.iter().any(|s| s == &anon),
            "a still-referenced anon volume must NOT be pruned (even for a stopped container): {:?}",
            rep.volumes_deleted
        );
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == anon),
            "the anon volume survives prune #1 while its container exists"
        );

        // Plain `rm` (no -v): the container goes, the anon volume is LEFT behind (now dangling/unused).
        let r = crate::containers::containers_delete(
            State(app.clone()),
            Path(cid.clone()),
            Query::<crate::containers::DeleteQ>(serde_json::from_value(serde_json::json!({})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT);
        assert!(
            app.inner.lock().await.volumes.iter().any(|v| v.name == anon),
            "plain rm (no -v) leaves the anon volume dangling — still present pre-prune"
        );

        // Prune #2: now nothing references it, so the dangling anon volume is reclaimed.
        let axum::Json(rep) = crate::volumes::volumes_prune(State(app.clone())).await;
        assert!(
            rep.volumes_deleted.iter().any(|s| s == &anon),
            "the now-unreferenced dangling anon volume is reclaimed by prune #2: {:?}",
            rep.volumes_deleted
        );
        assert!(
            !app.inner.lock().await.volumes.iter().any(|v| v.name == anon),
            "anon volume gone after prune #2"
        );
    }

    // ---- FLOW 20: a `--name` freed by `rm` can be REUSED (no stale name reservation) --------------
    // docker: `create --name web` then `rm web` FREES the name; a second `create --name web` is a 201
    // (the name is only reserved while the container exists). Drives create -> delete -> create(same name).
    #[tokio::test]
    async fn flow_container_name_reusable_after_remove() {
        let app = test_app();
        seed_image_rootfs(&app, "alpine", "/store/alpine-R").await;

        let create_web = |app: App| async move {
            crate::containers::containers_create(
                State(app),
                Query(create_q(serde_json::json!({"name":"web"}))),
                create_body(serde_json::json!({"Image":"alpine"})),
            )
            .await
        };

        // Step 1: first `--name web` -> 201.
        let r = create_web(app.clone()).await;
        assert_eq!(r.status(), StatusCode::CREATED, "first --name web creates");
        let cid1 = to_body_json(r).await["Id"].as_str().unwrap().to_string();
        assert_eq!(app.inner.lock().await.containers[&cid1].name, "web");

        // Step 2: rm web (created state -> engine-free delete) -> 204; the name is freed.
        let r = crate::containers::containers_delete(
            State(app.clone()),
            Path("web".into()),
            Query::<crate::containers::DeleteQ>(serde_json::from_value(serde_json::json!({})).unwrap()),
        )
        .await;
        assert_eq!(r.status(), StatusCode::NO_CONTENT, "rm web succeeds");
        assert_eq!(
            app.inner.lock().await.containers.values().filter(|c| c.name == "web").count(),
            0,
            "no container named web after the rm"
        );

        // Step 3: create `--name web` AGAIN -> 201 (the freed name is reusable), a NEW distinct id.
        let r = create_web(app.clone()).await;
        assert_eq!(
            r.status(),
            StatusCode::CREATED,
            "a --name freed by rm must be reusable (no stale 409 reservation)"
        );
        let cid2 = to_body_json(r).await["Id"].as_str().unwrap().to_string();
        assert_ne!(cid1, cid2, "the recreated container is a fresh distinct id");
        let g = app.inner.lock().await;
        assert_eq!(
            g.containers.values().filter(|c| c.name == "web").count(),
            1,
            "exactly one container named web (the recreated one)"
        );
        assert!(g.containers.contains_key(&cid2), "the recreated web is present");
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

    /// Collect a response body into a String (for error-message assertions).
    async fn to_body_string(resp: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Collect a response body and parse it as JSON (for wire-shape assertions on `Response`-typed
    /// handlers).
    async fn to_body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Build a query extractor with every (all-Option) field defaulted to None. The handler query
    /// structs (`KillQ`/`StopQ`/`PsQ`) don't derive `Default` and their fields are private, so we
    /// deserialize an empty object via their `Deserialize` impl (which maps missing Options to None).
    fn empty_q<T: serde::de::DeserializeOwned>() -> T {
        serde_json::from_value(serde_json::json!({})).unwrap()
    }
}
