//! The byte-exact `ddjit_config` wire encoder: [`LaunchConfig::to_wire`] plus the `WireHeader` C-contract
//! layout + string-pool packing the C engine (`ddjit_configfd.c`) reads. A byte change here mislaunches
//! every container, so this is moved verbatim and locked by the offset/layout tests below.

use super::LaunchConfig;

pub(super) const DDJIT_CONFIG_MAGIC: u32 = 0x4443_4647; // 'DCFG'

// Mirrors `struct ddjit_config` in ddjit_api.h EXACTLY (field order + types) so `#[repr(C)]` produces
// the same 112-byte header the engine reads. Every `*_off` is a byte offset into the string pool that
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
    net_isolate: u32,
    publish_daemon: u32,
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
    netbr_off: u32,
    ip_off: u32,
    fsgen_off: u32,
    argv_off: u32,
    gpu_iosurface: u32,
    nopcache: u32,   // bool: per-container DDJIT_NOPCACHE kill switch (was reserved tail pad)
    egress_off: u32, // per-workspace VPN egress SOCKS5 endpoint (DD_EGRESS_SOCKS); "" = direct
    reserved0: u32,  // explicit tail pad (keeps the struct 8-aligned, no implicit padding); future use
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
    pub(super) fn to_wire(&self) -> Vec<u8> {
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
        let netbr_off = pool.add(&self.netbr);
        let ip_off = pool.add(&self.ip);
        let fsgen_off = pool.add(&self.fsgen_file);
        let egress_off = pool.add(&self.egress_socks);
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
            net_isolate: self.net_isolate as u32,
            publish_daemon: self.publish_daemon as u32,
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
            netbr_off,
            ip_off,
            fsgen_off,
            argv_off,
            gpu_iosurface: self.gpu_iosurface as u32,
            nopcache: self.nopcache as u32,
            egress_off,
            reserved0: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_layout_is_stable() {
        // Must match `sizeof(struct ddjit_config)` in ddjit_api.h: 10 u32 scalars + 15 offsets
        // (…fsgen_off, egress_off) + gpu_iosurface/nopcache bools + 1 u32 explicit `reserved0` pad =
        // 28 u32 + 1 u64 (mem_max) = 120 bytes, 8-aligned, no implicit padding (both sides use sizeof).
        // Grown from 112: egress_off carries the VPN egress SOCKS endpoint (DD_EGRESS_SOCKS); the paired
        // reserved0 keeps the struct 8-aligned. Rust `to_wire` and the C `ddjit_configfd.c` reader compile
        // into the SAME dd-jit-darwin artifact, so this is an internal contract with no cross-binary skew.
        assert_eq!(std::mem::size_of::<WireHeader>(), 120);
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

    #[test]
    fn wire_gpu_iosurface_flag_roundtrips() {
        // The --gui GPU opt-in is a scalar bool in the tail-pad slot; off by default, 1 when armed.
        let off = LaunchConfig { rootfs: "/img".into(), ..Default::default() }.to_wire();
        let h0: WireHeader = unsafe { std::ptr::read_unaligned(off.as_ptr() as *const WireHeader) };
        assert_eq!(h0.gpu_iosurface, 0);
        let on = LaunchConfig { rootfs: "/img".into(), gpu_iosurface: true, ..Default::default() }.to_wire();
        let h1: WireHeader = unsafe { std::ptr::read_unaligned(on.as_ptr() as *const WireHeader) };
        assert_eq!(h1.gpu_iosurface, 1);
    }

    #[test]
    fn wire_pool_offsets_point_at_expected_strings() {
        // Lock the exact byte layout `ddjit_configfd.c` decodes: every `*_off` is a pool-relative
        // offset to a NUL-terminated string (0 == empty), and argv is NUL-separated + double-NUL
        // terminated at `argv_off`.
        let cfg = LaunchConfig {
            rootfs: "/mnt/root".into(),
            lowers: vec!["/lo/a".into(), "/lo/b".into()],
            hostname: "dbhost".into(),
            netns: "nskey".into(),
            cwd: "/work".into(),
            pcache_dir: "/pc".into(),
            netbr: "br7".into(),
            ip: "10.1.2.3".into(),
            fsgen_file: "/run/fsgen".into(),
            egress_socks: "127.30.0.1:1080".into(),
            publish: vec![(8080, 80), (5432, 5432)],
            volumes: vec![("/data".into(), "/hostdata".into(), true), ("/cfg".into(), "/hcfg".into(), false)],
            ulimits: vec![("nofile".into(), 1024, 4096)],
            guest_env: vec!["A=1".into(), "B=2".into()],
            argv: vec!["/bin/sh".into(), "-c".into(), "echo hi".into()],
            ..Default::default()
        };
        let wire = cfg.to_wire();

        let hsize = std::mem::size_of::<WireHeader>();
        // The header prefix is exactly the WireHeader bytes; read it back unaligned (Vec<u8> data is
        // not guaranteed 8-aligned).
        let hdr: WireHeader = unsafe { std::ptr::read_unaligned(wire.as_ptr() as *const WireHeader) };
        assert_eq!(hdr.magic, DDJIT_CONFIG_MAGIC);
        assert_eq!(hdr.pool_len as usize, wire.len() - hsize);

        let pool = &wire[hsize..];
        // Read the NUL-terminated C string living at pool offset `off`.
        let read_str = |off: u32| -> String {
            let start = off as usize;
            let end = start + pool[start..].iter().position(|&b| b == 0).unwrap();
            String::from_utf8(pool[start..end].to_vec()).unwrap()
        };

        // offset 0 is the shared lone NUL == empty string
        assert_eq!(pool[0], 0);
        assert_eq!(read_str(0), "");

        assert_eq!(read_str(hdr.rootfs_off), "/mnt/root");
        assert_eq!(read_str(hdr.lowers_off), "/lo/a:/lo/b");
        assert_eq!(read_str(hdr.hostname_off), "dbhost");
        assert_eq!(read_str(hdr.netns_off), "nskey");
        assert_eq!(read_str(hdr.publish_off), "8080:80,5432:5432");
        assert_eq!(read_str(hdr.volumes_off), "ro:/data:/hostdata,/cfg:/hcfg");
        assert_eq!(read_str(hdr.ulimits_off), "nofile=1024:4096");
        assert_eq!(read_str(hdr.cwd_off), "/work");
        assert_eq!(read_str(hdr.guest_env_off), "A=1\nB=2");
        assert_eq!(read_str(hdr.pcache_off), "/pc");
        assert_eq!(read_str(hdr.netbr_off), "br7");
        assert_eq!(read_str(hdr.ip_off), "10.1.2.3");
        assert_eq!(read_str(hdr.fsgen_off), "/run/fsgen");
        // VPN egress SOCKS endpoint round-trips as its own pool offset (was silently unrepresentable).
        assert_eq!(read_str(hdr.egress_off), "127.30.0.1:1080");
        assert_eq!(hdr.reserved0, 0, "the explicit tail pad is always zero");

        // Unset string fields (never assigned above) must read as offset 0 == "".
        // (`gpu_iosurface` and `nopcache` default off; neither is a pool offset.)
        assert_eq!(hdr.gpu_iosurface, 0);
        assert_eq!(hdr.nopcache, 0);

        // The per-container pcache kill switch round-trips as a header bool (was the reserved tail pad).
        let np = LaunchConfig { nopcache: true, ..Default::default() }.to_wire();
        let nphdr: WireHeader = unsafe { std::ptr::read_unaligned(np.as_ptr() as *const WireHeader) };
        assert_eq!(nphdr.nopcache, 1);
        // No VPN configured (the default) -> egress_off is 0 == "" -> the engine keeps direct egress.
        assert_eq!(nphdr.egress_off, 0, "unset egress must read as the empty string (direct egress)");

        // argv: NUL-separated args, terminated by an extra NUL (double-NUL after the last arg).
        let astart = hdr.argv_off as usize;
        let expected: Vec<u8> = b"/bin/sh\0-c\0echo hi\0\0".to_vec();
        assert_eq!(&pool[astart..astart + expected.len()], &expected[..]);
        // Splitting on NUL up to the double-NUL recovers the argv exactly.
        let argv_bytes = &pool[astart..astart + expected.len() - 1]; // drop the final terminator NUL
        let argv: Vec<String> = argv_bytes
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8(s.to_vec()).unwrap())
            .collect();
        assert_eq!(argv, vec!["/bin/sh", "-c", "echo hi"]);
    }
}
