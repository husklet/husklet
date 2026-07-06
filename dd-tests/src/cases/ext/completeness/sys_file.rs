use super::*;

// ===================== SYSCALL COMPLETENESS =====================

/// File-family syscalls: the at-suffixed / extended file ops a modern libc emits.
pub(super) fn sys_file() -> Group {
    group(
        "comp-sys-file",
        vec![
            // GAP openat2: engine opens a fd (ok=1) but the read returns \0 not the file's byte — openat2
            // doesn't honor the open_how flags / opens the wrong backing fd. (jit byte=\0 vs native byte=Z)
            sy("openat2", "completeness/sys_openat2.c"),
            sy("statx", "completeness/sys_statx.c"),
            sy("faccessat2", "completeness/sys_faccessat2.c"),
            sy("readlinkat", "completeness/sys_readlinkat.c"),
            sy("fchmodat", "completeness/sys_fchmodat.c"),
            sy("utimensat", "completeness/sys_utimensat.c"),
            sy("copy_file_range", "completeness/sys_copy_file_range.c"),
            sy("fallocate", "completeness/sys_fallocate.c"),
            // GAP name_to_handle_at: engine returns an error (ok=0) where real Linux succeeds (ok=1).
            sy("name_to_handle_at", "completeness/sys_name_to_handle_at.c"),
            sy("truncate", "completeness/sys_truncate.c"),
            sy("fsops", "completeness/sys_fsops.c"), // linkat/symlinkat/mkdirat/unlinkat
        ],
    )
}
