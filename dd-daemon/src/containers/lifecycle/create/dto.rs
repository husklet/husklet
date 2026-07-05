//! Create-body / host-config DTOs for `POST /containers/create`: the deserialize
//! structs (`CreateBody`, `NetworkingConfig`, `HostConfig`, `PortBinding`, `CreateQ`)
//! decoded from the request body/query. Pure data shapes — no state threading.
//! Fields are `pub(super)` so the `create` handler (parent) and the `ports` sibling
//! read them exactly as they did when this lived in one file; behavior unchanged.
use super::super::super::*;

#[derive(Deserialize)]
pub(crate) struct CreateBody {
    #[serde(rename = "Image")]
    pub(super) image: Option<String>,
    #[serde(rename = "Cmd")]
    pub(super) cmd: Option<Vec<String>>,
    #[serde(rename = "Env")]
    pub(super) env: Option<Vec<String>>,
    #[serde(rename = "Entrypoint")]
    pub(super) entrypoint: Option<Vec<String>>,
    #[serde(rename = "Hostname")]
    pub(super) hostname: Option<String>,
    #[serde(rename = "Tty")]
    pub(super) tty: Option<bool>,
    #[serde(rename = "WorkingDir")]
    pub(super) working_dir: Option<String>,
    #[serde(rename = "Labels")]
    pub(super) labels: Option<HashMap<String, String>>,
    // `docker run --user U[:G]` — docker puts the "uid:gid" / "name" string in Config.User (top-level
    // of the create body, alongside Image/Cmd/Env). Stored on the Container and turned into DD_UID/DD_GID.
    #[serde(rename = "User")]
    pub(super) user: Option<String>,
    #[serde(rename = "HostConfig")]
    pub(super) host_config: Option<HostConfig>,
    // `docker create`/compose attach a container to one or more user-defined networks via the
    // top-level NetworkingConfig.EndpointsConfig (a map keyed by network name). HostConfig.NetworkMode
    // names the *primary* network; EndpointsConfig enumerates ALL of them (compose puts every
    // `networks:` entry of a service here). We join each so a multi-network service lands on them all.
    #[serde(rename = "NetworkingConfig")]
    pub(super) networking_config: Option<NetworkingConfig>,
    // Config-level lifecycle fields (top of the create body, NOT under HostConfig): `--stop-signal`,
    // `--stop-timeout`, and `--health-*` (Healthcheck). Each overrides the image's; absent ⇒ inherit.
    #[serde(rename = "StopSignal")]
    pub(super) stop_signal: Option<String>,
    #[serde(rename = "StopTimeout")]
    pub(super) stop_timeout: Option<i64>,
    #[serde(rename = "Healthcheck")]
    pub(super) healthcheck: Option<crate::model::HealthConfig>,
    // `Config.Volumes` — the docker CLI puts a bare `-v /path` (anonymous volume) HERE (a set of dirs),
    // the same channel as an image `VOLUME`. Each uncovered dir gets a fresh anonymous volume at run.
    #[serde(rename = "Volumes")]
    pub(super) volumes: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
pub(crate) struct NetworkingConfig {
    #[serde(rename = "EndpointsConfig")]
    pub(super) endpoints_config: Option<HashMap<String, Value>>,
}

#[derive(Deserialize)]
pub(crate) struct HostConfig {
    #[serde(rename = "Binds")]
    pub(super) binds: Option<Vec<String>>,
    #[serde(rename = "Memory")]
    pub(super) memory: Option<i64>,
    #[serde(rename = "PidsLimit")]
    pub(super) pids_limit: Option<i64>,
    // Resource fidelity: `--cpus` (NanoCpus), `--read-only` (ReadonlyRootfs), `--ulimit` (Ulimits).
    #[serde(rename = "NanoCpus")]
    pub(super) nano_cpus: Option<i64>,
    #[serde(rename = "ReadonlyRootfs")]
    pub(super) readonly_rootfs: Option<bool>,
    #[serde(rename = "Ulimits")]
    pub(super) ulimits: Option<Vec<crate::model::Ulimit>>,
    #[serde(rename = "PortBindings")]
    pub(super) port_bindings: Option<HashMap<String, Vec<PortBinding>>>,
    #[serde(rename = "NetworkMode")]
    pub(super) network_mode: Option<String>,
    // HostConfig fidelity extras (parsed + persisted; round-tripped back through inspect).
    #[serde(rename = "RestartPolicy")]
    pub(super) restart_policy: Option<RestartPolicy>,
    #[serde(rename = "CapAdd")]
    pub(super) cap_add: Option<Vec<String>>,
    #[serde(rename = "CapDrop")]
    pub(super) cap_drop: Option<Vec<String>>,
    #[serde(rename = "Devices")]
    pub(super) devices: Option<Vec<DeviceMapping>>,
    #[serde(rename = "Mounts")]
    pub(super) mounts: Option<Vec<Mount>>,
    // `--tmpfs DST[:opts]` (HostConfig.Tmpfs): a map of container-path -> mount options ("size=64m,mode=1777").
    // A fresh empty in-memory-equivalent mount. `--mount type=tmpfs` arrives via Mounts instead (folded in).
    #[serde(rename = "Tmpfs")]
    pub(super) tmpfs: Option<HashMap<String, String>>,
    #[serde(rename = "Privileged")]
    pub(super) privileged: Option<bool>,
    // `--security-opt` (Vec<String> like ["sandbox"], ["seccomp=untrusted"], ["no-new-privileges"]).
    // Parsed + persisted verbatim; an entry matching sandbox/untrusted opts into the JIT sentry (spawn_cfg).
    #[serde(rename = "SecurityOpt")]
    pub(super) security_opt: Option<Vec<String>>,
    // `--rm` (HostConfig.AutoRemove): the daemon removes the container automatically once it exits.
    #[serde(rename = "AutoRemove")]
    pub(super) auto_remove: Option<bool>,
}

#[derive(Deserialize, Clone)]
pub(crate) struct PortBinding {
    #[serde(rename = "HostPort")]
    pub(super) host_port: Option<String>,
    // `docker -p 127.0.0.1:8080:80` sets HostIp so the publish is loopback-only (NOT world-reachable).
    // Empty/absent ⇒ 0.0.0.0. Previously dropped — the reason `127.0.0.1` publishes leaked to every iface.
    #[serde(rename = "HostIp")]
    pub(super) host_ip: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct CreateQ {
    pub(super) name: Option<String>,
    pub(super) platform: Option<String>,
}
