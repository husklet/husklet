//! ipc — basics expansion (in-process JIT matrix). Owner: ipc agent. Edit ONLY this file.
//! Builders: src(name,file).oracle()/.exit()/.out()/.has(); port(name,file) for cross-engine golden.
//! Keep this module compiling at all times (`cargo build -p dd-tests`).
//!
//! Breadth over inter-process communication: anonymous pipes (sum/poll/EOF/EPIPE), FIFOs (non-blocking
//! open, multi-writer, two-way request/response), System V IPC (shm with IPC_STAT + RDONLY attach, a
//! 3-semaphore set with SETALL/semop/GETALL, typed message queues + ftok keys), POSIX shared memory as
//! a cross-process atomic counter, POSIX named semaphores across a fork, SCM_RIGHTS fd passing, AF_UNIX
//! dgram framing, dup'd-fd shared offsets, and advisory file locks (flock + lockf) across a fork.
//!
//! `port(...)` cases prove the IPC behaviour is byte-identical emulated-on-Linux and native-on-macOS.
//! A few Linux-only mechanisms (POSIX mq, eventfd, SOCK_SEQPACKET) are `src(...)` diffed vs the oracle.
#![allow(unused_imports)]
use crate::{fixture, group, in_rootfs, port, src, Case, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![ext_ipc()]
}

