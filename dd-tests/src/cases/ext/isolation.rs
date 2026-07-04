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
        // NO --cpus (#412): nproc / sched_getaffinity / /proc/cpuinfo / /sys .../cpu/online must ALL report
        // the true HOST core count (via macOS hw.activecpu), not 1. Oracle -> the JIT must byte-match native
        // (the real host count) on both Linux engines; before the fix the mac-side engine's sysconf gave 1.
        src("cpu-default", "ext_iso/cpudefault.c").oracle(),
        // #412 part 2 (htop still showed 1 CPU): htop sizes its CPU meters by opendir()ing
        // /sys/devices/system/cpu and counting the cpuN SUBDIRECTORIES (not the online/possible/present
        // files) -- finding none it keeps its built-in default of 1. This probe runs htop's exact algorithm
        // plus glibc get_nprocs_conf() (also a cpuN-dir count). Oracle -> must byte-match the native host
        // count. Before the fix the engine served the cpu FILES but never the DIRECTORY, so htop_cpus=1.
        src("cpu-sysfs-dirs", "ext_iso/cpusysfs.c").oracle(),
        // --cpus=2: the materialized cpuN directory count is clamped to the allotment too (2 cpuN dirs),
        // so htop's meter sizing honors --cpus exactly like nproc does.
        src("cpu-sysfs-dirs-cap2", "ext_iso/cpusysfs.c").cpus(2)
            .out("htop_cpus=2 get_nprocs=2 get_nprocs_conf=2\n"),
        // --cpus: nproc / sched_getaffinity / sysconf all report the CAP, not the host core count.
        port("cpucap-1", "ext_iso/cpucount.c").cpus(1).out("cpucount=1\n"),
        port("cpucap-2", "ext_iso/cpucount.c").cpus(2).out("cpucount=2\n"),
        // --cpus=2 with the 4-path cross-check guest: the allotment clamp still holds on every path.
        src("cpu-default-cap2", "ext_iso/cpudefault.c").cpus(2).out("cpus=2\n"),
        // --ulimit nofile: getrlimit(RLIMIT_NOFILE) returns exactly the requested soft/hard pair.
        port("ulimit-nofile", "ext_iso/ulimit.c").ulimit("nofile", 1024, 2048)
            .out("nofile soft=1024 hard=2048\n"),
        // #412 part 2, the REAL overlay-rootfs path `docker run htop` takes: a busybox shell in an alpine
        // image counts the /sys/devices/system/cpu/cpuN dirs (htop's source) and asserts it equals nproc.
        // Host-count-independent MATCH verdict (stable across engines/arches/hosts). Exercises the overlay
        // relative-open re-entry that a bare guest doesn't: without the cpuN-dir synth this printed MISMATCH.
        in_rootfs("cpu-sysfs-rootfs", "alpine", &["/bin/sh", "-c", CPUDIR_PROBE])
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .out("cpudirs=MATCH\n"),
    ])
}

// Count the cpuN directories htop enumerates in /sys/devices/system/cpu and compare to nproc. A
// host-independent verdict: MATCH iff the engine materialized one cpuN dir per online CPU.
const CPUDIR_PROBE: &str = "\
n=$(ls -d /sys/devices/system/cpu/cpu[0-9]* 2>/dev/null | wc -l); \
if [ \"$n\" -eq \"$(nproc)\" ]; then echo cpudirs=MATCH; else echo cpudirs=MISMATCH n=$n nproc=$(nproc); fi";

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
