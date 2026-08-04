// #419 (extends #409) regression guard — non-PIE ET_EXEC pointer-arg rebase.
//
// Built STATIC NON-PIE (-static -no-pie) via src_nopie(), so the loader biases the image high and turns
// on dispatch.c's non-PIE g2h rebase switch (g_nonpie_lo != 0). EVERY pointer this program hands to a
// syscall is the address of a STATIC (.bss/.data) object — i.e. a LOW link vaddr on aarch64 (the guest
// computes it via ADRP; its memory *accesses* are folded high, but the raw pointer *value* stays low and
// is passed to the kernel as-is). Without the per-syscall rebase in dispatch.c, hl's native handler
// dereferences that unmapped low address and returns EFAULT (or SIGSEGV's the engine) on a perfectly
// valid pointer. On x86 the same pointer was already biased high by the loader/translator, so it passes
// either way — this program guards BOTH arches (one build per arch; the JIT normalizes x86 syscall
// numbers onto the aarch64-canonical ones the switch uses).
//
// The verdict per call is intentionally environment-INDEPENDENT: "ok" == "the valid pointer was NOT
// rejected with EFAULT". That is exactly the bug's signature, so a broken rebase flips a token to
// "EFAULT" (or crashes the run → empty/short output) and the .oracle() byte-compare vs native fails;
// meanwhile legitimate per-kernel differences (group counts, uid overlay, ENOSYS on an old oracle,
// EAGAIN/ETIMEDOUT, permissions) never diverge because they all count as "ok". Output is a single fixed
// line, so the native run (real kernel / qemu-x86_64) and all four matrix engines agree byte-for-byte.

#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <sys/ipc.h>
#include <sys/msg.h>
#include <sys/sem.h>
#include <sys/shm.h>
#include <sys/select.h>
#include <poll.h>
#include <linux/futex.h>

// "ok" iff the call did not fail with EFAULT on its (valid) low static pointer — the precise #419 bug.
#define TOK(name, rc) printf(" %s=%s", (name), ((long)(rc) >= 0 || errno != EFAULT) ? "ok" : "EFAULT")

// ---- all syscall pointer args below are addresses of THESE statics (low .bss/.data in a non-PIE) ----
static gid_t g_groups[64];
static uid_t g_ru, g_eu, g_su;
static gid_t g_rg, g_eg, g_sg;
static unsigned g_caphdr[2] = {0x20080522u, 0}; // {version=_LINUX_CAPABILITY_VERSION_3, pid=self}
static unsigned g_capdata[6];                    // cap_user_data_t[2] = 3 u32 x2
static sigset_t g_set, g_old, g_pend;
static struct sigaction g_act, g_oact;
static stack_t g_oss;
static siginfo_t g_si;
static struct timespec g_zero, g_tmo = {0, 5 * 1000 * 1000}; // 5ms for the futex/sigwait deadlines
static struct rlimit g_rl;
static cpu_set_t g_cpu;
static unsigned char g_schedattr[48];
static int g_status;
static struct rusage g_ru4;
static fd_set g_rfds;
static struct pollfd g_pfds[1];
static int g_futexw = 7;
static struct sembuf g_sops[1];
static unsigned short g_semarr[1] = {1};
static struct shmid_ds g_shmds;

// SysV msg buffer (mtype + payload), static so msgsnd/msgrcv get a low pointer.
static struct { long mtype; char mtext[8]; } g_msg = {1, "419abcd"}, g_msgr;

