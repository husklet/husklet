//! signalx — signal-delivery/control coverage (task #311). Owner: signalx-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! The signal surface beyond ext/posix's sigmask/killraise: alternate signal stacks (SA_ONSTACK),
//! SA_RESTART auto-restart vs EINTR, interval timers (setitimer/alarm), pause(), synchronous sigwait,
//! and SA_SIGINFO sender identification — all portable golden verdicts. Plus Linux-only tgkill (oracle).
#![allow(unused_imports)]
use crate::{group, src, port, fixture, in_rootfs, Case, Engine, Group};

const LIN: &[Engine] = &[Engine::LinuxAarch64, Engine::LinuxX86_64];

pub fn groups() -> Vec<Group> { vec![signalx()] }

fn signalx() -> Group {
    group("ext-signal", vec![
        port("sigaltstack", "ext_sig/sigaltstack.c").out("sigaltstack set=1 ran=1 on_alt=1 query=1\n"),
        port("itimer", "ext_sig/itimer.c").out("itimer pending=1 fired=1 alarm=1\n"),
        port("pausesig", "ext_sig/pausesig.c").out("pausesig got=1 eintr=1\n"),
        // sigwait() produces no output on the Linux JIT (the synchronously-accepted signal is never
        // returned to the caller) — passes native-on-macOS. xfail Linux; GAPS `signalx-sigwait`.
        port("sigwait", "ext_sig/sigwait.c").out("sigwait ok=1 clear=1\n").xfail(LIN),
        // SA_RESTART is not honored on the Linux JIT: a signal-interrupted read returns EINTR instead of
        // auto-restarting (restarted=0) — passes native-on-macOS. xfail Linux; GAPS `signalx-sa-restart`.
        port("sarestart", "ext_sig/sarestart.c").out("sarestart restarted=1 eintr=1 handler=1\n").xfail(LIN),
        // SA_SIGINFO si_pid is not populated with the sender's pid on the Linux JIT (pid_match=0). macOS
        // uses a different si_code for cross-process kill, so this is Linux-only + oracle. xfail Linux;
        // GAPS `signalx-siginfo-sender`.
        src("siginfo", "ext_sig/siginfo.c").oracle().xfail(LIN),
        // Linux-only thread-directed signal -> native oracle
        src("tgkill", "ext_sig/tgkill.c").oracle(),
    ])
}
