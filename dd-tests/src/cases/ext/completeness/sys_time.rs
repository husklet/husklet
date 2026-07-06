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
            // GAP adjtimex/clock_adjtime: read-only (modes=0) query returns an error under the engine.
            sy("adjtimex", "completeness/sys_adjtimex.c"),
        ],
    )
}
