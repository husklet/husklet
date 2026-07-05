//! `docker ps` — the container list, with `--all`/`--filter`/`--size` and docker's newest-first order.
use super::super::*;
use super::detail::container_mounts_json;

#[derive(Deserialize)]
pub(crate) struct PsQ {
    all: Option<String>,
    filters: Option<String>,
    size: Option<String>,
}

/// `docker ps --size` -> (SizeRw, SizeRootFs). dd gives each container a private copy-on-write UPPER over
/// the read-only image rootfs, so SizeRw is the `du`-style size of that writable upper layer (matching
/// docker, which measures the container's writable diff) and SizeRootFs is the full image rootfs walk.
/// The host-fs `macos` image (rootfs "/") is skipped -- walking it would be catastrophic, exactly as
/// `image_size` guards against.
fn container_sizes(c: &Container) -> (i64, i64) {
    if c.image == "macos" || c.rootfs.is_empty() || c.rootfs == "/" {
        return (0, 0);
    }
    let rw = if c.upper.is_empty() {
        0
    } else {
        dir_size(std::path::Path::new(&c.upper))
    };
    (rw, dir_size(std::path::Path::new(&c.rootfs)))
}

/// Apply `docker ps --filter`. `f` is the decoded `filters` map (`{"status":[..],"name":[..],"label":[..]}`).
/// Within a filter type the values are OR'd; `label` entries are AND'd (each must match). `name` is a
/// substring match against the container's effective name; `label` matches `key` or `key=value`.
/// `before_ts`/`since_ts` are the `created` timestamps of the containers named by `before=`/`since=`
/// (resolved by the caller, which holds the full container map); `None` => that key is absent/unresolved.
fn ps_match(
    c: &Container,
    name: &str,
    f: &HashMap<String, Vec<String>>,
    before_ts: Option<i64>,
    since_ts: Option<i64>,
) -> bool {
    if let Some(vals) = f.get("status") {
        if !vals.iter().any(|v| v == &c.status) {
            return false;
        }
    }
    if let Some(vals) = f.get("name") {
        if !vals.iter().any(|v| name.contains(v.as_str())) {
            return false;
        }
    }
    if let Some(vals) = f.get("label") {
        for v in vals {
            let ok = match v.split_once('=') {
                Some((k, val)) => c.labels.get(k).map(|cv| cv == val).unwrap_or(false),
                None => c.labels.contains_key(v),
            };
            if !ok {
                return false;
            }
        }
    }
    // `id=`: full-or-prefix match on the container id (docker accepts a leading prefix).
    if let Some(vals) = f.get("id") {
        if !vals.iter().any(|v| c.id.starts_with(v.as_str())) {
            return false;
        }
    }
    // `ancestor=`: the image the container was created from (repo[:tag] or a raw image ref).
    if let Some(vals) = f.get("ancestor") {
        // Repository-aware: `ancestor=nginx` must not also match a `linuxserver/nginx`
        // container just because the basenames coincide. Compare the fully qualified repository.
        if !vals
            .iter()
            .any(|v| c.image == *v || ref_repo(&c.image) == ref_repo(v))
        {
            return false;
        }
    }
    // `exited=N`: containers that exited with code N (only meaningful for the exited state).
    if let Some(vals) = f.get("exited") {
        if !vals.iter().any(|v| {
            v.parse::<i64>()
                .map_or(false, |n| c.status == "exited" && c.exit_code == n)
        }) {
            return false;
        }
    }
    // `health=`: dd models no healthcheck, so every container is effectively `none`; any other value
    // (starting/healthy/unhealthy) matches nothing.
    if let Some(vals) = f.get("health") {
        if !vals.iter().any(|v| v == "none") {
            return false;
        }
    }
    // `before=`/`since=`: created strictly before / after the referenced container (by create time).
    if let Some(ts) = before_ts {
        if c.created >= ts {
            return false;
        }
    }
    if let Some(ts) = since_ts {
        if c.created <= ts {
            return false;
        }
    }
    true
}

/// Render a container's `docker ps` Status column the way docker does: "Up 3 minutes" while
/// running/paused, "Exited (0) 5 minutes ago" otherwise. The elapsed time is measured from the
/// container's `created` unix timestamp and humanized coarsely (seconds/minutes/hours/days).
fn human_status(c: &Container) -> String {
    let secs = (now_secs() - c.created).max(0);
    let dur = if secs < 60 {
        format!("{secs} seconds")
    } else if secs < 3600 {
        format!("{} minutes", secs / 60)
    } else if secs < 86400 {
        format!("{} hours", secs / 3600)
    } else {
        format!("{} days", secs / 86400)
    };
    if c.status == "restarting" {
        // Docker shows a container in its restart-backoff window as "Restarting (code) …".
        format!("Restarting ({}) {dur} ago", c.exit_code)
    } else if c.status == "running" || c.status == "paused" {
        format!("Up {dur}")
    } else if c.status == "created" {
        // A created-but-never-started container shows a bare "Created" (no elapsed time), matching docker.
        "Created".to_string()
    } else {
        format!("Exited ({}) {dur} ago", c.exit_code)
    }
}

