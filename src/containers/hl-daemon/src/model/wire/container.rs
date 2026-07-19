use super::*;

#[derive(Deserialize, Default)]
pub(crate) struct ProcessCreateBody {
    #[serde(rename = "Cmd")]
    pub(crate) cmd: Option<Vec<String>>,
    #[serde(rename = "Tty")]
    pub(crate) tty: Option<bool>,
    #[serde(rename = "Env")]
    pub(crate) env: Option<Vec<String>>,
    #[serde(rename = "WorkingDir")]
    pub(crate) working_dir: Option<String>,
    #[serde(rename = "User")]
    pub(crate) user: Option<String>,
}

/// `--restart` policy (HostConfig.RestartPolicy). `name` is one of "" / "no" / "always" /
/// "unless-stopped" / "on-failure"; `max_retry` caps `on-failure` restarts. Serde-renamed so it both
/// deserializes from the create body and round-trips verbatim back through inspect HostConfig.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct RestartPolicy {
    #[serde(rename = "Name", default)]
    pub(crate) name: String,
    #[serde(rename = "MaximumRetryCount", default)]
    pub(crate) max_retry: i64,
}

/// `--device` mapping (HostConfig.Devices[]). Metadata only — the JIT does not enforce device cgroups —
/// stored and reported verbatim in inspect.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct DeviceMapping {
    #[serde(rename = "PathOnHost", default)]
    pub(crate) path_on_host: String,
    #[serde(rename = "PathInContainer", default)]
    pub(crate) path_in_container: String,
    #[serde(rename = "CgroupPermissions", default)]
    pub(crate) cgroup_permissions: String,
}

/// `--ulimit` entry (HostConfig.Ulimits[]). `name` is docker's resource name (nofile/nproc/stack/...),
/// `soft`/`hard` the limits. Reflected in the container's getrlimit via the engine's HL_ULIMITS contract.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Ulimit {
    #[serde(rename = "Name", default)]
    pub(crate) name: String,
    #[serde(rename = "Soft", default)]
    pub(crate) soft: i64,
    #[serde(rename = "Hard", default)]
    pub(crate) hard: i64,
}

/// Serialize `Container.arch` (an `Option<Guest>`) as the guest's stable `target()` string (e.g.
/// `"linux_x86_64"`) in `state.json`, so the arch survives a daemon restart even when the backing image
/// is gone. `Guest` itself has no serde derive, so this bridges it without touching the hljit crate.
pub(crate) struct ArchOption;

impl ArchOption {
    pub(crate) fn serialize<S: serde::Serializer>(
        g: &Option<Guest>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match g {
            Some(g) => s.serialize_some(g.target()),
            None => s.serialize_none(),
        }
    }

    pub(crate) fn deserialize<'de, D: serde::Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Guest>, D::Error> {
        let opt = <Option<String> as serde::Deserialize>::deserialize(d)?;
        Ok(opt.and_then(|t| Guest::ALL.into_iter().find(|g| g.target() == t)))
    }
}

