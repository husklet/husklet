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
        ],
    )
}
