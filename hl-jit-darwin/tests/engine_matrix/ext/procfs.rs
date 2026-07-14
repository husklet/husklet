//! procfs — /proc /sys /dev pseudo-file CONTENT conformance + permission/mode fidelity. Owner: container-fs
//! agent. Edit ONLY this file. The zero-stub release gate for "basic Linux internals": each fixture reads
//! the ACTUAL content of a pseudo-file and asserts real Linux structure/values (fixed constants byte-exact,
//! host-derived values shape-exact) so a stub/empty/placeholder handler fails. Linux-form files (/proc,
//! /sys, most of /dev) have no portable shape -> `src` (both Linux engines, run bare so the synth fires via
//! proc_open/synth_stat). permbits is a real chmod/stat round-trip -> `port` (all three engines).
#![allow(unused_imports)]
use crate::support::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![proc_content(), dev_sys(), perms(), supp_groups()]
}

// a busybox/coreutils shell in a real image rootfs. The container run user (root) must hold the
// IMAGE-DERIVED supplementary GID set runc computes from /etc/passwd + /etc/group (additionalGids) --
// getgroups(2) (via `id -G`) and /proc/self/status `Groups:` must AGREE and equal that set. dd previously
// left both EMPTY (only the primary gid / nothing), so any group-membership check inside a container failed.
// The verdict normalizes both to a sorted-unique multiset so it is host-independent (the set is image-
// derived, so a fixed golden per image is valid). Exercises the overlay relative-open path a bare guest
// does not. Verified byte-exact vs OrbStack real docker (runc) for each image.
const GROUPS_PROBE: &str = "\
gg=$(id -G | tr ' ' '\\n' | grep -v '^$' | sort -nu | tr '\\n' ' '); \
st=$(grep '^Groups:' /proc/self/status | cut -f2- | tr ' ' '\\n' | grep -v '^$' | sort -nu | tr '\\n' ' '); \
echo \"gg=[$gg] st=[$st]\"";

/// Supplementary groups (runc additionalGids) — image-derived, must match `docker run <img> id` exactly.
fn supp_groups() -> Group {
    let both = &[Engine::LinuxAarch64, Engine::LinuxX86_64];
    group(
        "procfs-groups",
        vec![
            // alpine: root is a listed member of many groups (bin,daemon,sys,adm,disk,wheel,floppy,dialout,
            // tape,video) AND of its own primary group root -> getgroups & status both carry the full set.
            in_rootfs(
                "pf-groups-alpine",
                "alpine",
                &["/bin/sh", "-c", GROUPS_PROBE],
            )
            .only(both)
            .out("gg=[0 1 2 3 4 6 10 11 20 26 27 ] st=[0 1 2 3 4 6 10 11 20 26 27 ]\n"),
            // ubuntu/debian: group root(0) has NO members and no other group lists root -> the set is the
            // primary gid alone (0). Guards that the parse is genuinely image-derived, not a fixed alpine list.
            in_rootfs(
                "pf-groups-ubuntu",
                "ubuntu",
                &["/bin/sh", "-c", GROUPS_PROBE],
            )
            .only(both)
            .out("gg=[0 ] st=[0 ]\n"),
            // busybox: only group wheel(10) lists root -> primary 0 plus 10.
            in_rootfs(
                "pf-groups-busybox",
                "busybox",
                &["/bin/sh", "-c", GROUPS_PROBE],
            )
            .only(both)
            .out("gg=[0 10 ] st=[0 10 ]\n"),
        ],
    )
}