/// `HostConfig.LogConfig` (`--log-driver`/`--log-opt`). Metadata only — hl has a single built-in log
/// sink — but accepted at create and round-tripped verbatim through inspect so clients diff it cleanly.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct LogConfig {
    #[serde(rename = "Type", default)]
    pub(crate) typ: String,
    #[serde(rename = "Config", default)]
    pub(crate) config: std::collections::HashMap<String, String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Container {
    pub(crate) id: String,
    pub(crate) image: String,
    pub(crate) rootfs: String,
    // Per-container copy-on-write UPPER layer: a private writable dir overlaid on the read-only image
    // `rootfs` (the lower). The guest's writes/creates land here and deletions become whiteouts, so a
    // container never mutates the shared image. Allocated under `<hl_home>/containers/<id>/upper` at
    // create (linux guests only; darwin uses the native jail) and reclaimed on `docker rm`/prune. Empty
    // for darwin containers and for state predating overlay, in which case the flat `rootfs` is used.
    #[serde(default)]
    pub(crate) upper: String,
    pub(crate) cmd: Vec<String>, // the resolved LAUNCH argv (entrypoint ++ cmd) handed to the engine
    // Docker inspect reports Config.Entrypoint and Config.Cmd SPLIT, not collapsed into one argv. These
    // hold the resolved parts (entrypoint = user `--entrypoint` else image ENTRYPOINT; cmd_config = the
    // cmd part only) purely for inspect/commit fidelity; the engine still launches the merged `cmd` above.
    #[serde(default)]
    pub(crate) entrypoint: Vec<String>,
    #[serde(default)]
    pub(crate) cmd_config: Vec<String>,
    pub(crate) binds: Vec<String>,
    pub(crate) hostname: String,
    // `Config.Domainname` (`--domainname`): the container's DNS domain (UTS). Metadata + inspect fidelity.
    #[serde(default)]
    pub(crate) domainname: String,
    pub(crate) memory: i64,
    pub(crate) pids_limit: i64,
    // `--cpus` (HostConfig.NanoCpus, billionths of a CPU): the container's CPU allotment. Threaded to the
    // engine as HL_CPUS=ceil(NanoCpus/1e9) so nproc / GOMAXPROCS / the JVM self-size to the allotment.
    #[serde(default)]
    pub(crate) nano_cpus: i64,
    // `--read-only` (HostConfig.ReadonlyRootfs): the rootfs is EROFS (writes to /proc /dev /sys /tmp /run
    // stay allowed). Threaded to the engine as HL_ROOTFS_RO=1.
    #[serde(default)]
    pub(crate) readonly_rootfs: bool,
    // `--ulimit` (HostConfig.Ulimits): (name, soft, hard) triples reflected in the container's getrlimit.
    #[serde(default)]
    pub(crate) ulimits: Vec<Ulimit>,
    pub(crate) publish: String,
    pub(crate) created: i64,
    pub(crate) status: String,
    pub(crate) exit_code: i64,
    #[serde(default)]
    pub(crate) tty: bool,
    // `Config.OpenStdin` (`-i`) / `Config.StdinOnce`: interactive-stdio flags. Metadata + inspect fidelity
    // so attach/exec tooling can reconstruct whether the container keeps stdin open.
    #[serde(default)]
    pub(crate) open_stdin: bool,
    #[serde(default)]
    pub(crate) stdin_once: bool,
    // `Config.ExposedPorts` (image EXPOSE + create-body ExposedPorts): "port/proto" keys the container
    // declares as listening. Distinct from published (host-bound) ports; inspect reports them as null
    // bindings in NetworkSettings.Ports and under Config.ExposedPorts.
    #[serde(default)]
    pub(crate) exposed_ports: Vec<String>,
    // `HostConfig.LogConfig`, DNS settings, and DeviceRequests — accepted at create, round-tripped through
    // inspect. DeviceRequests is stored opaquely (its NVIDIA-style shape varies) and echoed verbatim.
    #[serde(default)]
    pub(crate) log_config: Option<LogConfig>,
    #[serde(default)]
    pub(crate) dns: Vec<String>,
    #[serde(default)]
    pub(crate) dns_search: Vec<String>,
    #[serde(default)]
    pub(crate) dns_options: Vec<String>,
    #[serde(default)]
    pub(crate) extra_hosts: Vec<String>, // "host:ip" entries for /etc/hosts + inspect
    #[serde(default)]
    pub(crate) device_requests: Vec<serde_json::Value>,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) working_dir: String,
    #[serde(default)]
    pub(crate) env: Vec<String>, // "K=V" entries from the image ENV + `docker run -e`
    #[serde(default)]
    pub(crate) labels: std::collections::HashMap<String, String>, // `docker run --label`
    #[serde(default)]
    pub(crate) network_mode: String,
    #[serde(default)]
    pub(crate) user: String, // `docker run --user` / `docker exec -u` -> HL_UID/HL_GID
    #[serde(default)]
    pub(crate) started_at: i64, // unix secs, set on start (ps sort / logs since-until / human status)
    #[serde(default)]
    pub(crate) finished_at: i64, // unix secs, set on stop/natural exit
    // Nanosecond-precision copies for the inspect State.StartedAt/FinishedAt strings — docker reports
    // these to the nanosecond, so a quick `docker restart` (same wall-clock second) still advances StartedAt.
    #[serde(default)]
    pub(crate) started_at_ns: i64,
    #[serde(default)]
    pub(crate) finished_at_ns: i64,
    // ---- HostConfig fidelity (docker run extras) ----
    #[serde(default)]
    pub(crate) restart_policy: RestartPolicy, // `--restart` (supervisor restarts on exit per policy)
    #[serde(default)]
    pub(crate) restart_count: i64, // restarts performed so far (caps `on-failure:N`; inspect RestartCount)
    #[serde(default)]
    pub(crate) cap_add: Vec<String>, // `--cap-add` — metadata (JIT doesn't enforce Linux capabilities)
    #[serde(default)]
    pub(crate) cap_drop: Vec<String>, // `--cap-drop` — metadata
    #[serde(default)]
    pub(crate) devices: Vec<DeviceMapping>, // `--device` — metadata
    #[serde(default)]
    pub(crate) mounts: Vec<Mount>, // `--mount` (wired into the rootfs alongside `-v`/Binds)
    #[serde(default)]
    pub(crate) privileged: bool, // `--privileged` — metadata; relaxes daemon-side guards (none currently)
    #[serde(default)]
    pub(crate) security_opt: Vec<String>, // `--security-opt` (e.g. "sandbox"/"seccomp=untrusted"); an entry
    // matching sandbox/untrusted opts the container into the JIT's untrusted-guest sentry (see spawn_cfg).
    #[serde(default)]
    pub(crate) auto_remove: bool, // `--rm` (HostConfig.AutoRemove): the daemon removes the container on exit.
    // ---- Lifecycle/volume fidelity (Moby §6/§8) ----
    // `--stop-signal` / image `Config.StopSignal`: the signal `docker stop` sends before the SIGKILL grace
    // (nginx SIGQUIT, postgres SIGINT fast-shutdown). Empty ⇒ SIGTERM. Resolved at create (create-body
    // override else the image's), so `docker stop` reads it here without touching the image.
    #[serde(default)]
    pub(crate) stop_signal: String,
    // `--stop-timeout` / `Config.StopTimeout` (seconds): the SIGKILL grace after the stop signal. 0 ⇒ the
    // daemon default (10s). A `docker stop -t N` still overrides per-call.
    #[serde(default)]
    pub(crate) stop_timeout: i64,
    // `--tmpfs DST[:opts]` / `--mount type=tmpfs`: an in-memory-equivalent mount — a FRESH, empty writable
    // dir cleared on every container start (never persisted). Keyed by container target path -> raw options
    // (size=/mode=, kept for inspect fidelity). Backed by a per-container host dir path-spliced like a bind.
    #[serde(default)]
    pub(crate) tmpfs: std::collections::HashMap<String, String>,
    // Names of ANONYMOUS volumes this container created (bare `-v /path` + image `VOLUME` dirs). Removed on
    // `docker rm -v` and eligible for `volume prune`; NAMED volumes are never auto-removed. Mirrors Moby's
    // "remove only anonymous volumes on rm" (mounts.go:removeMountPoints).
    #[serde(default)]
    pub(crate) anon_volumes: Vec<String>,
    // Durable manual-stop flag — the persisted equivalent of Moby's `HasBeenManuallyStopped`. Set true by
    // stop/kill/rm, cleared on an explicit start/restart. Survives a daemon restart so `unless-stopped`
    // does NOT resurrect a container the user deliberately stopped (the in-memory `Live.stop_requested` is
    // lost across a daemon restart; this is not).
    #[serde(default)]
    pub(crate) manually_stopped: bool,
    // Resolved HEALTHCHECK config (create-body `--health-*` override else the image's), and the live probe
    // state surfaced as inspect `State.Health`. `health` is None until the first probe runs.
    #[serde(default)]
    pub(crate) healthcheck: Option<HealthConfig>,
    #[serde(default)]
    pub(crate) health: Option<HealthState>,
    // The guest architecture. Re-derived from the image at load when the image is present, but ALSO
    // persisted (as the target string) so a container whose image has since disappeared keeps its correct
    // arch across a daemon restart instead of defaulting to arm64 (which would run x86 code on the wrong
    // engine). See `load_state`: the persisted value is the fallback when no image resolves.
    #[serde(
        default,
        serialize_with = "ArchOption::serialize",
        deserialize_with = "ArchOption::deserialize"
    )]
    pub(crate) arch: Option<Guest>,
    // `docker exec` only: overrides the id-derived HL_NETNS loopback key so the exec'd process SHARES the
    // TARGET container's 127.0.0.1 address space instead of getting its own isolated loopback. Set to the
    // parent container's id in exec_start; None for a normal container (which keys off its own id).
    // Runtime-only (lives on the throwaway exec temp), never persisted.
    #[serde(skip)]
    pub(crate) netns_key: Option<String>,
    // Captured output is not persisted (would bloat the state file).
    #[serde(skip)]
    pub(crate) stdout: Vec<u8>,
    #[serde(skip)]
    pub(crate) stderr: Vec<u8>,
}

/// A `docker exec` invocation: a command to run in a container's rootfs. hl runs it as a fresh JIT
/// process sharing the container's rootfs + volumes (the same files; a distinct process namespace).
#[derive(Clone)]
pub(crate) struct Exec {
    pub(crate) container_id: String,
    pub(crate) cmd: Vec<String>,
    pub(crate) tty: bool,
    pub(crate) started: bool,
    /// `docker exec -e` overrides (added on top of the container env), `-w` working dir, `-u` user.
    pub(crate) env: Vec<String>,
    pub(crate) working_dir: String,
    pub(crate) user: String,
    /// `docker exec --privileged` — metadata only (the JIT doesn't enforce Linux caps); accepted
    /// without error and surfaced in exec inspect, mirroring the container-level `privileged`.
    pub(crate) privileged: bool,
    /// The exec process's exit code, recorded by the reaper when it exits. The reaper then drops the
    /// exec's `Live` (its log buffers/channels) to avoid a leak, but keeps THIS record so a post-exit
    /// `docker exec inspect` still reports the real code (the Live is gone, so inspect reads it here).
    pub(crate) exit_code: i64,
}
