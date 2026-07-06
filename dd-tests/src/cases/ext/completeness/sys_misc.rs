use super::*;

/// Random / system-info / misc syscalls.
pub(super) fn sys_misc() -> Group {
    group(
        "comp-sys-misc",
        vec![
            sy("getrandom-flags", "completeness/sys_getrandom_flags.c"),
            sy("getentropy", "completeness/sys_getentropy.c"),
            // GAP auxv: AT_PAGESZ leaks the macOS host 16K page on aarch64 (jit 16384 vs native 4096);
            // the x86_64 engine now reports the correct auxv vector. Synthetic auxv vector is wrong on aarch64.
            sy("auxval", "completeness/sys_auxval.c").xfail(ARM),
            sy("sysconf-nproc", "completeness/sys_sysconf_nproc.c"),
            // GAP close_range: engine returns an error (ok=0); real Linux closes the fd range (ok=1).
            sy("close-range", "completeness/sys_close_range.c"),
        ],
    )
}
