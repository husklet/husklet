use crate::images::*;
use crate::model::*;
use crate::util::*;
use crate::prelude::*;

pub(crate) async fn version() -> Json<crate::api::Version> {
    use crate::api::{Component, ComponentDetails, Platform, Version};
    Json(Version {
        version: "0.1.0-dd".into(),
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
            version: "0.1.0-dd".into(),
            details: ComponentDetails {
                api_version: API_VERSION,
                os: "linux",
                arch: "arm64",
            },
        }],
    })
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
        ncpu: 1,
        mem_total: 0,
        kernel_version: "6.1.0-dd",
        server_version: "0.1.0-dd",
        docker_root_dir: dd_home().to_string_lossy().into_owned(),
        cgroup_driver: "none",
        default_runtime: "dd-jit",
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
            // Containers backed by this image (by fully qualified repository) — Docker's
            // `system df` shows this count; a bare `nginx` must not absorb `linuxserver/nginx`'s containers.
            let containers = g
                .containers
                .values()
                .filter(|c| ref_repo(&c.image) == ref_repo(&i.name))
                .count();
            ImageDf {
                id: format!("sha256:{}", fake_id(&i.name)),
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
        .map(|v| VolumeDf {
            name: v.name.clone(),
            driver: "local",
            mountpoint: v.mountpoint.clone(),
            usage_data: VolumeUsageData {
                size: -1,
                ref_count: -1,
            },
        })
        .collect();
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
    let (pc_size, pc_count) = std::fs::read_dir(crate::util::dd_home().join("pcache"))
        .map(|rd| {
            rd.filter_map(|e| e.ok().and_then(|e| e.metadata().ok()))
                .filter(|m| m.is_file())
                .fold((0i64, 0i64), |(s, c), m| (s + m.len() as i64, c + 1))
        })
        .unwrap_or((0, 0));
    Json(DiskUsage {
        layers_size: layers,
        image_usage: Usage {
            active_count: nctr,
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
            active_count: 0,
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
            items: vec![],
        },
        images,
        containers,
        volumes,
        build_cache: vec![],
        builder_size: pc_size,
    })
}

// `GET /events` — `docker events`. The handler now lives in `crate::events` (the lifecycle bus):
// see `events.rs` for the broadcast-backed, newline-delimited JSON stream and `emit_event`.
