use super::*;

// ===================== SYSCALL COMPLETENESS =====================

/// File-family syscalls: the at-suffixed / extended file ops a modern libc emits.
pub(super) fn sys_file() -> Group {
    group(
        "comp-sys-file",
        vec![
            // openat2(2) via open_how opens the backing fd and reads the file's byte back — oracle-identical to native.
            sy("openat2", "completeness/sys_openat2.c"),
            sy("statx", "completeness/sys_statx.c"),
            sy("faccessat2", "completeness/sys_faccessat2.c"),
            sy("readlinkat", "completeness/sys_readlinkat.c"),
            sy("fchmodat", "completeness/sys_fchmodat.c"),
            sy("utimensat", "completeness/sys_utimensat.c"),
            sy("copy_file_range", "completeness/sys_copy_file_range.c"),
            sy("fallocate", "completeness/sys_fallocate.c"),
            sy("name_to_handle_at", "completeness/sys_name_to_handle_at.c"), // handle round-trip — oracle-identical to native
            sy("truncate", "completeness/sys_truncate.c"),
            sy("fsops", "completeness/sys_fsops.c"), // linkat/symlinkat/mkdirat/unlinkat
            // S3DB_DURABILITY=none|fast|strict fsync policy (helpers.c s3db_sync_fd). The DEFAULT (env unset)
            // path is byte-identical to native Linux fsync, so oracle-diff it. The explicit modes have no
            // native equivalent (native ignores the var) -> golden-pin them: coherence (readback_ok=1) holds
            // in every mode, and `none` is a genuine no-op (returns 0 even for a bad fd, issuing no real
            // fsync) while fast/strict issue the real syscall and fail (-1) on a bad fd, exactly like Linux.
            sy("s3db-durability-default", "completeness/s3db_durability.c"),
            src("s3db-durability-none", "completeness/s3db_durability.c")
                .env("S3DB_DURABILITY", "none")
                .has("regfile_fsync=0 regfile_fdatasync=0 readback_ok=1 badfd_fsync_ret=0")
                .exit(0),
            src("s3db-durability-fast", "completeness/s3db_durability.c")
                .env("S3DB_DURABILITY", "fast")
                .has("regfile_fsync=0 regfile_fdatasync=0 readback_ok=1 badfd_fsync_ret=-1")
                .exit(0),
            src("s3db-durability-strict", "completeness/s3db_durability.c")
                .env("S3DB_DURABILITY", "strict")
                .has("regfile_fsync=0 regfile_fdatasync=0 readback_ok=1 badfd_fsync_ret=-1")
                .exit(0),
        ],
    )
}