/// /proc top-level + /proc/self content, verdict-style (ok=1 iff every field/shape assertion held).
fn proc_content() -> Group {
    group(
        "procfs-proc",
        vec![
            src("pf-sysctl", "ext_procfs/sysctl.c").out("sysctl ok=1\n"),
            src("pf-meminfo", "ext_procfs/meminfo.c").out("meminfo ok=1\n"),
            src("pf-stat", "ext_procfs/pstat.c").out("pstat ok=1\n"),
            src("pf-selfstat", "ext_procfs/selfstat.c").out("selfstat ok=1\n"),
            src("pf-selfstatus", "ext_procfs/selfstatus.c").out("selfstatus ok=1\n"),
            // Capabilities + security context: default docker drops all but the 14 CapBnd=a80425fb caps, and
            // /proc/self/status, capget(2) and PR_CAPBSET_READ must all agree (nginx/postgres/capsh gate on it).
            src("pf-selfcaps", "ext_procfs/selfcaps.c").out("selfcaps ok=1\n"),
            // top/htop/ps read a process's RES from /proc/self/status VmRSS, /proc/self/statm resident, and
            // /proc/self/stat field 24. dd derived the SELF pid's rss from the guest's tracked anon charge,
            // which is 0 for a process resident only in its static image -> RES=0 (a PEER pid already showed a
            // live rss via libproc; only self read 0). Asserts all three are non-zero (fails on the pre-fix engine).
            src("pf-selfrss", "ext_procfs/selfrss.c").out("selfrss ok=1\n"),
            src("pf-procstate", "ext_procfs/procstate.c").out("procstate ok=1\n"), // cross-proc R/S state
            // A child parked in futex(FUTEX_WAIT) is interruptible-sleep 'S' on Linux, NOT 'R'. dd stamped
            // 'R' (macOS SRUN can't express "asleep in a futex") -> blocked threads hidden from monitors and
            // deadlock diagnostics. Parent reads the child's /proc/<pid>/stat field 3 AND status State:.
            src("pf-futexstate", "ext_procfs/futexstate.c").out("futexstate ok=1\n"),
            src("pf-cpuinfo", "ext_procfs/cpuinfo.c").out("cpuinfo ok=1\n"),
            src("pf-misc", "ext_procfs/miscfiles.c").out("miscfiles ok=1\n"),
            src("pf-net", "ext_procfs/netfiles.c").out("netfiles ok=1\n"),
            // round 2 (networking tool class): tcp6/udp6 wide v6 header, /proc/net/{netstat,snmp6,ipv6_route},
            // and the /proc/[self|pid]/net/* namespaced mirrors -- each a dd-only divergence vs docker before the fix.
            src("pf-net2", "ext_procfs/netfiles2.c").out("netfiles2 ok=1\n"),
            src("pf-maps", "ext_procfs/maps.c").out("maps ok=1\n"),
        ],
    )
}

/// /dev char-device nodes (type + Linux rdev + read/write semantics) and /sys attributes.
fn dev_sys() -> Group {
    group(
        "procfs-devsys",
        vec![
            src("pf-devnodes", "ext_procfs/devnodes.c").out("devnodes ok=1\n"),
            src("pf-sysfs", "ext_procfs/sysfs.c").out("sysfs ok=1\n"),
            // --network none (HL_NET_ISOLATE): eth0 is hidden by readdir AND by direct stat/open, so a
            // direct /sys/class/net/eth0 probe can't contradict the interface listing. lo stays visible.
            src("pf-netnone-direct", "ext_procfs/netnone.c")
                .env("HL_NET_ISOLATE", "1")
                .out("netnone ok=1\n"),
            // -v/--mount binds (HL_VOL) appear in /proc/mounts + /proc/self/mountinfo so findmnt/df/JVM
            // mount discovery see the guest's binds, not a false namespace.
            src("pf-mountinfo-bind", "ext_procfs/bindmount.c")
                .env("HL_VOL", "/mnt:/tmp")
                .out("bindmount ok=1\n"),
            // lsns/nsenter inspect a live peer's /proc/<pid>/ns: a container shares one namespace set, so
            // the peer ns links readlink to the same "<name>:[<inode>]" values as self.
            src("pf-peer-ns", "ext_procfs/peerns.c").out("peer_ns ok=1\n"),
            // A peer's /proc/<pid>/fd was advertised (listed as a dir entry) but its contents ENOENTed --
            // each guest process has a private fd table in its own macOS process and the proc registry
            // published only comm+argv. Now the peer fd table is read from the host kernel (libproc) and the
            // listing + per-fd readlink (symlink-target view) are served; opening a peer fd stays deferred.
            src("pf-peer-fd", "ext_procfs/peerfd.c").out("peerfd ok=1 dir=1 lstat=1 readlink=1\n"),
            // A NONBLOCKING read on a controlling terminal with no input is EAGAIN on Linux, never EOF(0).
            // dd backs /dev/tty (host device) and /dev/console (/dev/null) with something that returns 0 when
            // empty -> readline/TUI code read the 0 as terminal closure. dd-behavior golden: /dev/console is
            // the deterministic path (always intercepted); /dev/tty is best-effort (harness lacks a ctty).
            src("pf-ttynonblock", "ext_procfs/ttynonblock.c").out("ttynonblock ok=1 console=1 tty=-1\n"),
        ],
    )
}

/// Permission/mode fidelity — chmod full mode space + special bits (suid/sgid/sticky) via stat(2), and
/// access(2). Real chmod/stat, so golden across Linux-emulated and native-macOS.
fn perms() -> Group {
    group(
        "procfs-perms",
        vec![port("pf-permbits", "ext_procfs/permbits.c").out("permbits ok=1\n")],
    )
}
