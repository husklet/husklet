//! The typed, env-free launch path: marshal a [`LaunchConfig`] into the `ddjit_config` wire buffer
//! (see `src/runtime/include/ddjit_api.h`) and hand it to the C `ddjit_spawn`, which posix_spawns the
//! arch-matching engine as `<engine> --configfd <fd>` and returns the container's pid. No `bash`, no
//! `DD_*`/`DDJIT_*` environment — the container config travels as a struct.

use crate::Guest;
use std::ffi::CString;
use std::os::raw::c_char;

const DDJIT_CONFIG_MAGIC: u32 = 0x4443_4647; // 'DCFG'

// Mirrors `struct ddjit_config` in ddjit_api.h EXACTLY (field order + types) so `#[repr(C)]` produces
// the same 84-byte header the engine reads. Every `*_off` is a byte offset into the string pool that
// trails this header; 0 = unset (pool[0] is a lone NUL, so 0 reads as "").
#[repr(C)]
struct WireHeader {
    magic: u32,
    pool_len: u32,
    mem_max: u64,
    pids_max: u32,
    cpus: u32,
    uid: i32,
    gid: i32,
    rootfs_ro: u32,
    sandbox: u32,
    rootfs_off: u32,
    lowers_off: u32,
    hostname_off: u32,
    netns_off: u32,
    publish_off: u32,
    volumes_off: u32,
    ulimits_off: u32,
    cwd_off: u32,
    guest_env_off: u32,
    pcache_off: u32,
    argv_off: u32,
    reserved: u32,
}

extern "C" {
    /// C spawn shim (os/darwin/ffi.c). Returns the child pid, or -1 (errno set) on failure.
    fn ddjit_spawn(engine_path: *const c_char, config: *const u8, config_len: usize) -> i32;
}

/// Everything needed to launch one container, as typed Rust — the caller (dd-jit) builds this from its
/// `Container`; there is no environment dialect. Empty/`None` fields are simply omitted from the wire.
#[derive(Clone, Debug, Default)]
pub struct LaunchConfig {
    pub rootfs: String,
    /// read-only overlay lowers, highest-priority first
    pub lowers: Vec<String>,
    pub hostname: String,
    pub mem_max: u64,
    pub pids_max: u32,
    pub cpus: u32,
    pub rootfs_ro: bool,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub sandbox: bool,
    /// (host_port, container_port) tcp publishes
    pub publish: Vec<(u16, u16)>,
    /// (guest_path, host_dir, read_only)
    pub volumes: Vec<(String, String, bool)>,
    /// (name, soft, hard)
    pub ulimits: Vec<(String, u64, u64)>,
    /// private-loopback key (not the /tmp path); empty = shared
    pub netns: String,
    pub cwd: String,
    /// guest environment as `K=V` lines (forwarded verbatim to the guest, never the host env)
    pub guest_env: Vec<String>,
    /// persistent translated-code cache dir; empty = disabled
    pub pcache_dir: String,
    /// the guest argv (entrypoint + args)
    pub argv: Vec<String>,
}

/// Builds the string pool (offset 0 is always a lone NUL so a 0 offset reads as the empty string).
struct Pool(Vec<u8>);
impl Pool {
    fn new() -> Self {
        Pool(vec![0])
    }
    /// Append a NUL-terminated string; returns its offset (0 for an empty string — shares pool[0]).
    fn add(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        let off = self.0.len() as u32;
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
        off
    }
    /// Append raw bytes verbatim (for the double-NUL-terminated argv); returns its offset.
    fn add_bytes(&mut self, b: &[u8]) -> u32 {
        let off = self.0.len() as u32;
        self.0.extend_from_slice(b);
        off
    }
}

