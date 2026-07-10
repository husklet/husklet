use crate::model::*;
use crate::util::*;
use crate::prelude::*;

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

/// A fresh, UNIQUE name for an anonymous `docker volume create` (empty `Name`). Docker mints a random
/// id per call; dd uses `vol_<12 hex>`. The seed MUST be unique per call: an earlier version seeded
/// `fake_id("v")` — a pure hash of a constant — so every unnamed create produced the SAME name, and the
/// second call returned the FIRST volume, silently sharing one backing dir across unrelated containers.
fn new_unnamed_volume_name() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("vol_{}", &fake_id(&format!("v{nanos}-{seq}"))[..12])
}

/// Outcome of a `DELETE /volumes/{name}` request, decided against daemon state. Existence is checked
/// BEFORE the in-use scan: a nonexistent volume is `NotFound` even if some container's bind string
/// happens to mention its name (previously that produced a spurious `409 volume is in use`).
#[derive(Debug, PartialEq)]
pub(crate) enum VolDeleteVerdict {
    NotFound,
    InUse,
    Remove(String), // mountpoint to unlink
}

pub(crate) fn volume_delete_verdict(g: &Inner, name: &str) -> VolDeleteVerdict {
    let Some(mp) = g
        .volumes
        .iter()
        .find(|v| v.name == name)
        .map(|v| v.mountpoint.clone())
    else {
        return VolDeleteVerdict::NotFound;
    };
    if volume_in_use(g, name, Some(&mp)) {
        return VolDeleteVerdict::InUse;
    }
    VolDeleteVerdict::Remove(mp)
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
        .unwrap_or_else(new_unnamed_volume_name);
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
    // Existence is checked BEFORE the in-use scan: a missing volume is 404 even if a container's bind
    // string mentions its name — otherwise cleanup tools see a spurious `409 volume is in use`.
    match volume_delete_verdict(&g, &name) {
        VolDeleteVerdict::NotFound => no_such_volume(&name),
        VolDeleteVerdict::InUse => conflict(format!("remove {name}: volume is in use")),
        VolDeleteVerdict::Remove(_) => {
            g.volumes.retain(|v| v.name != name);
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
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn inner_with(c: Container) -> Inner {
        let mut g = Inner::default();
        g.containers.insert(c.id.clone(), c);
        g
    }

    fn ctr() -> Container {
        Container {
            id: "c1".into(),
            ..Default::default()
        }
    }

    #[test]
    fn volume_in_use_via_bind_name() {
        // `-v myvol:/data` references by volume NAME.
        let mut c = ctr();
        c.binds = vec!["myvol:/data".into()];
        let g = inner_with(c);
        assert!(volume_in_use(&g, "myvol", Some("/mp/myvol")));
        assert!(!volume_in_use(&g, "othervol", Some("/mp/other")));
    }

    #[test]
    fn volume_in_use_via_bind_mountpoint() {
        // A bind whose source is the volume's MOUNTPOINT path also counts as in-use.
        let mut c = ctr();
        c.binds = vec!["/mp/myvol:/data".into()];
        let g = inner_with(c);
        assert!(volume_in_use(&g, "myvol", Some("/mp/myvol")));
        // Without the mountpoint hint the name doesn't match the path -> not in use.
        assert!(!volume_in_use(&g, "myvol", None));
    }

    #[test]
    fn volume_in_use_via_mount_type_volume() {
        // `--mount type=volume,source=myvol` (the §6.3-6 repair: previously missed).
        let mut c = ctr();
        c.mounts = vec![Mount {
            typ: "volume".into(),
            source: "myvol".into(),
            target: "/data".into(),
            read_only: false, bind_options: None }];
        let g = inner_with(c);
        assert!(volume_in_use(&g, "myvol", None));
        // A type=bind mount with the same source name must NOT count as a volume reference.
        let mut c2 = ctr();
        c2.mounts = vec![Mount {
            typ: "bind".into(),
            source: "myvol".into(),
            target: "/data".into(),
            read_only: false, bind_options: None }];
        assert!(!volume_in_use(&inner_with(c2), "myvol", None));
    }

    #[test]
    fn volume_in_use_via_anon_volume() {
        let mut c = ctr();
        c.anon_volumes = vec!["anon123".into()];
        let g = inner_with(c);
        assert!(volume_in_use(&g, "anon123", None));
    }

    #[test]
    fn volume_in_use_false_when_no_container_references_it() {
        assert!(!volume_in_use(&Inner::default(), "myvol", Some("/mp/myvol")));
    }

    #[test]
    fn vol_json_defaults_empty_driver_to_local() {
        let v = Vol {
            name: "n".into(),
            mountpoint: "/mp/n".into(),
            created_at: 0,
            driver: "".into(),
            options: Default::default(),
            labels: Default::default(),
        };
        let j = vol_json(&v);
        assert_eq!(j.driver, "local");
        assert_eq!(j.name, "n");
        assert_eq!(j.mountpoint, "/mp/n");
        assert_eq!(j.scope, "local");
        // created_at 0 renders as the epoch RFC3339 string.
        assert_eq!(j.created_at, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn vol_json_preserves_explicit_driver() {
        let v = Vol {
            name: "n".into(),
            mountpoint: "/mp/n".into(),
            created_at: 0,
            driver: "nfs".into(),
            options: Default::default(),
            labels: Default::default(),
        };
        assert_eq!(vol_json(&v).driver, "nfs");
    }

    fn vol(name: &str) -> Vol {
        Vol {
            name: name.into(),
            mountpoint: format!("/vol/{name}"),
            created_at: 0,
            driver: "local".into(),
            options: Default::default(),
            labels: Default::default(),
        }
    }

    // "Unnamed Volume Creation Reuses One Deterministic Name" (P1): each anonymous create must mint a
    // distinct name, else unrelated containers silently share one backing dir.
    #[test]
    fn unnamed_volume_create_allocates_unique_names() {
        let a = new_unnamed_volume_name();
        let b = new_unnamed_volume_name();
        assert_ne!(a, b, "two anonymous volume creates must not share a name");
        assert!(a.starts_with("vol_") && b.starts_with("vol_"));
    }

    // "Volume Delete Checks Binds Before Existence" (P2): a missing volume is NotFound even when a
    // container's bind string mentions its name — not a spurious InUse (409).
    #[test]
    fn volume_delete_missing_name_is_not_found_even_if_bind_mentions_it() {
        let mut c = ctr();
        c.binds = vec!["ghost:/data".into()];
        let g = inner_with(c);
        assert_eq!(
            volume_delete_verdict(&g, "ghost"),
            VolDeleteVerdict::NotFound,
            "deleting a nonexistent volume must be NotFound, not InUse"
        );
    }

    #[test]
    fn volume_delete_verdict_reports_in_use_and_removable() {
        let mut c = ctr();
        c.binds = vec!["data:/data".into()];
        let mut g = inner_with(c);
        g.volumes.push(vol("data"));
        g.volumes.push(vol("free"));
        assert_eq!(volume_delete_verdict(&g, "data"), VolDeleteVerdict::InUse);
        assert_eq!(
            volume_delete_verdict(&g, "free"),
            VolDeleteVerdict::Remove("/vol/free".into())
        );
    }
}
