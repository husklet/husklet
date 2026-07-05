//! Processes, threads, signals, and IPC & fd plumbing.

use crate::{group, port, src, src_nopie, Engine, Group};

/// Threads — mutex/condvar producer-consumer, 64-way contention, and thread-local storage. Portable
/// across engines (Linux x2 + macOS), golden-checked. Proves the threading model is sound everywhere.
pub(super) fn threads() -> Group {
    group(
        "threads",
        vec![
            port("mutex", "threads_mutex.c").out("queue produced=40000 consumed=40000\n"), // mutex + condvar
            port("contention", "threads_many.c").out("threads mutex=640000 atomic=640000\n"), // 64 threads
            port("tls", "tls.c").out("tls ok=8\n"), // __thread storage
            // TLS access models (LE/IE/GD/LD) x {main, spawned} threads, value survives alloc churn.
            port("tls-models", "tlsmodels.c").out("tlsmodels main=5 thread=5\n"),
            // The non-PIE ET_EXEC local-exec model clickhouse's DB::current_thread actually uses,
            // oracle-diffed on both Linux arches to prove the TP-relative address matches native exactly.
            // Main-thread only: the spawned-thread path is covered by the portable tls-models above, and a
            // spawned thread in a non-PIE ET_EXEC currently SEGVs on the x86_64 engine (see threads-nopie).
            src_nopie("tls-models-nopie", "tlsmodels_main.c").oracle(),
            // FIXED (was an x86_64 xfail): a spawned pthread inside a non-PIE ET_EXEC (static -no-pie) SEGV'd
            // on the x86 engine — rip-relative `lea` materialized the biased-HIGH address while baked pointers
            // (glibc's .tdata tcache sentinel &__tcache_dummy) sit at the LOW link address, so glibc's
            // thread-exit sentinel check diverged and freed the sentinel as a real chunk. Fixed by emitting the
            // low link address for low-image lea targets (x86 analogue of aarch64's adr/adrp un-biasing).
            src_nopie("threads-nopie", "threads_mutex.c")
                .out("queue produced=40000 consumed=40000\n"),
            // Minimized differential guard: a spawned thread that also reads a __thread var and a
            // -fstack-protector %fs:0x28 canary frame, byte-exact vs the qemu-x86_64 oracle (both arches).
            src_nopie("threads-nopie-tls-canary", "threads_nopie_tls.c").oracle(),
        ],
    )
}

/// IPC & fd plumbing — named pipes, POSIX + System V shared memory/semaphores, dup2 redirection,
/// fcntl flag commands. Portable across engines, golden-checked.
pub(super) fn ipc() -> Group {
    group(
        "ipc",
        vec![
            port("fifo", "mkfifo.c").out("fifo sum=125250\n"), // named pipe across fork
            port("shm-posix", "shmposix.c").out("shmposix sum=5559680\n"), // shm_open + MAP_SHARED
            port("shm-sysv", "sysvshm.c").out("sysvshm sum=131328\n"), // shmget/semget handshake
            port("dup2", "dup2redir.c").out("dup2 file=captured-line\n"), // dup/dup2 redirection
            port("fcntl", "fcntlflags.c").out("fcntl dupfd=1 cloexec=1 nonblock=1\n"), // F_DUPFD/SETFD/SETFL
        ],
    )
}

/// Real-Linux-correct decode of every waitcore.c case (see guests/waitcore.c). WCOREDUMP is set (core=1)
/// for core-dumping signals with RLIMIT_CORE>0, clear otherwise. Verified byte-exact vs a native aarch64 run.
const WAITCORE_OUT: &str = "\
quit-nocore signaled=1 term=3 core=0 expect=0 OK
quit signaled=1 term=3 core=1 expect=1 OK
abrt signaled=1 term=6 core=1 expect=1 OK
segv signaled=1 term=11 core=1 expect=1 OK
fpe signaled=1 term=8 core=1 expect=1 OK
ill signaled=1 term=4 core=1 expect=1 OK
bus signaled=1 term=7 core=1 expect=1 OK
trap signaled=1 term=5 core=1 expect=1 OK
sys signaled=1 term=31 core=1 expect=1 OK
kill signaled=1 term=9 core=0 expect=0 OK
term signaled=1 term=15 core=0 expect=0 OK
int signaled=1 term=2 core=0 expect=0 OK
exit exited=1 code=7 signaled=0
waitcore done
";

/// Process trees — fork/wait/exit-status propagation and parent<->child pipes. Portable across engines.
pub(super) fn proc() -> Group {
    group(
        "proc",
        vec![
            port("forkwait", "forkwait.c").out("forkwait reaped=8 sum=36\n"), // fork 8, reap, sum exit codes
            port("procreap", "procreap.c").oracle(), // fork/wait/exit-code + process-group teardown (kill(-pgid)) — parent MUST survive & exit-match native
            port("pipeproc", "pipeproc.c").out("pipeproc sum=500500\n"), // producer/consumer over a pipe
            // wait4/waitpid status must carry WCOREDUMP (0x80) exactly as Linux: core-dumping signal +
            // RLIMIT_CORE>0 sets it, non-core signals / zero limit clear it. Golden values are the REAL-Linux
            // truth (verified byte-exact vs the native aarch64 run); dd emits them identically on x86_64 too.
            // (Not oracle-diffed on x86_64: qemu-user doesn't reproduce WCOREDUMP for emulated fatal signals.)
            src("waitcore", "waitcore.c").out(WAITCORE_OUT),
            // Continuous dd==native proof on the arch whose oracle is a real Linux run (aarch64 executes bare).
            src("waitcore-oracle", "waitcore.c")
                .only(&[Engine::LinuxAarch64])
                .oracle(),
        ],
    )
}

/// Threads, signals, syscalls.
pub(super) fn system() -> Group {
    group(
        "system",
        vec![
            src("threads", "threads.c").out("threads sum=800000\n"),
            src("atomics", "atomics.c").out("atomic v=1000000\n"),
            src("signals", "signals.c").out("signal got=12\n"), // SIGUSR2 = 12
            src("sysinfo", "sysinfo.c").has("sys=Linux pid_ok=1"), // uname + getpid
            src("shm", "shm.c").out("SHM-ROUNDTRIP-OK\n"),      // SysV shared memory get/at/dt/ctl
            src("sem", "sem.c").out("SEM v=0 w=1\n"),           // SysV semaphores get/op/ctl
            src("msg", "msg.c").out("MSG=MSG-PAYLOAD\n"), // SysV message queues get/snd/rcv/ctl
        ],
    )
}
