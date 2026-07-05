//! timex — clock/time-syscall coverage. Owner: timex-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! Beyond ext/posix's clockid/nanosleep: clock_getres across the four standard clocks, gettimeofday
//! cross-checked against clock_gettime, and relative clock_nanosleep — portable golden verdicts. Plus
//! the Linux-only clock ids (BOOTTIME/MONOTONIC_RAW/COARSE) diffed against a native oracle.
#![allow(unused_imports)]
use crate::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![timex()]
}

fn timex() -> Group {
    group(
        "ext-clock",
        vec![
            port("clockres", "ext_timex/clockres.c").out("clockres real=1 mono=1 pcpu=1 tcpu=1\n"),
            port("gettimeofday", "ext_timex/gettimeofday.c")
                .out("gettimeofday usec=1 agrees=1 mono=1 positive=1\n"),
            // clock_nanosleep does not exist on macOS libc, so this is Linux-only, diffed vs native oracle.
            src("clocknanosleep", "ext_timex/clocknanosleep.c").oracle(),
            // Linux-specific clock ids (no macOS equivalent) -> native oracle
            src("clockids", "ext_timex/clockids.c").oracle(),
            // POSIX per-process timers (timer_create/settime/gettime/getoverrun/delete): SIGEV_SIGNAL on
            // REALTIME+MONOTONIC with si_code/si_value, remaining-time, overrun accumulation, SIGEV_NONE, and the
            // EINVAL/EFAULT error surface. timer_create has no macOS libc -> Linux-only, diffed vs native oracle.
            src("posixtimer", "ext_timex/posixtimer.c")
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .oracle(),
            // timerfd (create/settime/gettime + expiration-count read): relative + periodic + TFD_TIMER_ABSTIME,
            // remaining time, disarm, and the EINVAL/EFAULT error surface. No macOS equivalent -> Linux-only oracle.
            src("timerfdx", "ext_timex/timerfdx.c")
                .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64])
                .oracle(),
        ],
    )
}
