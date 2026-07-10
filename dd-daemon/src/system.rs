use crate::images::*;
use crate::model::*;
use crate::util::*;
use crate::prelude::*;

/// The built daemon version, tracked from the crate version so `/version`, `/info` ServerVersion, and
/// the `Server` response header all report the same real identity (previously a stale hardcoded `0.1.0`).
pub(crate) const DD_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) async fn version() -> Json<crate::api::Version> {
    use crate::api::{Component, ComponentDetails, Platform, Version};
    Json(Version {
        version: DD_VERSION.into(),
        api_version: API_VERSION,
        min_api_version: "1.24",
        os: "linux",
        arch: "arm64",
        kernel_version: "6.1.0-dd",
        git_commit: "dd00000",
        go_version: "rustc",
        build_time: "2024-01-01T00:00:00Z",
        experimental: false,
        platform: Platform { name: "dd" },
        components: vec![Component {
            name: "Engine",
            version: DD_VERSION.into(),
            details: ComponentDetails {
                api_version: API_VERSION,
                os: "linux",
                arch: "arm64",
            },
        }],
    })
}

/// Logical CPUs usable by the daemon, for `/info` `NCPU`. Docker reports the host's usable CPU count;
/// dd hardcoded 1, so schedulers/clients under-sized workloads.
pub(crate) fn host_ncpu() -> i64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(1)
}

/// Total physical memory in bytes, for `/info` `MemTotal`. Reads Linux `/proc/meminfo`; `0` when it is
/// unavailable (e.g. non-Linux host) — the same sentinel as before, so no regression there.
pub(crate) fn host_mem_total() -> i64 {
    let Ok(s) = std::fs::read_to_string("/proc/meminfo") else {
        return 0;
    };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            if let Some(kb) = rest.split_whitespace().next().and_then(|v| v.parse::<i64>().ok()) {
                return kb.saturating_mul(1024);
            }
        }
    }
    0
}

/// dd's only container runtime. `/info` advertises it as `DefaultRuntime` AND lists it in `Runtimes`
/// (via [`runtimes`]) so the two stay consistent — Docker clients validate the default against that map.
pub(crate) const DEFAULT_RUNTIME: &str = "dd-jit";

/// The `Runtimes` map for `/info`. Always contains [`DEFAULT_RUNTIME`] so runtime validation/capability
/// discovery sees a well-formed shape (dd previously omitted `Runtimes` while advertising a default).
pub(crate) fn runtimes() -> std::collections::HashMap<&'static str, crate::api::Runtime> {
    std::collections::HashMap::from([(DEFAULT_RUNTIME, crate::api::Runtime { path: DEFAULT_RUNTIME })])
}

pub(crate) async fn info(State(a): State<App>) -> Json<crate::api::Info> {
    use crate::api::{Info, Plugins, Swarm};
    let g = a.inner.lock().await;
    let running = g
        .containers
        .values()
        .filter(|c| c.status == "running")
        .count();
    let paused = g
        .containers
        .values()
        .filter(|c| c.status == "paused")
        .count();
    let stopped = g.containers.len() - running - paused;
    Json(Info {
        id: "DD",
        name: "dd",
        containers: g.containers.len(),
        containers_running: running,
        containers_paused: paused,
        containers_stopped: stopped,
        images: g.images.len(),
        volumes: g.volumes.len(),
        networks: g.networks.len(),
        driver: "jit-overlay",
        operating_system: "dd (VM-less JIT on macOS)",
        os_type: "linux",
        architecture: "aarch64",
        ncpu: host_ncpu(),
        mem_total: host_mem_total(),
        kernel_version: "6.1.0-dd",
        server_version: DD_VERSION,
        docker_root_dir: dd_home().to_string_lossy().into_owned(),
        cgroup_driver: "none",
        default_runtime: DEFAULT_RUNTIME,
        runtimes: runtimes(),
        swarm: Swarm {
            local_node_state: "inactive",
            control_available: false,
        },
        plugins: Plugins {
            volume: vec!["local"],
            network: vec!["bridge", "host", "none"],
            authorization: None,
            log: vec![],
        },
        security_options: vec![],
        warnings: vec![],
    })
}