impl LaunchConfig {
    /// Serialize into the `ddjit_config` wire buffer (`<header><string pool>`).
    fn to_wire(&self) -> Vec<u8> {
        let mut pool = Pool::new();
        let rootfs_off = pool.add(&self.rootfs);
        let lowers_off = pool.add(&self.lowers.join(":"));
        let hostname_off = pool.add(&self.hostname);
        let netns_off = pool.add(&self.netns);
        let publish_off = pool.add(
            &self
                .publish
                .iter()
                .map(|(h, c)| format!("{h}:{c}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        let volumes_off = pool.add(
            &self
                .volumes
                .iter()
                .map(|(g, h, ro)| if *ro { format!("ro:{g}:{h}") } else { format!("{g}:{h}") })
                .collect::<Vec<_>>()
                .join(","),
        );
        let ulimits_off = pool.add(
            &self
                .ulimits
                .iter()
                .map(|(n, s, h)| format!("{n}={s}:{h}"))
                .collect::<Vec<_>>()
                .join(","),
        );
        let cwd_off = pool.add(&self.cwd);
        let guest_env_off = pool.add(&self.guest_env.join("\n"));
        let pcache_off = pool.add(&self.pcache_dir);
        // argv: NUL-separated, double-NUL terminated.
        let argv_off = {
            let mut a = Vec::new();
            for arg in &self.argv {
                a.extend_from_slice(arg.as_bytes());
                a.push(0);
            }
            a.push(0);
            pool.add_bytes(&a)
        };

        let header = WireHeader {
            magic: DDJIT_CONFIG_MAGIC,
            pool_len: pool.0.len() as u32,
            mem_max: self.mem_max,
            pids_max: self.pids_max,
            cpus: self.cpus,
            uid: self.uid.map(|u| u as i32).unwrap_or(-1),
            gid: self.gid.map(|g| g as i32).unwrap_or(-1),
            rootfs_ro: self.rootfs_ro as u32,
            sandbox: self.sandbox as u32,
            rootfs_off,
            lowers_off,
            hostname_off,
            netns_off,
            publish_off,
            volumes_off,
            ulimits_off,
            cwd_off,
            guest_env_off,
            pcache_off,
            argv_off,
            reserved: 0,
        };
        let hbytes = unsafe {
            std::slice::from_raw_parts(
                &header as *const WireHeader as *const u8,
                std::mem::size_of::<WireHeader>(),
            )
        };
        let mut buf = Vec::with_capacity(hbytes.len() + pool.0.len());
        buf.extend_from_slice(hbytes);
        buf.extend_from_slice(&pool.0);
        buf
    }
}

/// Launch a container in the matching guest's engine. Returns the container pid, or an error if the
/// engine for `guest` isn't built or the spawn failed.
pub fn spawn(guest: Guest, cfg: &LaunchConfig) -> std::io::Result<u32> {
    let engine = guest
        .jit_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no engine built for guest"))?;
    let engine_c = CString::new(engine).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let wire = cfg.to_wire();
    let pid = unsafe { ddjit_spawn(engine_c.as_ptr(), wire.as_ptr(), wire.len()) };
    if pid <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(pid as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_is_stable() {
        // Must match `sizeof(struct ddjit_config)` in ddjit_api.h: 21 u32 payload + 1 u64 + explicit
        // `reserved` pad = 88 bytes, 8-aligned, no implicit padding (both sides use sizeof).
        assert_eq!(std::mem::size_of::<WireHeader>(), 88);
        assert_eq!(std::mem::align_of::<WireHeader>(), 8);
    }

    #[test]
    fn wire_roundtrip_shape() {
        let cfg = LaunchConfig {
            rootfs: "/img".into(),
            lowers: vec!["/a".into(), "/b".into()],
            hostname: "web".into(),
            mem_max: 512 * 1024 * 1024,
            cpus: 2,
            uid: Some(0),
            gid: Some(0),
            publish: vec![(8080, 80)],
            volumes: vec![("/data".into(), "/host".into(), false)],
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            ..Default::default()
        };
        let wire = cfg.to_wire();
        // header + at least the pooled strings
        assert!(wire.len() > std::mem::size_of::<WireHeader>());
        // magic at offset 0
        assert_eq!(&wire[0..4], &DDJIT_CONFIG_MAGIC.to_ne_bytes());
        // the rootfs string is present in the pool
        assert!(wire.windows(4).any(|w| w == b"/img"));
    }
}
