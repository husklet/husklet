use super::*;

/// Signal-delivery / disposition syscalls.
pub(super) fn sys_signal() -> Group {
    group(
        "comp-sys-signal",
        vec![
            sy("rt-sigtimedwait", "completeness/sys_rt_sigtimedwait.c"),
            sy("sigaltstack", "completeness/sys_sigaltstack.c"),
            sy("rt-sigpending", "completeness/sys_rt_sigpending.c"),
            // GAP pidfd_open / pidfd_send_signal: engine returns an error from pidfd_open (open_ok=0);
            // real Linux opens a pidfd and signal-0 existence-check succeeds.
            sy("pidfd-signal", "completeness/sys_pidfd_signal.c"),
            sy("pidfd-flags", "completeness/sys_pidfd_flags.c"),
        ],
    )
}