/// `POST /auth` — `docker login`. dd has no central auth store; accept any credentials so the CLI
/// records them locally (pull/push then send them via `X-Registry-Auth`). Empty body = a probe.
pub(crate) async fn auth(body: axum::body::Bytes) -> Response {
    let _ = body;
    (
        StatusCode::OK,
        Json(crate::api::AuthResponse {
            status: "Login Succeeded",
            identity_token: "",
        }),
    )
        .into_response()
}

/// `GET /system/df` — `docker system df`. Reports the rough disk usage of images, containers and
/// volumes. dd has no build cache and no per-container/volume size accounting yet, so `BuildCache`
/// is empty, `BuilderSize` is 0 and the rw/volume sizes take Docker's "not calculated" sentinels.
pub(crate) async fn system_df(State(a): State<App>) -> Json<crate::api::DiskUsage> {
    use crate::api::{ContainerDf, DiskUsage, ImageDf, Usage, VolumeDf, VolumeUsageData};
    let g = a.inner.lock().await;
    let images: Vec<ImageDf> = g
        .images
        .iter()
        .map(|i| {
            let size = image_size(&i.rootfs, &i.name);
            // Containers backed by THIS exact image tag — Docker's `system df` counts by the precise
            // `repository:tag`, so a container from `repo/app:v1` must not be attributed to sibling `:v2`.
            let containers = g
                .containers
                .values()
                .filter(|c| {
                    ref_repo(&c.image) == ref_repo(&i.name) && ref_tag(&c.image) == ref_tag(&i.name)
                })
                .count();
            ImageDf {
                id: image_id(i),
                parent_id: "",
                repo_tags: vec![repo_tag(&i.name)],
                created: 0,
                size,
                shared_size: 0,
                virtual_size: size,
                containers,
            }
        })
        .collect();
    let layers: i64 = images.iter().map(|i| i.size).sum();
    let containers: Vec<ContainerDf> = g
        .containers
        .values()
        .map(|c| ContainerDf {
            id: c.id.clone(),
            image: c.image.clone(),
            command: "",
            created: c.created,
            size_rw: 0,
            size_root_fs: 0,
            state: c.status.clone(),
            status: c.status.clone(),
            names: vec![format!(
                "/{}",
                if c.name.is_empty() {
                    c.id[..12.min(c.id.len())].to_string()
                } else {
                    c.name.clone()
                }
            )],
        })
        .collect();
    let volumes: Vec<VolumeDf> = g
        .volumes
        .iter()
        .map(|v| {
            // RefCount = number of containers referencing this volume (by name/bind/mount/anon) — a
            // mounted volume must not read back as unused. Docker reports the live reference count here.
            let ref_count = g
                .containers
                .values()
                .filter(|c| crate::volumes::container_uses_volume(c, &v.name, Some(&v.mountpoint)))
                .count() as i64;
            VolumeDf {
                name: v.name.clone(),
                driver: "local",
                mountpoint: v.mountpoint.clone(),
                usage_data: VolumeUsageData {
                    size: -1,
                    ref_count,
                },
            }
        })
        .collect();
    // Volumes with at least one live container reference (for VolumeUsage.ActiveCount).
    let volumes_active = volumes
        .iter()
        .filter(|v| v.usage_data.ref_count > 0)
        .count() as i64;
    // Images actively used by at least one container (ImageUsage.ActiveCount describes IMAGES, not the
    // container count — two containers on one image is one active image).
    let images_active = images.iter().filter(|i| i.containers > 0).count() as i64;
    // Emit BOTH shapes: the classic flat lists (older clients) AND the newer nested *Usage objects
    // current clients (docker CLI / bollard) read — so `docker system df` and the GUI both populate.
    let running = g
        .containers
        .values()
        .filter(|c| c.status == "running")
        .count() as i64;
    let (nimg, nctr, nvol) = (
        images.len() as i64,
        containers.len() as i64,
        volumes.len() as i64,
    );
    // Persistent JIT translated-code cache (~/.dd/pcache, one <binid>.pcache per guest binary). It's the
    // closest analogue to Docker's build cache, so we surface it in that slot: shown by `system df`,
    // reclaimed by `system prune` / `builder prune` (see build_prune). Fully reclaimable (rebuilds on demand).
    // Materialize one build-cache ITEM per pcache file so the reported TotalCount always matches the
    // item list — a nonzero count with an empty Items list is internally contradictory (docker parity).
    let pc_items: Vec<Value> = std::fs::read_dir(crate::util::dd_home().join("pcache"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let m = e.metadata().ok()?;
                    if !m.is_file() {
                        return None;
                    }
                    let id = e.file_name().to_string_lossy().into_owned();
                    Some(json!({
                        "ID": id,
                        "Type": "regular",
                        "Size": m.len() as i64,
                        "InUse": false,
                        "Shared": false,
                        "CreatedAt": "0001-01-01T00:00:00Z",
                        "LastUsedAt": null,
                        "UsageCount": 0,
                        "Parent": "",
                        "Description": "dd JIT translated-code cache",
                    }))
                })
                .collect()
        })
        .unwrap_or_default();
    let pc_size: i64 = pc_items
        .iter()
        .map(|i| i["Size"].as_i64().unwrap_or(0))
        .sum();
    let pc_count = pc_items.len() as i64;
    Json(DiskUsage {
        layers_size: layers,
        image_usage: Usage {
            active_count: images_active,
            total_count: nimg,
            reclaimable: 0,
            total_size: layers,
            items: images.clone(),
        },
        container_usage: Usage {
            active_count: running,
            total_count: nctr,
            reclaimable: 0,
            total_size: 0,
            items: containers.clone(),
        },
        volume_usage: Usage {
            active_count: volumes_active,
            total_count: nvol,
            reclaimable: 0,
            total_size: 0,
            items: volumes.clone(),
        },
        build_cache_usage: Usage {
            active_count: 0,
            total_count: pc_count,
            reclaimable: pc_size,
            total_size: pc_size,
            items: pc_items.clone(),
        },
        images,
        containers,
        volumes,
        build_cache: pc_items,
        builder_size: pc_size,
    })
}