pub(crate) async fn containers_json(
    State(a): State<App>,
    Query(q): Query<PsQ>,
) -> Json<Vec<crate::api::ContainerSummary>> {
    let all = matches!(q.all.as_deref(), Some("1") | Some("true") | Some("True"));
    // `filters` arrives URL-encoded JSON; axum has already percent-decoded it. Bad JSON => no filters.
    // Docker encodes it as map[key]->{value:true} (e.g. {"name":{"web":true}}), older clients as
    // map[key]->[value]. Accept BOTH: decode to a generic Value, normalize to key -> [values].
    let filters: HashMap<String, Vec<String>> = q
        .filters
        .as_deref()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .and_then(|v| {
            v.as_object().map(|m| {
                m.iter()
                    .map(|(k, val)| {
                        let vals = match val {
                            Value::Object(set) => set.keys().cloned().collect(), // {"web":true}
                            Value::Array(a) => a
                                .iter()
                                .filter_map(|x| x.as_str().map(String::from))
                                .collect(), // ["web"]
                            _ => vec![],
                        };
                        (k.clone(), vals)
                    })
                    .collect()
            })
        })
        .unwrap_or_default();
    // A `status` filter implies "show all matching" (like `docker ps --filter status=exited`).
    let status_filter = filters.contains_key("status");
    // `--size`: docker only computes SizeRw/SizeRootFs on demand (a per-container rootfs walk is
    // expensive). Gather the matching containers, release the lock, THEN walk the disk so the daemon
    // lock isn't held across the (synchronous) `du`.
    let want_size = q_truthy(&q.size);
    let mut matched: Vec<Container> = {
        let g = a.inner.lock().await;
        // `before=`/`since=` name a reference container (by id-prefix or name); resolve each to that
        // container's `created` time so ps_match can compare create-order against it.
        let resolve = |key: &str| -> Option<i64> {
            filters
                .get(key)
                .and_then(|vals| vals.first())
                .and_then(|r| {
                    g.containers
                        .values()
                        .find(|c| c.id.starts_with(r.as_str()) || &c.name == r)
                        .map(|c| c.created)
                })
        };
        let before_ts = resolve("before");
        let since_ts = resolve("since");
        // A before/since (like status) filter implies "show all matching", not just running.
        let order_filter = filters.contains_key("before") || filters.contains_key("since");
        g.containers
            .values()
            .filter(|c| {
                all || status_filter
                    || order_filter
                    || c.status == "running"
                    || c.status == "restarting"
            })
            .filter(|c| {
                let name = if c.name.is_empty() {
                    c.id[..12.min(c.id.len())].to_string()
                } else {
                    c.name.clone()
                };
                ps_match(c, &name, &filters, before_ts, since_ts)
            })
            .cloned()
            .collect()
    };
    // Resolve named-volume mountpoints for the Mounts array below (the g lock is released after the block
    // above, so snapshot the volume set here).
    let vols_snapshot: Vec<Vol> = { a.inner.lock().await.volumes.clone() };
    // Docker lists containers newest-first (by creation time); our container map is unordered, so a raw
    // walk yields an arbitrary order and `docker ps`/`ps -q` would return IDs in an unpredictable order.
    // Sort descending by `created` (tie-break on `started_at`) to match docker's ordering.
    matched.sort_by(|a, b| {
        b.created
            .cmp(&a.created)
            .then(b.started_at.cmp(&a.started_at))
    });
    let v: Vec<crate::api::ContainerSummary> = matched
        .iter()
        .map(|c| {
            // Only emit the size keys when requested -- docker omits them otherwise.
            let (size_rw, size_root_fs) = if want_size {
                let (rw, rootfs) = container_sizes(c);
                (Some(rw), Some(rootfs))
            } else {
                (None, None)
            };
            crate::api::ContainerSummary {
                id: c.id.clone(),
                image: c.image.clone(),
                command: c.cmd.join(" "),
                created: c.created,
                state: c.status.clone(),
                status: human_status(c),
                exit_code: c.exit_code,
                ports: ports_json(&c.publish),
                labels: c.labels.clone(),
                mounts: container_mounts_json(&vols_snapshot, c),
                names: vec![format!(
                    "/{}",
                    if c.name.is_empty() {
                        c.id[..12.min(c.id.len())].to_string()
                    } else {
                        c.name.clone()
                    }
                )],
                size_rw,
                size_root_fs,
            }
        })
        .collect();
    Json(v)
}
