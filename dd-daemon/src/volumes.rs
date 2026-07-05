#![allow(unused_imports, dead_code)]
use crate::archive::*;
use crate::build::*;
use crate::containers::*;
use crate::images::*;
use crate::model::*;
use crate::networks::*;
use crate::registry::{Client, Credentials, ImageRef};
use crate::runtime::*;
use crate::system::*;
use crate::util::*;
use crate::prelude::*;
use ddjit::{Guest, PortMap, SpawnConfig, Volume};

// ---- volumes ---------------------------------------------------------------

/// Whether any container currently references the volume named `name` (mountpoint `mp`). Covers BOTH
/// mount surfaces — `-v name:/dst` / bind-by-mountpoint (`c.binds`) AND `--mount type=volume,source=name`
/// (`c.mounts`) — so a volume wired via `--mount` (or an anonymous volume) is no longer prunable/removable
/// while a container uses it. Previously only `c.binds` was scanned, so a `--mount` volume looked unused
/// and could be reclaimed out from under a live container (§6.3-6).
pub(crate) fn volume_in_use(g: &Inner, name: &str, mp: Option<&str>) -> bool {
    g.containers.values().any(|c| {
        c.binds.iter().any(|b| {
            b.split(':')
                .next()
                .map_or(false, |src| src == name || mp == Some(src))
        }) || c
            .mounts
            .iter()
            .any(|m| m.typ == "volume" && m.source == name)
            || c.anon_volumes.iter().any(|a| a == name)
    })
}

pub(crate) fn vol_json(v: &Vol) -> crate::api::VolumeJson {
    let driver = if v.driver.is_empty() {
        "local".to_string()
    } else {
        v.driver.clone()
    };
    crate::api::VolumeJson {
        name: v.name.clone(),
        driver,
        mountpoint: v.mountpoint.clone(),
        created_at: fmt_rfc3339(v.created_at),
        scope: "local",
        labels: v.labels.clone(),
        options: v.options.clone(),
    }
}

pub(crate) async fn volumes_list(State(a): State<App>) -> Json<crate::api::VolumeList> {
    let g = a.inner.lock().await;
    Json(crate::api::VolumeList {
        volumes: g.volumes.iter().map(vol_json).collect::<Vec<_>>(),
        warnings: vec![],
    })
}

#[derive(Deserialize)]
pub(crate) struct VolumeCreateBody {
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Driver")]
    driver: Option<String>,
    #[serde(rename = "DriverOpts")]
    driver_opts: Option<HashMap<String, String>>,
    #[serde(rename = "Labels")]
    labels: Option<HashMap<String, String>>,
}

pub(crate) async fn volumes_create(
    State(a): State<App>,
    Json(body): Json<VolumeCreateBody>,
) -> Response {
    let name = body
        .name
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("vol_{}", fake_id("v")[..12].to_string()));
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
    {
        return bad_request("invalid volume name");
    }
    let driver = body
        .driver
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "local".into());
    let options = body.driver_opts.unwrap_or_default();
    let labels = body.labels.unwrap_or_default();
    let mountpoint = PathBuf::from(&a.volumes_dir).join(&name);
    let _ = std::fs::create_dir_all(&mountpoint);
    let mut g = a.inner.lock().await;
    let v = if let Some(existing) = g.volumes.iter().find(|v| v.name == name).cloned() {
        existing
    } else {
        let v = Vol {
            name: name.clone(),
            mountpoint: mountpoint.to_string_lossy().into_owned(),
            created_at: now_secs(),
            driver,
            options,
            labels,
        };
        g.volumes.push(v.clone());
        save_state(&g, &a.state_path);
        crate::events::emit_event(
            &a.events,
            "volume",
            "create",
            &name,
            json!({"driver": "local"}),
        );
        v
    };
    (StatusCode::CREATED, Json(vol_json(&v))).into_response()
}

pub(crate) async fn volume_inspect(State(a): State<App>, Path(name): Path<String>) -> Response {
    match a.inner.lock().await.volumes.iter().find(|v| v.name == name) {
        Some(v) => Json(vol_json(v)).into_response(),
        None => no_such_volume(&name),
    }
}

pub(crate) async fn volume_delete(State(a): State<App>, Path(name): Path<String>) -> Response {
    let mut g = a.inner.lock().await;
    let mountpoint = g
        .volumes
        .iter()
        .find(|v| v.name == name)
        .map(|v| v.mountpoint.clone());
    let in_use = volume_in_use(&g, &name, mountpoint.as_deref());
    if in_use {
        return (
            StatusCode::CONFLICT,
            Json(json!({"message": format!("remove {name}: volume is in use")})),
        )
            .into_response();
    }
    let before = g.volumes.len();
    g.volumes.retain(|v| v.name != name);
    if g.volumes.len() != before {
        let _ = std::fs::remove_dir_all(PathBuf::from(&a.volumes_dir).join(&name));
        save_state(&g, &a.state_path);
        crate::events::emit_event(
            &a.events,
            "volume",
            "destroy",
            &name,
            json!({"driver": "local"}),
        );
        StatusCode::NO_CONTENT.into_response()
    } else {
        no_such_volume(&name)
    }
}

/// `POST /volumes/prune` — `docker volume prune`. Removes volumes not referenced by any container's
/// binds and reports reclaimed names. (No space accounting yet.)
pub(crate) async fn volumes_prune(State(a): State<App>) -> Json<crate::api::VolumesPruneReport> {
    let mut g = a.inner.lock().await;
    // Prune every volume no container references — scanning BOTH `-v`/Binds AND `--mount`/anon volumes
    // (via `volume_in_use`), so an in-use `--mount type=volume` volume is no longer wrongly reclaimed.
    let pruned: Vec<String> = g
        .volumes
        .iter()
        .filter(|v| !volume_in_use(&g, &v.name, Some(&v.mountpoint)))
        .map(|v| v.name.clone())
        .collect();
    g.volumes.retain(|v| !pruned.contains(&v.name));
    for name in &pruned {
        let _ = std::fs::remove_dir_all(std::path::Path::new(&a.volumes_dir).join(name));
    }
    save_state(&g, &a.state_path);
    Json(crate::api::VolumesPruneReport {
        volumes_deleted: pruned,
        space_reclaimed: 0,
    })
}
