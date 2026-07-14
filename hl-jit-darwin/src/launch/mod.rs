//! The typed, env-free launch path: marshal a [`LaunchConfig`] into the `hl_config` wire buffer
//! (see `src/runtime/include/hl_api.h`) and hand it to the C `hl_spawn`, which posix_spawns the
//! arch-matching engine as `<engine> --configfd <fd>` and returns the container's pid. No `bash`, no
//! `HL_*`/`HL_JIT_*` environment — the container config travels as a struct.
//!
//! Split by concern: this module holds the typed [`LaunchConfig`]; `wire.rs` is the byte-exact
//! `hl_config` encoder ([`LaunchConfig::to_wire`] + the `WireHeader` C-contract layout); `spawn.rs`
//! is the fork/exec entry ([`spawn`] / [`spawn_io`] / [`SpawnIo`]).

mod spawn;
mod wire;

pub use spawn::{spawn, spawn_io, SpawnIo};

/// Everything needed to launch one container, as typed Rust — the caller (hl-jit) builds this from its
/// `Container`; there is no environment dialect. Empty/`None` fields are simply omitted from the wire.
#[derive(Clone, Debug, Default)]
pub struct LaunchConfig {
    /// The writable rootfs (the overlay UPPER, or a plain single rootfs). Empty = run un-jailed.
    pub rootfs: String,
    /// read-only overlay lowers, highest-priority first
    pub lowers: Vec<String>,
    /// UTS hostname (empty = inherit the host's).
    pub hostname: String,
    /// cgroup `memory.max` in bytes (0 = unlimited).
    pub mem_max: u64,
    /// cgroup `pids.max` (0 = unlimited).
    pub pids_max: u32,
    /// docker `--cpus`: online-CPU count the container advertises (0 = unlimited).
    pub cpus: u32,
    /// docker `--read-only`: writes to the rootfs/overlay-upper fail EROFS.
    pub rootfs_ro: bool,
    /// USER-ns uid the guest process runs as (`None` = root = 0).
    pub uid: Option<u32>,
    /// USER-ns gid the guest process runs as (`None` = root = 0).
    pub gid: Option<u32>,
    /// Run the guest under the JIT's syscall sandbox (`HL_JIT_SANDBOX`).
    pub sandbox: bool,
    /// `--network none`: refuse all non-loopback egress.
    pub net_isolate: bool,
    /// An external host forwarder owns published ports; the engine must not start its own listener.
    pub publish_daemon: bool,
    /// user-network virtual-switch id (empty = not on a user network)
    pub netbr: String,
    /// this container's IP on that switch (empty = none)
    pub ip: String,
    /// shared external-writer generation file for daemon-write coherence (empty = none)
    pub fsgen_file: String,
    /// per-workspace VPN egress: funnel the guest's genuine external TCP connects through this SOCKS5 proxy
    /// (`host:port`, the engine's `HL_EGRESS_SOCKS` switch). Empty = direct host egress (unchanged).
    pub egress_socks: String,
    /// (host_port, container_port) tcp publishes
    pub publish: Vec<(u16, u16)>,
    /// (guest_path, host_dir, read_only)
    pub volumes: Vec<(String, String, bool)>,
    /// (name, soft, hard)
    pub ulimits: Vec<(String, u64, u64)>,
    /// private-loopback key (not the /tmp path); empty = shared
    pub netns: String,
    /// Guest working directory (empty = the rootfs root, `/`).
    pub cwd: String,
    /// guest environment as `K=V` lines (forwarded verbatim to the guest, never the host env)
    pub guest_env: Vec<String>,
    /// persistent translated-code cache dir; empty = disabled
    pub pcache_dir: String,
    /// opt-in the host-IOSurface GPU path (`--gui`): the engine synthesizes `/dev/dri/renderD128` + services
    /// `HL_IOCTL_GPU_ALLOC`. Off = the whole GPU path is inert (existing workloads/the gate never see it).
    pub gpu_iosurface: bool,
    /// per-container persistent-cache kill switch (`HL_JIT_NOPCACHE`): disables pcache for THIS container even
    /// when the runtime enables pcache defaults globally. Off = pcache follows the global gate.
    pub nopcache: bool,
    /// the guest argv (entrypoint + args)
    pub argv: Vec<String>,
}