/// `GET /plugins` — `docker plugin ls`. dd ships no managed plugins, but `/info` advertises plugin
/// categories, so a compatible client may query the inventory. Return an empty list (200) rather than a
/// 404 fallback, which strict clients treat as a daemon error.
pub(crate) async fn plugins_list() -> Json<Vec<Value>> {
    Json(vec![])
}

/// `POST /system/prune` — `docker system prune`. Runs the individual prune passes (stopped containers,
/// unused user networks, unreferenced volumes, dangling images) and returns the combined report docker
/// clients expect. Previously unrouted, so compatible clients hit a 404 fallback.
pub(crate) async fn system_prune(State(a): State<App>) -> Json<Value> {
    let containers = crate::containers::containers_prune(State(a.clone())).await.0;
    let networks = crate::networks::networks_prune(State(a.clone())).await.0;
    let volumes = crate::volumes::volumes_prune(State(a.clone())).await.0;
    let images = crate::images::images_prune(State(a.clone())).await.0;
    Json(json!({
        "ContainersDeleted": containers.containers_deleted,
        "NetworksDeleted": networks.networks_deleted,
        "VolumesDeleted": volumes.volumes_deleted,
        "ImagesDeleted": images.images_deleted,
        "SpaceReclaimed": 0,
    }))
}

// `GET /events` — `docker events`. The handler now lives in `crate::events` (the lifecycle bus):
// see `events.rs` for the broadcast-backed, newline-delimited JSON stream and `emit_event`.

#[cfg(test)]
mod capacity_tests {
    use super::{host_mem_total, host_ncpu};

    // "Info Under-Reports Daemon Capacity" (P1): /info must report the real usable CPU count (>=1),
    // not a hardcoded 1, and on Linux a nonzero MemTotal derived from /proc/meminfo.
    #[test]
    fn host_ncpu_is_at_least_one() {
        assert!(host_ncpu() >= 1);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn host_mem_total_is_nonzero_on_linux() {
        assert!(host_mem_total() > 0, "MemTotal must be read from /proc/meminfo on Linux");
    }

    // "Info Default Runtime Is Not Declared" (P1): /info advertises a DefaultRuntime, so the Runtimes map
    // must declare it — otherwise runtime validation sees a broken shape.
    #[test]
    fn default_runtime_is_present_in_runtimes_map() {
        assert!(
            super::runtimes().contains_key(super::DEFAULT_RUNTIME),
            "the declared DefaultRuntime must appear in the Runtimes map"
        );
    }
}
