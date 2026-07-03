//! isolation — container isolation + resource fidelity (docker --cpus / --read-only / --ulimit and the
//! runc MaskedPaths / ReadonlyPaths). Owner: container-isolation agent. Edit ONLY this file.
//!
//! Coverage spans all three matrix engines wherever the control can apply:
//!  - CPU-count cap + ulimit: PORTABLE guests, run JIT-emulated on both Linux engines AND native (under
//!    darwinjail) on macOS -- one source, byte-identical golden output on every engine.
//!  - read-only rootfs: Linux via a busybox shell in a real image rootfs (both arches); darwin via a
//!    portable guest under darwinjail with the rootfs jail armed. All assert rootfs write -> EROFS,
//!    /tmp still writable.
//!  - masked / read-only /proc paths: a Linux-kernel concept (there is no procfs on macOS), so these are
//!    Linux-only BY CONSTRUCTION -- darwin is excluded here deliberately, not silently skipped (a masked
//!    /proc/kcore has no darwin analogue; the darwin container's confinement is the darwinjail sandbox,
//!    covered by the read-only case above).
#![allow(unused_imports)]
use crate::{group, src, port, in_rootfs, Case, Engine, Group};

pub fn groups() -> Vec<Group> { vec![resources(), rootfs_ro(), proc_masking()] }

/// docker --cpus / --ulimit: the guest self-sizes to its allotment, and getrlimit reflects the override.
/// Portable -> runs on linux/x86_64, linux/aarch64, and darwin/aarch64.
fn resources() -> Group {
    group("iso-resources", vec![
        // --cpus: nproc / sched_getaffinity / sysconf all report the CAP, not the host core count.
        port("cpucap-1", "ext_iso/cpucount.c").cpus(1).out("cpucount=1\n"),
        port("cpucap-2", "ext_iso/cpucount.c").cpus(2).out("cpucount=2\n"),
        // --ulimit nofile: getrlimit(RLIMIT_NOFILE) returns exactly the requested soft/hard pair.
        port("ulimit-nofile", "ext_iso/ulimit.c").ulimit("nofile", 1024, 2048)
            .out("nofile soft=1024 hard=2048\n"),
    ])
}

// A busybox shell probe: a write to the rootfs root must fail "Read-only file system", while /tmp stays
// writable. Under --read-only -> root=RO tmp=OK; without it (the old behaviour) -> root=RW.
const RO_PROBE: &str = "\
if ( echo x > /dd_ro_probe ) 2>&1 | grep -qi 'read-only'; then echo root=RO; else echo root=RW; rm -f /dd_ro_probe; fi; \
if ( echo x > /tmp/dd_w_probe ) 2>/dev/null; then echo tmp=OK; rm -f /tmp/dd_w_probe; else echo tmp=FAIL; fi";

/// docker --read-only: the rootfs is EROFS, the /tmp pseudo-mount stays writable. Linux via a real image
/// rootfs (both arches); darwin via a portable guest with the darwinjail rootfs armed.
fn rootfs_ro() -> Group {
    group("iso-readonly", vec![
        in_rootfs("readonly-rootfs", "alpine", &["/bin/sh", "-c", RO_PROBE])
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .read_only()
            .has("root=RO").has("tmp=OK"),
        // darwin: the same contract via darwinjail's EROFS interposers (the guest runs native, the rootfs
        // jail is armed by DD_ROOTFS + DD_ROOTFS_RO). rootfs("alpine") just supplies a jail prefix -- the
        // guest is our own Mach-O, not run FROM the rootfs.
        port("readonly-rootfs-darwin", "ext_iso/rofs.c")
            .rootfs("alpine").read_only()
            .only(&[Engine::DarwinAarch64])
            .out("root=EROFS tmp=OK\n"),
    ])
}

// runc MaskedPaths: exist but empty (not ENOENT). ReadonlyPaths: a write fails "Read-only file system".
const MASK_PROBE: &str = "\
test -e /proc/kcore && echo kcore=exists || echo kcore=missing; \
echo kcorelen=$(wc -c < /proc/kcore 2>/dev/null || echo NA); \
test -d /proc/scsi && echo scsi=dir || echo scsi=nodir; \
test -e /sys/firmware && echo firmware=exists || echo firmware=missing; \
if ( echo x > /proc/sys/kernel/hostname ) 2>&1 | grep -qi 'read-only'; then echo sysctl=RO; else echo sysctl=RW; fi";

/// runc MaskedPaths + ReadonlyPaths (a Linux procfs concept -> Linux engines only; see the module note on
/// why darwin is excluded by construction rather than skipped).
fn proc_masking() -> Group {
    group("iso-proc-mask", vec![
        in_rootfs("masked-paths", "alpine", &["/bin/sh", "-c", MASK_PROBE])
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .has("kcore=exists").has("kcorelen=0").has("scsi=dir")
            .has("firmware=exists").has("sysctl=RO"),
    ])
}
