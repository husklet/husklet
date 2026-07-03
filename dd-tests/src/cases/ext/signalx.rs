//! signalx — signal-delivery/control coverage (task #311). Owner: signalx-coverage agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! The signal surface beyond ext/posix's sigmask/killraise: alternate signal stacks (SA_ONSTACK),
//! SA_RESTART auto-restart vs EINTR, interval timers (setitimer/alarm), pause(), synchronous sigwait,
//! and SA_SIGINFO sender identification — all portable golden verdicts. Plus Linux-only tgkill (oracle).
#![allow(unused_imports)]
use crate::{group, src, port, fixture, in_rootfs, Case, Engine, Group};

pub fn groups() -> Vec<Group> { vec![signalx()] }

fn signalx() -> Group {
    group("ext-signal", vec![
        port("sigaltstack", "ext_sig/sigaltstack.c").out("sigaltstack set=1 ran=1 on_alt=1 query=1\n"),
        port("itimer", "ext_sig/itimer.c").out("itimer pending=1 fired=1 alarm=1\n"),
        port("pausesig", "ext_sig/pausesig.c").out("pausesig got=1 eintr=1\n"),
        // sigwait(): case 137 installs a host handler for each awaited signal lacking a guest handler so a
        // cross-process kill becomes pending, then dequeues it synchronously and returns the signo.
        port("sigwait", "ext_sig/sigwait.c").out("sigwait ok=1 clear=1\n"),
        // SA_RESTART: io.c restarts a signal-interrupted blocking read/write in place when the handler asked
        // for it (syscall_should_restart), and lets EINTR through otherwise.
        port("sarestart", "ext_sig/sarestart.c").out("sarestart restarted=1 eintr=1 handler=1\n"),
        // SA_SIGINFO si_pid/si_uid: the SA_SIGINFO host handler (host_sigh_si) captures the sender's pid/uid
        // and the sigframe stamps them at the _kill union offset.
        src("siginfo", "ext_sig/siginfo.c").oracle(),
        // Linux-only thread-directed signal -> native oracle
        src("tgkill", "ext_sig/tgkill.c").oracle(),
    ])
}
