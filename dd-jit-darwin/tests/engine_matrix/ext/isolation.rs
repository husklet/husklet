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
use dd_tests::{group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![resources(), rootfs_ro(), proc_masking()]
}

/// docker --cpus / --ulimit: the guest self-sizes to its allotment, and getrlimit reflects the override.
/// Portable -> runs on linux/x86_64, linux/aarch64, and darwin/aarch64.
fn resources() -> Group {
    group("iso-resources", vec![
        // NO --cpus: nproc / sched_getaffinity / /proc/cpuinfo / /sys.../cpu/online must ALL report
        // the true HOST core count (via macOS hw.activecpu), not 1. Oracle -> the JIT must byte-match native
        // (the real host count) on both Linux engines; before the fix the mac-side engine's sysconf gave 1.
        src("cpu-default", "ext_iso/cpudefault.c").oracle(),
        // part 2 (htop still showed 1 CPU): htop sizes its CPU meters by opendiring
        // /sys/devices/system/cpu and counting the cpuN SUBDIRECTORIES (not the online/possible/present
        // files) -- finding none it keeps its built-in default of 1. This probe runs htop's exact algorithm
        // plus glibc get_nprocs_conf() (also a cpuN-dir count). Oracle -> must byte-match the native host
        // count. Before the fix the engine served the cpu FILES but never the DIRECTORY, so htop_cpus=1.
        src("cpu-sysfs-dirs", "ext_iso/cpusysfs.c").oracle(),
        // --cpus=2: the materialized cpuN directory count is clamped to the allotment too (2 cpuN dirs),
        // so htop's meter sizing honors --cpus exactly like nproc does.
        src("cpu-sysfs-dirs-cap2", "ext_iso/cpusysfs.c").cpus(2)
            .out("htop_cpus=2 get_nprocs=2 get_nprocs_conf=2\n"),
        // part 3 (htop cores all show IDENTICAL usage): htop/top compute per-core busy% from the
        // DELTA of each /proc/stat cpuN line between two samples. dd emitted every cpuN line as the
        // aggregate host ticks split EVENLY (aggregate/ncpu), so every core's delta was identical -> all
        // meters move in lockstep. This probe pegs one core for ~250ms between two reads and checks the
        // per-core deltas are not all identical. Oracle -> real Linux shows the busy core diverging
        // (deltas_differ=1); before the fix dd showed deltas_differ=0 (every core the same split share).
        src("procstat-percpu", "ext_iso/procstat_percpu.c").oracle()
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // lscpu / util-linux reconstruct sockets/cores/threads from /sys/devices/system/cpu/cpuN/topology/*
        // (core_id, physical_package_id, thread_siblings_list, core_cpus_list, core_siblings_list, ...). dd
        // materialized the cpuN dirs but served NONE of the topology attribute files -> every open
        // ENOENT'd and lscpu mis-counted. This probe drives lscpu's exact per-CPU reads and asserts a
        // host-independent structural verdict (values are host-variant; STRUCTURE is not). Linux-only (/sys).
        src("cpu-topology", "ext_iso/cputopo.c").out("cputopo ok=1\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // --cpus: nproc / sched_getaffinity / sysconf all report the CAP, not the host core count.
        port("cpucap-1", "ext_iso/cpucount.c").cpus(1).out("cpucount=1\n"),
        port("cpucap-2", "ext_iso/cpucount.c").cpus(2).out("cpucount=2\n"),
        // --cpus=2 with the 4-path cross-check guest: the allotment clamp still holds on every path.
        src("cpu-default-cap2", "ext_iso/cpudefault.c").cpus(2).out("cpus=2\n"),
        // --ulimit nofile: getrlimit(RLIMIT_NOFILE) returns exactly the requested soft/hard pair.
        port("ulimit-nofile", "ext_iso/ulimit.c").ulimit("nofile", 1024, 2048)
            .out("nofile soft=1024 hard=2048\n"),
        // cgroup v2: the surface the JVM (-XX:+UseContainerSupport), the Go runtime (GOMAXPROCS via
        // cpu.max quota/period, GOMEMLIMIT via memory.max), Node/libuv and systemd SIZE THEMSELVES from.
        // Unconstrained -> the kernel "max" sentinels (cpu.max="max 100000", memory.max/swap.max="max");
        // the v2 markers (statfs CGROUP2 magic, cgroup.controllers, cgroup.type=domain) present; the v1
        // fallback ENOENT (pure-v2 host, exactly like docker/OrbStack -- NOT fabricated). Linux-only: cgroup
        // is a kernel concept with no darwin analogue. Golden byte-identical to `docker run` (runc oracle).
        src("cgroup-v2-default", "ext_iso/cgroup.c")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .out("cgv2=1 controllers=OK type=domain v1absent=1 cpu.max=[max 100000] mem.max=[max] \
mem.swap.max=[max] mem.high=[max] cpu.weight=[100] pids.max=[max]\n"),
        // docker --cpus=2: cpu.max quota MUST equal 2*period ("200000 100000"). A runtime computes
        // cpus=quota/period, so this is what makes Go GOMAXPROCS / JVM availableProcessors self-size to the
        // allotment instead of one worker per HOST core. Verified vs `docker run --cpus=2`.
        src("cgroup-cpu-cap2", "ext_iso/cgroup.c").cpus(2)
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .out("cgv2=1 controllers=OK type=domain v1absent=1 cpu.max=[200000 100000] mem.max=[max] \
mem.swap.max=[max] mem.high=[max] cpu.weight=[100] pids.max=[max]\n"),
        // docker --memory=512m: memory.max == the byte cap (536870912) and memory.swap.max == the same
        // (docker's default --memory-swap=2*mem -> the v2 swap-ONLY ceiling = mem). This is what the JVM
        // UseContainerSupport + GOMEMLIMIT read to bound the heap. Verified vs `docker run --memory=512m`.
        src("cgroup-mem-cap512m", "ext_iso/cgroup.c").mem(536870912)
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .out("cgv2=1 controllers=OK type=domain v1absent=1 cpu.max=[max 100000] mem.max=[536870912] \
mem.swap.max=[536870912] mem.high=[max] cpu.weight=[100] pids.max=[max]\n"),
        // The REAL overlay-rootfs `docker run --cpus --memory` path a JVM/Go image takes: a busybox shell in
        // an alpine image reads the sizing files through the overlay relative-open re-entry (which a bare
        // guest doesn't exercise). cpu.max reflects --cpus=2, memory.max reflects --memory=512m, and the
        // controller set is present. Golden byte-identical to `docker run --cpus=2 --memory=512m`.
        in_rootfs("cgroup-rootfs-caps", "alpine", &["/bin/sh", "-c", CGROUP_PROBE])
            .cpus(2).mem(536870912)
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
            .out("cpu.max=[200000 100000] mem.max=[536870912] controllers=[cpuset cpu io memory pids]\n"),
        // part 2, the REAL overlay-rootfs path `docker run htop` takes: a busybox shell in an alpine
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

// Read the JVM/Go self-sizing cgroup files through the overlay rootfs (busybox cat). The values reflect
// docker --cpus/--memory exactly; the controller set proves the v2 hierarchy is present in the overlay view.
const CGROUP_PROBE: &str = "\
printf 'cpu.max=[%s] mem.max=[%s] controllers=[%s]\\n' \
\"$(cat /sys/fs/cgroup/cpu.max)\" \"$(cat /sys/fs/cgroup/memory.max)\" \
\"$(cat /sys/fs/cgroup/cgroup.controllers)\"";

// A busybox shell probe: a write to the rootfs root must fail "Read-only file system", while /tmp stays
// writable. Under --read-only -> root=RO tmp=OK; without it (the old behaviour) -> root=RW.
const RO_PROBE: &str = "\
if ( echo x > /dd_ro_probe ) 2>&1 | grep -qi 'read-only'; then echo root=RO; else echo root=RW; rm -f /dd_ro_probe; fi; \
if ( echo x > /tmp/dd_w_probe ) 2>/dev/null; then echo tmp=OK; rm -f /tmp/dd_w_probe; else echo tmp=FAIL; fi";

/// docker --read-only: the rootfs is EROFS, the /tmp pseudo-mount stays writable. Linux via a real image
/// rootfs (both arches); darwin via a portable guest with the darwinjail rootfs armed.
fn rootfs_ro() -> Group {
    group(
        "iso-readonly",
        vec![
            in_rootfs("readonly-rootfs", "alpine", &["/bin/sh", "-c", RO_PROBE])
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .read_only()
                .has("root=RO")
                .has("tmp=OK"),
            // darwin: the same contract via darwinjail's EROFS interposers (the guest runs native, the rootfs
            // jail is armed by DD_ROOTFS + DD_ROOTFS_RO). rootfs("alpine") just supplies a jail prefix -- the
            // guest is our own Mach-O, not run FROM the rootfs.
            port("readonly-rootfs-darwin", "ext_iso/rofs.c")
                .rootfs("alpine")
                .read_only()
                .only(&[Engine::DarwinAarch64])
                .out("root=EROFS tmp=OK\n"),
        ],
    )
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
    group(
        "iso-proc-mask",
        vec![
            in_rootfs("masked-paths", "alpine", &["/bin/sh", "-c", MASK_PROBE])
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .has("kcore=exists")
                .has("kcorelen=0")
                .has("scsi=dir")
                .has("firmware=exists")
                .has("sysctl=RO"),
        ],
    )
}