fn ext_ipc() -> Group {
    group("ext_ipc", vec![
        // ---- pipes ----
        port("pipe", "ext_ipc/ipc_pipe.c").out("pipe sum=2001000\n"),
        port("pipe-poll", "ext_ipc/ipc_pipe_poll.c").out("pipe_poll readable=1 got=X hup=1\n"),
        port("pipe-eof", "ext_ipc/ipc_pipe_eof.c").out("pipe_eof epipe=1 first=2 eof=0\n"),
        // ---- FIFOs ----
        port("fifo-nonblock", "ext_ipc/ipc_fifo_nonblock.c").out("fifo_nb enxio=1 rd_ok=1 wr_ok=1 data=hello\n"),
        port("fifo-multi-writer", "ext_ipc/ipc_fifo_multi_writer.c").out("fifo_mw sum=1001000 got=2000\n"),
        port("fifo-twoway", "ext_ipc/ipc_fifo_twoway.c").out("fifo_twoway sum=385\n"),
        // ---- System V IPC ----
        // shmctl(IPC_STAT).shm_segsz is wrong (< requested) on the arm64 JIT only; x86_64 + macOS OK.
        // The data round-trip itself is correct. xfail arm64; see GAPS "ext-shmstat-arm".
        port("sysv-shm", "ext_ipc/ipc_sysv_shm.c").out("sysv_shm size_ok=1 sum=523776 sum2=523776\n"),
        port("sysv-sem", "ext_ipc/ipc_sysv_sem.c").out("sysv_sem v0=3 v1=13 v2=10 all=3,13,10\n"),
        port("sysv-msg", "ext_ipc/ipc_sysv_msg.c").out("sysv_msg t2=type2 any=type1 t3=type3\n"),
        port("msgget-ftok", "ext_ipc/ipc_msgget_ftok.c").out("ftok key_ok=1 msg=ftok-msg\n"),
        // the dd-internal per-container SysV registry (was: the macOS host 32-slot table). Allocates
        // >32 shm segments concurrently, a cross-fork shmat shared-memory write, and cross-process BLOCKING
        // semop + msgsnd/msgrcv round-trips — every one of which the old host-backed path could not do
        // (shmmni=32, no cross-process shm). Confirmed byte-exact vs the native Linux oracle during bring-up;
        // pinned as a Linux-engine GOLDEN (not `.oracle()`) because the oracle would create 64 *native*
        // IPC_PRIVATE segments in the shared HOST SysV table, whose churn destabilises the concurrent
        // `sysv-ctl` badidx oracle (a `*_STAT(0x40000000)` maps via `idx % IPCMNI == 0` onto whatever host
        // object sits at index 0) — the very host-table contention removes on the dd side. Golden keeps
        // this to dd's own per-container registry (macOS host can't do >32, so darwin is excluded).
        port("sysv-stress", "ext_ipc/ipc_sysv_stress.c")
            .out("many_segs over32=1 allmapped=1 dataok=1\nxfork shm_shared=1 sem_blockwait=1 msg_roundtrip=1\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // ---- POSIX shm / sem ----
        port("posix-shm", "ext_ipc/ipc_posix_shm.c").out("posix_shm total=40000\n"),
        // /dev/shm functional contract real software (postgres DSM / parallel workers) needs: shm_open
        // round-trip persisting across close/reopen, MAP_SHARED coherence across fork (the DSM pattern), and
        // a named POSIX semaphore across fork. Portable POSIX -> golden on all engines.
        port("shm-dsm", "ext_ipc/shm_dsm.c").out("shm_dsm roundtrip=1 forkshared=1 sem=1\n"),
        // sem_open ENOENT under the JIT (same gap as threads/sem-named) — passes native-on-macOS.
        // xfail Linux; see GAPS "ext-sem-open".
        port("posix-sem-named", "ext_ipc/ipc_posix_sem_named.c").out("posix_sem_named c=5\n"),
        // ---- fd passing / unix dgram ----
        port("scm-rights", "ext_ipc/ipc_scm_rights.c").out("scm_rights data=passed-fd-content\n"),
        port("sockpair-dgram", "ext_ipc/ipc_sockpair_dgram.c").out("sockpair_dgram lens=242\n"),
        // ---- fd offset sharing ----
        port("dup-offset", "ext_ipc/ipc_dup_offset.c").out("dup_offset a=012 b=345\n"),
        // ---- advisory locks across fork ----
        port("flock-fork", "ext_ipc/ipc_flock_fork.c").out("flock child_blocked=1 child_acquired=1\n"),
        // the in-engine cross-process fcntl POSIX-lock manager. Two child processes serialize N
        // read-inc-write cycles under a whole-file F_SETLKW write lock (final==2*N, no lost updates),
        // F_GETLK sees a conflicting holder across processes, and flock<->fcntl stay independent.
        // Linux engines only: the flock<->fcntl independence (indep=1) is a Linux semantic the engine
        // emulates; native macOS (darwin) routes both through one vnode lock list, so it reports indep=0.
        port("poslk-xproc", "ext_ipc/ipc_poslk_xproc.c")
            .out("poslk final=400 noloss=1 getlk=1 indep=1\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // lockf() POSIX record-lock conflicts ARE enforced across processes (the child's F_TLOCK is
        // refused while the parent holds the lock, then acquired after release) — golden-identical on
        // both Linux engines and macOS.
        port("lockf-fork", "ext_ipc/ipc_lockf.c").out("lockf blocked=1 acquired=1\n"),
        // ---- SysV IPC errno/edge fidelity (LTP msgget/semget/shmget + *ctl) — diffed vs native ----
        // IPC_EXCL EEXIST on re-create, ENOENT for a missing key w/o IPC_CREAT, shm data+IPC_STAT size,
        // sem SETVAL/GETVAL/semop, msg IPC_NOWAIT ENOMSG + selective/any receive. Byte-exact vs native.
        src("sysv-edge", "ext_ipc/ipc_sysv_edge.c").oracle(),
        // ---- SysV IPC control-command surface: shmctl/semctl/msgctl full IPC_STAT/IPC_SET/*_INFO/
        // *_STAT + EINVAL/EFAULT/EACCES/EPERM. STAT round-trips (perms/nsems/qbytes/segsz), SET-then-STAT,
        // the INFO/index forms, and the errno paths — verdict-only (booleans/errno names vs our own
        // getuid), so root-dd and the unprivileged native oracle print byte-identically. Both Linux arches.
        src("sysv-ctl", "ext_ipc/sysv_ctl.c").oracle(),
        // ---- POSIX mqueue errno/edge fidelity (mq_open/timedsend/timedreceive/getattr) — diffed ----
        // O_CREAT|O_EXCL EEXIST, ENOENT w/o O_CREAT, ENAMETOOLONG, priority ordering, EMSGSIZE, O_NONBLOCK
        // EAGAIN, getattr maxmsg/msgsize/curmsgs, and the blocking mq_timed{send,receive} matrix
        // EINVAL(tv_nsec)/ETIMEDOUT. dd emulates POSIX mq in-process (macOS has no mqueue kernel).
        src("mq-edge", "ext_ipc/ipc_mq_edge.c").oracle(),
        // mq_notify register/EBUSY/unregister/EINVAL + SIGEV_SIGNAL SI_MESGQ delivery on the empty->non-empty
        // edge. aarch64-only: qemu-user's mq_notify is not a faithful oracle (it fails the SIGEV path), and
        // dd runs the SAME arch-normalized handler for both guest arches, so the real-kernel aarch64 diff
        // also covers the x86 path.
        src("mq-notify", "ext_ipc/ipc_mq_notify.c").oracle().only(&[Engine::LinuxAarch64]),
        // ---- Linux-only IPC (no portable POSIX form) — diffed vs native oracle ----
        // POSIX mq priority ordering. macOS has no mqueue kernel object, so dd emulates a named
        // in-process priority queue (rare.c) — byte-exact vs native here; errno edges in `mq-edge` above.
        src("mq", "ext_ipc/ipc_mq.c").oracle(),
        // eventfd counters ARE shared across fork (the child's writes reach the parent's object, so the
        // parent reads the accumulated count) — oracle-identical to native on both Linux engines.
        src("eventfd", "ext_ipc/ipc_eventfd.c").oracle(),
        // socketpair(AF_UNIX, SOCK_SEQPACKET) round-trips message-boundary datagrams — oracle-identical
        // to native on both Linux engines.
        src("seqpacket", "ext_ipc/ipc_seqpacket.c").oracle(),
        // SEQPACKET Mojo-IPC fidelity (chromium NodeChannel): no premature EOF when the parent drops the
        // child's fork-inherited pair end while keeping its own; SCM_RIGHTS fd passing over SEQPACKET; and
        // SO_PASSCRED -> a synthesized SCM_CREDENTIALS record (ucred.uid == getuid()). Diffed vs native.
        src("seqcred", "ext_ipc/ipc_seqcred.c").oracle(),
        src("seqpacket-flags", "ext_ipc/ipc_seqpacket_flags.c")
            .out("seqpacket_flags cloexec=1 nonblock=1 msg=1 recvclo=1 passcred=1\n"),
        src("seqpacket-epoll-drain", "ext_ipc/ipc_seqpacket_epoll_drain.c")
            .out("seqpacket_epoll_drain send=1 ep1=1 msg=1 cred=1 fd=1 clo=1 drained=1 quiet=1 child=0\n"),
        src("seqpacket-passcred-full", "ext_ipc/ipc_seqpacket_passcred_full.c")
            .out("seqpass_full n=5 data=mojo! trunc=0 ctrunc=0 rights=1 fdbyte=R cred=1 credpid=1 child=1\n"),
        src("seqpacket-passcred-ctrunc", "ext_ipc/ipc_seqpacket_passcred_ctrunc.c")
            .out("seqpass_ctrunc n=4 data=tiny trunc=0 ctrunc=0 controllen=32 cmsgs=1 rights=0 fdbyte=- cred=1 credpid=1 child=1\n"),
        // Wall-7 (multi-process Chrome content) cross-process Mojo-transport gates. The renderer is
        // launched over Mojo's fd transport and blocks on INBOUND delivery from the browser/GPU; these
        // isolate every primitive that delivery rides on across a REAL process boundary (not the
        // cross-thread / same-VA fork cases the older gates cover). All pass — establishing that dd's
        // cross-process epoll/eventfd/socketpair/SCM_RIGHTS/futex emulation is NOT the content-blank drop
        // point (see docs/rendering/README.md §3.2). Diffing directions matters: scm-eventfd has the parent
        // epoll + child write; these add the child-epoll + parent-write (dormant-renderer) direction.
        //
        // fork+execve child parked in epoll_wait; parent writes a socketpair message + signals an eventfd.
        src("xproc-inbound", "ext_ipc/ipc_xproc_inbound.c")
            .out("child woke sock=1 msg=BeginFrame efd=1 val=7\nparent done child_exit=0\n"),
        // The renderer LAUNCH path: a channel end + eventfd are handed to a pre-existing zygote via
        // SCM_RIGHTS, which FORKS the renderer that inherits them; the browser then writes inbound.
        src("zygote-inbound", "ext_ipc/ipc_zygote_inbound.c")
            .out("browser got V sock=1 msg=BeginFrame efd=1 val=7\n"),
        // Edge-triggered (EPOLLET) delivery of data another process buffered BEFORE the child registered
        // its epoll (the browser sends a bootstrap message before the child's message loop starts) — the
        // registration-time readiness prime must fire cross-process.
        src("xproc-prearm", "ext_ipc/ipc_xproc_prearm.c")
            .out("child prearm n=2 sock=1 msg=Invitation efd=1 val=5\nparent child_exit=0\n"),
        // SUSTAINED edge-triggered inbound: the xproc-inbound/prearm gates above deliver a SINGLE message;
        // these stream thousands of large (>2KB, invitation-sized) SEQPACKET messages, each planted in the
        // child pump's drain->re-arm window (child drains to EAGAIN, signals "drained", parent immediately
        // sends the next), so a readiness edge lost between drain and re-block -- the exact renderer-pump
        // lost-wakeup hypothesis -- parks the child and the in-guest watchdog fails it. EPOLLET (EV_CLEAR)
        // and EPOLLONESHOT (EV_ONESHOT + MOD re-arm) are distinct dd kqueue paths, so both are covered.
        port("pump-xproc-et", "ext_ipc/ipc_pump_xproc_et.c")
            .out("child stream got=4000/4000 ok=1\nparent done sent=4000 child_exit=0\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        port("pump-xproc-oneshot", "ext_ipc/ipc_pump_xproc_oneshot.c")
            .out("child oneshot got=4000/4000 ok=1\nparent done sent=4000 child_exit=0\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // In-process renderer park-state shape: an EPOLLET IO pump (SEQPACKET + ScheduleWork eventfd)
        // dispatches each inbound message to one of several worker threads parked in FUTEX_WAIT (condvar),
        // coupling the epoll-readiness wakeup to the pump->worker FUTEX handoff none of the above gates
        // combine. A missed wakeup on any leg stalls the pipeline; the watchdog makes that a hard fail.
        port("pump-worker-dispatch", "ext_ipc/ipc_pump_worker_dispatch.c")
            .out("renderer rounds=30000 done=30000 ok=1\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // WRITE-readiness (EPOLLOUT|EPOLLET) drain->re-arm on a SOCK_STREAM socketpair — the Mojo
        // PlatformChannel (SOCK_STREAM) write-watch path none of the EPOLLIN pump gates cover. A lost
        // writable edge parks the sender with a half-written message (the browser->renderer channel write
        // that never lands); the watchdog makes that a hard fail.
        port("pump-epollout-rearm", "ext_ipc/ipc_pump_epollout_rearm.c")
            .out("epollout_rearm sent=67108864 recv=67108864 ok=1\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // SHARED epoll instance across threads: a waiter blocks in epoll_wait while other threads
        // epoll_ctl-ADD already-ready fds — the exact cross-thread registration-edge case dd's W3E fast
        // path (ep_flush + EVFILT_USER NOTE_TRIGGER wake + g_ep_prime) serves and no other gate covers.
        // A lost cross-thread wake parks the waiter with a ready fd pending; the watchdog hard-fails it.
        port("epoll-shared-xthread", "ext_ipc/ipc_epoll_shared_xthread.c")
            .out("epoll_shared_xthread registered=32000 delivered=32000 ok=1\n")
            .only(&[Engine::LinuxAarch64, Engine::LinuxX86_64]),
        // SCM_RIGHTS-passed memfd -> cross-process FUTEX wake (renderer<->GPU command-buffer wakeup): the
        // futex-shared-key gate covers only a fork-INHERITED memfd; this covers one delivered by SCM_RIGHTS
        // to an unrelated process, mmap'd at an independent VA.
        src("scm-futex", "ext_ipc/ipc_scm_futex.c").out("scm_futex xproc_woke=1\n"),
        // SCM_RIGHTS-RECEIVED socket armed on the CHILD's OWN epoll, woken by a cross-process write to
        // the parent's retained peer end — the exact Mojo AcceptBrokerClient/AcceptInvitee step (a
        // node-channel control message carries an out-of-band platform socket handle; the child
        // recvmsg's it and installs the RECEIVED fd on its kqueue/epoll, then parks). The untested
        // permutation: scm-eventfd epolls a fd the PARENT created (an eventfd); zygote-inbound epolls a
        // FORK-INHERITED socket (recvmsg then fork, so kqueue arming rides fork, not the recvmsg fd);
        // scm-futex delivers an SCM_RIGHTS memfd but waits on FUTEX. Here the SAME process recvmsg's AND
        // epoll-arms the socket, so a failure to arm the host kqueue EVFILT_READ on a fd installed by the
        // recvmsg SCM_RIGHTS path (net.c → cmsg_m2l) parks the child forever; a watchdog makes that a
        // hard fail. ROUNDS repeats drain→re-block so a readiness edge lost on RE-ARM is also caught.
        src("scm-recv-epoll", "ext_ipc/ipc_scm_recv_epoll.c")
            .out("child recv-epoll rounds=64/64 ok=1\nparent done sent=64 child_exit=0\n"),
        // Chrome child-process Mojo BOOTSTRAP end-to-end (the multi-process dormant-renderer hypothesis):
        // the browser hands a launched child its platform channel + a shared-memory command buffer by
        // placing each fd at a fixed number and naming that number on the command line (Chrome's
        // --mojo-platform-channel-handle=N convention). This gate proves each bootstrap fd survives dd's
        // fork + IN-PLACE execve AT THE CMDLINE-NAMED NUMBER: the exec'd child receives the browser's
        // inbound channel message (chan), mmaps the memfd command buffer coherently in the NEW image
        // (shmem), is released by a cross-process FUTEX_WAKE on a word inside it (futex), while a sibling
        // FD_CLOEXEC alias is correctly swept by the same execve (decoy_swept -- so survival is by honouring
        // the cleared cloexec flag, not a no-op sweep). None of the xproc-inbound/zygote/scm-futex gates
        // carries a memfd across the in-place execve + validates the argv fd-number. See rendering README §3.2.
        src("bootstrap-handle", "ext_ipc/ipc_bootstrap_handle.c")
            .out("child bootstrap chan=1 msg=EstablishGpu shmem=1 futex=1 decoy_swept=1\nparent bootstrap child_exit=0\n"),
        src("scm-eventfd", "ext_ipc/ipc_scm_eventfd.c").out("scm_eventfd epoll=1 read=8 val=1 child=0\n"),
        src("scm-eventfd-untrusted", "ext_ipc/ipc_scm_eventfd.c")
            .out("scm_eventfd epoll=1 read=8 val=1 child=0\n")
            .untrusted(),
        src("scm-eventfd-dense", "ext_ipc/ipc_scm_eventfd_dense.c")
            .out("scm_eventfd_dense recv=48 trunc=0 woke=48 read=48 sum=1176 child=0\n"),
        src("scm-eventfd-dense-untrusted", "ext_ipc/ipc_scm_eventfd_dense.c")
            .out("scm_eventfd_dense recv=48 trunc=0 woke=48 read=48 sum=1176 child=0\n")
            .untrusted(),
        src("scm-memfd-seal", "ext_ipc/ipc_scm_memfd_seal.c")
            .out("scm_memfd_seal seal=1 send=1 child=0\n"),
        // SCM_CREDENTIALS peer-pid IDENTITY (chromium Mojo ports node-merge): two distinct children over
        // two SEQPACKET socketpairs must present two DISTINCT ucred.pids, neither equal to the receiver's own
        // pid. macOS reports the socketpair creator's pid on both ends, so before the synthetic-peer-id fix
        // the creating parent read its own pid for every child -> all collapsed to guest 1 (self-equal,
        // colliding). Booleans only, so native (real pids) and guest (synthetic ids) agree. Diffed vs native.
        src("credpid", "ext_ipc/ipc_credpid.c").oracle(),
        // SEQPACKET bystander-EOF guard (chromium Mojo child-bootstrap wall): a third process that inherits a
        // channel's SEND end across fork and closes it UNUSED must not inject a zero-length "EOF" datagram
        // into the live peer's recv queue -- the parent's first read on the retained end must be the real
        // 4-byte record, never a spurious 0. The old close-time EOF injection fired for any inherited end;
        // the fix only injects for an end this process actually wrote to. Diffed vs native.
        src("seqbystander", "ext_ipc/ipc_seqbystander.c").oracle(),
        // Chromium renderer bootstrap shape: the browser opens /proc/<renderer-pid>/{status,statm},
        // sends both read-only fds to the child over a Mojo-like AF_UNIX channel, and the child reads
        // them after sandboxing. This catches proc peer-file synthesis plus multi-fd SCM_RIGHTS in the
        // exact path Chrome hits before first Viz frame submission.
        src("chrome-procfd", "ext_ipc/ipc_chrome_procfd.c")
            .out("chrome_procfd open=1 send=1 recv=3 status=1 statm=1 maps=1 child=1\n"),
    ])
}
