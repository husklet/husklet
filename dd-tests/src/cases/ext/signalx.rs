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
        // #292 + IRQSLIM: an interval timer must preempt a syscall-free spin -- one direct-branch loop
        // (poll on the backward edge) and one computed-goto cycle (poll on the indirect entry). A lost
        // preemption hangs the guest -> harness timeout.
        port("sigspin", "ext_sig/sigspin.c").out("sigspin loop1=1 loop2=1\n"),
        port("pausesig", "ext_sig/pausesig.c").out("pausesig got=1 eintr=1\n"),
        // #397 (LTP pause01/pause02): EVERY caught signal delivered by kill(2) -- incl. the fault-class
        // SIGILL/SIGTRAP/SIGFPE/SIGSEGV/SIGBUS, which dd previously routed to its hardware-fault guard and
        // never woke pause() -- must wake pause() with -1/EINTR after the handler runs; SIGKILL is
        // un-catchable so pause() never returns and the process dies by SIGKILL. Diffed vs native.
        src("pausewake", "ext_sig/pausewake.c").oracle(),
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
