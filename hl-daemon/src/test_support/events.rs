//! `GET /events` tests: the `--until` bounded-stream termination + filter parsing.
use super::*;
use axum::extract::{Path, Query, State};

/// Subscribe to the bus and drain every event currently queued into a Vec of (type, action) pairs.
/// A subscriber must exist BEFORE the handler runs (emit_event drops events with no receivers).
fn drain_events(rx: &mut tokio::sync::broadcast::Receiver<serde_json::Value>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push((
            ev["Type"].as_str().unwrap_or("").to_string(),
            ev["Action"].as_str().unwrap_or("").to_string(),
        ));
    }
    out
}

/// Build an `EventsQ` from a JSON object (its fields are all `Option<String>`).
fn events_q(v: serde_json::Value) -> crate::events::EventsQ {
    serde_json::from_value(v).unwrap()
}

async fn body_bytes(resp: axum::response::Response) -> Vec<u8> {
    axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

// ---- docker events --until <past> terminates immediately (does NOT hang) --------------------
// Regression: `--until`/`--since` were deserialized but never applied, so `docker events --until
// <past-ts>` (a BOUNDED command) streamed forever. hl keeps no event history, so a past `--until`
// must close the stream at once rather than block the client.
#[tokio::test]
async fn events_until_in_the_past_closes_immediately() {
    let app = test_app();
    // A far-past bound: the stream must be empty and complete (not hang).
    let resp = crate::events::events(State(app.clone()), Query(events_q(serde_json::json!({"until":"1"})))).await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    // to_bytes resolving at all proves the body ended; it must carry no events.
    let body = body_bytes(resp).await;
    assert!(body.is_empty(), "a past --until must yield an empty, closed stream, got {body:?}");
}

// ---- prune/lifecycle destroy events + network endpoint cleanup ------------------------------
#[tokio::test]
async fn container_prune_emits_destroy_and_clears_network_endpoint() {
    let app = test_app();
    seed_container(&app, "gone00000000", "exited").await;
    // Put the exited container on a user network so prune must also clear its endpoint.
    {
        let mut g = app.inner.lock().await;
        g.networks.push(Net {
            id: "net-frontend".into(),
            name: "frontend".into(),
            driver: "bridge".into(),
            scope: "local".into(),
            containers: vec!["gone00000000".into()],
            created: 0,
            subnet: "172.18.0.0/16".into(),
            gateway: "172.18.0.1".into(),
            endpoints: std::collections::HashMap::from([(
                "gone00000000".to_string(),
                Endpoint { name: "gone".into(), ip: "172.18.0.2".into(), aliases: vec![] },
            )]),
        });
    }
    let mut rx = app.events.subscribe();
    let _ = crate::containers::containers_prune(State(app.clone())).await;
    let evs = drain_events(&mut rx);
    assert!(evs.contains(&("container".into(), "destroy".into())), "prune must emit container/destroy: {evs:?}");
    let g = app.inner.lock().await;
    let net = g.networks.iter().find(|n| n.name == "frontend").unwrap();
    assert!(net.endpoints.is_empty(), "prune must clear the pruned container's endpoint");
    assert!(net.containers.is_empty(), "prune must clear membership");
}

#[tokio::test]
async fn network_prune_emits_destroy_events() {
    let app = test_app();
    seed_network(&app, "idle", false).await;
    let mut rx = app.events.subscribe();
    let _ = crate::networks::networks_prune(State(app.clone())).await;
    let evs = drain_events(&mut rx);
    assert!(evs.contains(&("network".into(), "destroy".into())), "{evs:?}");
}

#[tokio::test]
async fn volume_prune_emits_destroy_events() {
    let app = test_app();
    seed_volume(&app, "scratch", false).await;
    let mut rx = app.events.subscribe();
    let _ = crate::volumes::volumes_prune(State(app.clone())).await;
    let evs = drain_events(&mut rx);
    assert!(evs.contains(&("volume".into(), "destroy".into())), "{evs:?}");
}

#[tokio::test]
async fn network_connect_disconnect_emit_events() {
    let app = test_app();
    seed_container(&app, "cnet00000000", "running").await;
    seed_network(&app, "frontend", false).await;
    let mut rx = app.events.subscribe();
    let connect = crate::networks::network_connect(
        State(app.clone()),
        Path("frontend".into()),
        net_attach_body("cnet00000000"),
    )
    .await;
    assert_eq!(connect.status(), axum::http::StatusCode::OK);
    let disconnect = crate::networks::network_disconnect(
        State(app.clone()),
        Path("frontend".into()),
        net_attach_body("cnet00000000"),
    )
    .await;
    assert_eq!(disconnect.status(), axum::http::StatusCode::OK);
    let evs = drain_events(&mut rx);
    assert!(evs.contains(&("network".into(), "connect".into())), "{evs:?}");
    assert!(evs.contains(&("network".into(), "disconnect".into())), "{evs:?}");
}

#[tokio::test]
async fn container_rename_emits_rename_event() {
    let app = test_app();
    seed_container(&app, "ren000000000", "created").await;
    let mut rx = app.events.subscribe();
    let q: crate::containers::RenameQ =
        serde_json::from_value(serde_json::json!({"name":"newname"})).unwrap();
    let r = crate::containers::containers_rename(State(app.clone()), Path("ren000000000".into()), Query(q)).await;
    assert_eq!(r.status(), axum::http::StatusCode::NO_CONTENT);
    let evs = drain_events(&mut rx);
    assert!(evs.contains(&("container".into(), "rename".into())), "{evs:?}");
}

// ---- image prune, system prune, plugins, system df, version ---------------------------------
#[tokio::test]
async fn image_prune_removes_dangling_unreferenced_image() {
    let app = test_app();
    {
        let mut g = app.inner.lock().await;
        // A dangling (untagged) image, and a tagged one that must survive.
        g.images.push(Image { name: String::new(), rootfs: "/store/dangle".into(), ..Default::default() });
        g.images.push(Image { name: "keep:latest".into(), rootfs: "/store/keep".into(), ..Default::default() });
    }
    let report = crate::images::images_prune(State(app.clone())).await.0;
    assert!(!report.images_deleted.is_empty(), "dangling image should be reported deleted");
    let g = app.inner.lock().await;
    assert!(!g.images.iter().any(|i| i.rootfs == "/store/dangle"), "dangling image removed");
    assert!(g.images.iter().any(|i| i.name == "keep:latest"), "tagged image kept");
}

#[tokio::test]
async fn plugins_endpoint_returns_empty_list() {
    let list = crate::system::plugins_list().await.0;
    assert!(list.is_empty(), "plugins list is an empty array, not 404");
}

#[tokio::test]
async fn version_tracks_crate_version() {
    let v = crate::system::version().await.0;
    assert_eq!(v.version, env!("CARGO_PKG_VERSION"));
    assert_ne!(v.version, "0.1.0-hl", "version must not be the stale hardcoded value");
}

#[tokio::test]
async fn system_df_counts_are_consistent() {
    let app = test_app();
    {
        let mut g = app.inner.lock().await;
        g.images.push(Image { name: "app:v1".into(), rootfs: "/store/app".into(), ..Default::default() });
        // Two running containers on the SAME image -> ActiveCount(image) == 1, not 2.
        for id in ["c1aaaaaaaaaa", "c2aaaaaaaaaa"] {
            g.containers.insert(id.into(), Container {
                id: id.into(), image: "app:v1".into(), rootfs: "/store/app".into(),
                status: "running".into(), ..Default::default()
            });
        }
        // A volume mounted by one container -> RefCount 1, ActiveCount 1.
        g.volumes.push(Vol { name: "data".into(), mountpoint: "/mp/data".into(), created_at: 0,
            driver: "local".into(), options: Default::default(), labels: Default::default() });
        g.containers.get_mut("c1aaaaaaaaaa").unwrap().binds = vec!["data:/data".into()];
    }
    let df = crate::system::system_df(State(app.clone())).await.0;
    let df = serde_json::to_value(&df).unwrap();
    assert_eq!(df["ImageUsage"]["ActiveCount"], 1, "one active image, not the container count");
    assert_eq!(df["VolumeUsage"]["ActiveCount"], 1, "the mounted volume is active");
    assert_eq!(df["Volumes"][0]["UsageData"]["RefCount"], 1, "mounted volume RefCount is live");
    // Build-cache TotalCount must match the Items length (no phantom count).
    let bc = &df["BuildCacheUsage"];
    assert_eq!(bc["TotalCount"].as_i64().unwrap(), bc["Items"].as_array().unwrap().len() as i64);
}