int main(void) {
    printf("nonpie419");

    // ---- credentials (proc.c: buffers written directly / guarded) ----
    errno = 0; TOK("getgroups", getgroups(64, g_groups));
    errno = 0; TOK("getresuid", getresuid(&g_ru, &g_eu, &g_su));
    errno = 0; TOK("getresgid", getresgid(&g_rg, &g_eg, &g_sg));
    errno = 0; TOK("capget", syscall(SYS_capget, g_caphdr, g_capdata));

    // ---- rt_signal family (signal.c + rt_tgsigqueueinfo in rare.c) ----
    sigemptyset(&g_set); sigaddset(&g_set, SIGUSR1);
    errno = 0; TOK("sigprocmask", sigprocmask(SIG_BLOCK, &g_set, &g_old)); // SIGUSR1 now blocked
    errno = 0; TOK("sigpending", sigpending(&g_pend));
    g_act.sa_handler = SIG_IGN;
    errno = 0; TOK("sigaction", sigaction(SIGUSR1, &g_act, &g_oact));
    errno = 0; TOK("sigaltstack", sigaltstack(NULL, &g_oss));            // query current alt stack
    errno = 0; TOK("sigtimedwait", sigtimedwait(&g_set, &g_si, &g_zero)); // no pending -> EAGAIN (not EFAULT)
    errno = 0; TOK("rt_sigqueueinfo", syscall(SYS_rt_sigqueueinfo, getpid(), SIGUSR1, &g_si));
    errno = 0; TOK("rt_tgsigqueueinfo",
                   syscall(SYS_rt_tgsigqueueinfo, getpid(), syscall(SYS_gettid), SIGUSR1, &g_si));

    // ---- sched / rlimit / wait ----
    errno = 0; TOK("getrlimit", getrlimit(RLIMIT_NOFILE, &g_rl));
    errno = 0; TOK("setrlimit", setrlimit(RLIMIT_NOFILE, &g_rl)); // set-back the value just read
    errno = 0; TOK("sched_getaffinity", sched_getaffinity(0, sizeof g_cpu, &g_cpu));
    errno = 0; TOK("sched_getattr", syscall(SYS_sched_getattr, 0, g_schedattr, sizeof g_schedattr, 0));
    // futex on a low .bss word + a low .bss timespec: *word==val so it blocks, then times out (ETIMEDOUT).
    errno = 0; TOK("futex", syscall(SYS_futex, &g_futexw, FUTEX_WAIT, g_futexw, &g_tmo));

    // ---- poll / select on low static fd_set / pollfd / timespec ----
    {
        int pfd[2];
        if (pipe(pfd) == 0) {
            (void)!write(pfd[1], "x", 1);
            FD_ZERO(&g_rfds); FD_SET(pfd[0], &g_rfds);
            errno = 0; TOK("pselect", pselect(pfd[0] + 1, &g_rfds, NULL, NULL, &g_tmo, NULL));
            g_pfds[0].fd = pfd[0]; g_pfds[0].events = POLLIN;
            errno = 0; TOK("ppoll", ppoll(g_pfds, 1, &g_tmo, NULL));
            close(pfd[0]); close(pfd[1]);
        }
    }

    // ---- wait4: status + rusage written into low statics ----
    {
        fflush(stdout); // don't let the child inherit our buffered line (it _exit()s without flushing anyway)
        pid_t k = fork();
        if (k == 0) _exit(0);
        errno = 0; TOK("wait4", wait4(k, &g_status, 0, &g_ru4));
    }

    // ---- SysV IPC: msg / sem / shm pointers are all low statics ----
    {
        int q = msgget(IPC_PRIVATE, 0600);
        if (q >= 0) {
            errno = 0; TOK("msgsnd", msgsnd(q, &g_msg, sizeof g_msg.mtext, 0));
            errno = 0; TOK("msgrcv", msgrcv(q, &g_msgr, sizeof g_msgr.mtext, 0, 0));
            msgctl(q, IPC_RMID, NULL);
        }
        int s = semget(IPC_PRIVATE, 1, 0600);
        if (s >= 0) {
            errno = 0; TOK("semctl_setall", semctl(s, 0, SETALL, g_semarr));
            g_sops[0].sem_num = 0; g_sops[0].sem_op = 1; g_sops[0].sem_flg = 0;
            errno = 0; TOK("semop", semop(s, g_sops, 1));
            g_sops[0].sem_op = -1;
            errno = 0; TOK("semtimedop", syscall(SYS_semtimedop, s, g_sops, 1, &g_tmo));
            semctl(s, 0, IPC_RMID);
        }
        int h = shmget(IPC_PRIVATE, 4096, 0600);
        if (h >= 0) {
            errno = 0; TOK("shmctl", shmctl(h, IPC_STAT, &g_shmds));
            shmctl(h, IPC_RMID, NULL);
        }
    }

    printf("\n");
    return 0;
}
