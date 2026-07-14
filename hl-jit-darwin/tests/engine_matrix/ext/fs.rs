//! fs — FILESYSTEM/STORAGE tool fidelity: statfs f_type + geometry per mount point, and the mount
//! tables (/proc/mounts, /proc/self/mountinfo) that df/mount/findmnt parse. Owner: storage-fs agent.
//! Edit ONLY this file. These run in the alpine OVERLAY rootfs (container semantics) so the statfs/proc
//! synth fires exactly as under `docker run`; verdicts are golden constants derived from the real docker
//! (runc) oracle, so a stub / wrong-magic / wrong-geometry synth fails.
#![allow(unused_imports)]
use crate::support::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

const LIN: &[Engine] = &[Engine::LinuxAarch64, Engine::LinuxX86_64];

pub fn groups() -> Vec<Group> {
    vec![storage()]
}

fn storage() -> Group {
    group(
        "fs-storage",
        vec![
            // statfs f_type + pseudo-fs geometry. Pre-fix hl stamped TMPFS_MAGIC + host disk geometry on EVERY
            // path: `/` looked like tmpfs (not overlay) and /proc & /sys reported a huge nonzero size, so
            // `stat -f -c %T` named them wrong and `df -h` LISTED /proc & /sys (real docker hides 0-block fs).
            // Fix classifies by the resolved guest mount in os/linux/syscall/fs.c. Overlay-only -> Linux engines.
            src("fs-statfs-type", "ext_fs/statfs_type.c")
                .rootfs("alpine")
                .overlay()
                .only(LIN)
                .out("statfs-type ok=1\n"),
            // Mount-table shape df/mount/findmnt parse (/proc/mounts fstab form + /proc/self/mountinfo "-" form).
            // Verified-clean guard: the synth was already structurally correct; locks it against regression.
            src("fs-mounttab", "ext_fs/mounttab.c")
                .rootfs("alpine")
                .overlay()
                .only(LIN)
                .out("mounttab ok=1\n"),
            // Pseudo-mount COMPLETENESS (audit): hl omitted the /dev/shm, /dev/pts, /dev/mqueue mounts, listed
            // no cgroup2 line in /proc/mounts, and marked sysfs rw where runc marks it ro. Asserts each line the
            // docker oracle shows in BOTH /proc/mounts (fstab form) and /proc/self/mountinfo ("-"-separated).
            src("fs-mountpseudo", "ext_fs/mountpseudo.c")
                .rootfs("alpine")
                .overlay()
                .only(LIN)
                .out("mountpseudo ok=1\n"),
            // /dev/shm is a REAL per-container tmpfs: a shm_open segment appears as a regular file in the
            // /dev/shm listing, stats as a regular file, and statfs("/dev/shm")==tmpfs (was invisible pre-fix,
            // backed by a flat /tmp file in a global host namespace). Verified vs the docker (runc) oracle.
            src("fs-shm-visible", "ext_fs/shm_visible.c")
                .rootfs("alpine")
                .overlay()
                .only(LIN)
                .out("shm_visible ok=1\n"),
        ],
    )
}
