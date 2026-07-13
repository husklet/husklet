use super::*;

/// Time / clock / timer syscalls.
pub(super) fn sys_time() -> Group {
    group(
        "comp-sys-time",
        vec![
            sy("clock-getres", "completeness/sys_clock_getres.c"),
            sy("clock-variants", "completeness/sys_clock_variants.c"), // PROCESS/THREAD CPUTIME, BOOTTIME, RAW
            sy("timer-create", "completeness/sys_timer_create.c"),
            sy("itimer", "completeness/sys_itimer.c"),
            sy("adjtimex", "completeness/sys_adjtimex.c"), // read-only (modes=0) query — oracle-identical to native
        ],
    )
}
