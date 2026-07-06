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
