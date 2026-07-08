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
        // lockf() POSIX record-lock conflicts aren't enforced across processes under the JIT (child's
        // F_TLOCK succeeds while the parent holds the lock) — flock() above works, macOS works. xfail
        // Linux; see GAPS "ext-lockf-fork".
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
        // eventfd counters aren't shared across fork under the JIT (child's writes don't reach the
        // parent's object → reads 0; native reads 100). xfail Linux; see GAPS "ext-eventfd-fork".
        src("eventfd", "ext_ipc/ipc_eventfd.c").oracle(),
        // socketpair(AF_UNIX, SOCK_SEQPACKET) returns -1 under the JIT → empty. xfail Linux; GAPS "ext-seqpacket".
        src("seqpacket", "ext_ipc/ipc_seqpacket.c").oracle(),
        // SEQPACKET Mojo-IPC fidelity (chromium NodeChannel): no premature EOF when the parent drops the
        // child's fork-inherited pair end while keeping its own; SCM_RIGHTS fd passing over SEQPACKET; and
        // SO_PASSCRED -> a synthesized SCM_CREDENTIALS record (ucred.uid == getuid()). Diffed vs native.
        src("seqcred", "ext_ipc/ipc_seqcred.c").oracle(),
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
    ])
}
