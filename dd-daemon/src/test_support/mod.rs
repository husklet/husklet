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
//!
//! The tests themselves live in per-domain submodules ([`containers`], [`networks`], [`volumes`],
//! [`images`], [`exec`], [`flows`]); each does `use super::*` to inherit this harness plus the shared
//! helpers below. Helpers used by only ONE domain live in that domain's file instead.
#![cfg(test)]

use crate::events::new_bus;
use crate::model::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

mod containers;
mod events;
mod exec;
mod flows;
mod images;
mod networks;
mod volumes;

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

/// Push an image with an explicit `rootfs` (the shared field `image_delete`'s refcount rule keys on).
async fn seed_image_rootfs(app: &App, name: &str, rootfs: &str) {
    let mut g = app.inner.lock().await;
    g.images.push(Image {
        name: name.to_string(),
        rootfs: rootfs.to_string(),
        ..Default::default()
    });
}

/// Force a seeded container's lifecycle status (e.g. flip "running" -> "exited") in place, mirroring
/// the engine reaper writing back the exit without going through a spawn.
async fn set_status(app: &App, id: &str, status: &str) {
    let mut g = app.inner.lock().await;
    if let Some(c) = g.containers.get_mut(id) {
        c.status = status.to_string();
    }
}

/// Build a `NetworkCreateBody` from `{"Name": name}`.
fn net_create_body(name: &str) -> axum::Json<crate::networks::NetCreateBody> {
    axum::Json(serde_json::from_value(serde_json::json!({ "Name": name })).unwrap())
}
/// Build a `NetAttachBody` (connect/disconnect) from `{"Container": cref}`.
fn net_attach_body(cref: &str) -> axum::Json<crate::networks::NetAttachBody> {
    axum::Json(serde_json::from_value(serde_json::json!({ "Container": cref })).unwrap())
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

/// Build a `CreateQ` from an object (e.g. `{"name":"web"}` or `{}`).
fn create_q(v: serde_json::Value) -> crate::containers::CreateQ {
    serde_json::from_value(v).unwrap()
}
/// Build a create-body `CreateBody` from a JSON object.
fn create_body(v: serde_json::Value) -> axum::Json<crate::containers::CreateBody> {
    axum::Json(serde_json::from_value(v).unwrap())
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
