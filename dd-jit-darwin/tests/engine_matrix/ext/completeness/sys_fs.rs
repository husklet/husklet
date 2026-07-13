use super::*;

/// Filesystem sync / stat / lock / ioctl syscalls.
pub(super) fn sys_fs() -> Group {
    group(
        "comp-sys-fs",
        vec![
            sy("fstatfs", "completeness/sys_fstatfs.c"),
            // GAP syncfs: engine returns an error (syncfs=0) while fsync/fdatasync/sync_file_range all work.
            sy("sync-family", "completeness/sys_sync_family.c"),
            sy("ioctl-fio", "completeness/sys_ioctl_fio.c"), // FIONREAD/FIONBIO
            sy("flock", "completeness/sys_flock.c"),
            sy("fadvise", "completeness/sys_fadvise.c"),
        ],
    )
}
