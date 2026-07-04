//! procfs — /proc /sys /dev pseudo-file CONTENT conformance + permission/mode fidelity. Owner: container-fs
//! agent. Edit ONLY this file. The zero-stub release gate for "basic Linux internals": each fixture reads
//! the ACTUAL content of a pseudo-file and asserts real Linux structure/values (fixed constants byte-exact,
//! host-derived values shape-exact) so a stub/empty/placeholder handler fails. Linux-form files (/proc,
//! /sys, most of /dev) have no portable shape -> `src` (both Linux engines, run bare so the synth fires via
//! proc_open/synth_stat). permbits is a real chmod/stat round-trip -> `port` (all three engines).
#![allow(unused_imports)]
use crate::{group, src, port, fixture, in_rootfs, Case, Engine, Group};

pub fn groups() -> Vec<Group> { vec![proc_content(), dev_sys(), perms()] }

/// /proc top-level + /proc/self content, verdict-style (ok=1 iff every field/shape assertion held).
fn proc_content() -> Group {
    group("procfs-proc", vec![
        src("pf-sysctl", "ext_procfs/sysctl.c").out("sysctl ok=1\n"),
        src("pf-meminfo", "ext_procfs/meminfo.c").out("meminfo ok=1\n"),
        src("pf-stat", "ext_procfs/pstat.c").out("pstat ok=1\n"),
        src("pf-selfstat", "ext_procfs/selfstat.c").out("selfstat ok=1\n"),
        src("pf-selfstatus", "ext_procfs/selfstatus.c").out("selfstatus ok=1\n"),
        // top/htop/ps read a process's RES from /proc/self/status VmRSS, /proc/self/statm resident, and
        // /proc/self/stat field 24. dd derived the SELF pid's rss from the guest's tracked anon charge,
        // which is 0 for a process resident only in its static image -> RES=0 (a PEER pid already showed a
        // live rss via libproc; only self read 0). Asserts all three are non-zero (fails on the pre-fix engine).
        src("pf-selfrss", "ext_procfs/selfrss.c").out("selfrss ok=1\n"),
        src("pf-procstate", "ext_procfs/procstate.c").out("procstate ok=1\n"), // #404: cross-proc R/S state

        src("pf-cpuinfo", "ext_procfs/cpuinfo.c").out("cpuinfo ok=1\n"),
        src("pf-misc", "ext_procfs/miscfiles.c").out("miscfiles ok=1\n"),
        src("pf-net", "ext_procfs/netfiles.c").out("netfiles ok=1\n"),
        // #289 round 2 (networking tool class): tcp6/udp6 wide v6 header, /proc/net/{netstat,snmp6,ipv6_route},
        // and the /proc/[self|pid]/net/* namespaced mirrors -- each a dd-only divergence vs docker before the fix.
        src("pf-net2", "ext_procfs/netfiles2.c").out("netfiles2 ok=1\n"),
        src("pf-maps", "ext_procfs/maps.c").out("maps ok=1\n"),
    ])
}

/// /dev char-device nodes (type + Linux rdev + read/write semantics) and /sys attributes.
fn dev_sys() -> Group {
    group("procfs-devsys", vec![
        src("pf-devnodes", "ext_procfs/devnodes.c").out("devnodes ok=1\n"),
        src("pf-sysfs", "ext_procfs/sysfs.c").out("sysfs ok=1\n"),
    ])
}

/// Permission/mode fidelity — chmod full mode space + special bits (suid/sgid/sticky) via stat(2), and
/// access(2). Real chmod/stat, so golden across Linux-emulated and native-macOS.
fn perms() -> Group {
    group("procfs-perms", vec![
        port("pf-permbits", "ext_procfs/permbits.c").out("permbits ok=1\n"),
    ])
}
