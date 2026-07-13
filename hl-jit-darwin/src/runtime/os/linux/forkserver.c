// os/linux/forkserver.c -- W3D: resident "ddjitd" fork-server, SHARED by both Linux engines.
//
// WHY: per-launch wall is dominated by the irreducible per-process posix_spawn + dyld +
// codesign-validation floor of the engine ITSELF (opt8 measured ~2 ms of a ~3-5 ms launch), paid on
// EVERY container launch. A resident server pays dyld + engine init (code-cache arena, pthread key,
// signal handlers) + optional pre-translation ONCE, then fork()s a copy-on-write worker per launch.
// The worker inherits the warm translated arena + (optionally) the pre-loaded guest image COW, so a
// warm launch skips spawn + dyld + engine init + ELF load + translation entirely -- only the guest
// itself (and its container fd setup) is paid.
//
// HISTORY: this began as the x86-only translate/x86_64/forkserver.c (the W3D research diff).
// ported it to aarch64 by hoisting it here, parameterized by the SAME per-target seam the x86 W3D
// refactor already carved: container_init() / engine_global_init() / load_program() / run_loaded() /
// dd_run(), all defined by the including target TU before this file. ONE implementation, two engines.
//
// PROTOCOL v2 (AF_UNIX SOCK_STREAM; v2 extends the W3D research protocol with env/cwd + signal
// propagation so a forkserver launch is INDISTINGUISHABLE from a cold spawn):
//   client -> server:  [u32 magic 'DDF2'][u32 body_len] body, where body =
//                      [i32 argc][argc*(i32 len+bytes)][i32 envc][envc*(i32 len+bytes)][i32 cwdlen+bytes]
//                      (all strings NUL-terminated, lengths include the NUL), plus the client's
//                      {stdin,stdout,stderr} as 3 fds in an SCM_RIGHTS cmsg on the FIRST sendmsg.
//   server -> client:  [i32 runner_pid]   as soon as the guest process is forked, then
//                      [i32 wait_status]  the runner's RAW waitpid status when it terminates.
// The client forwards job-control-ish signals (INT/TERM/HUP/QUIT/USR1/USR2/WINCH) to runner_pid while
// it waits, and re-raises the runner's fatal signal on itself when WIFSIGNALED -- so ^C, `kill`, and a
// guest SIGSEGV all look exactly like a cold-spawned engine to the caller.
//
// PER-LAUNCH STATE: the worker is a double fork. The first child (the SUPERVISOR) owns the control
// connection: it forks the RUNNER, reports its pid, waitpid()s it, reports the raw status. The RUNNER
// is the guest process: it adopts the client's stdio (dup2), environ (wholesale) and cwd BEFORE
// running, closes every server-side fd (listener, control conn), resets the server's signal
// dispositions (SIGCHLD/SIGINT/SIGTERM/SIGPIPE back to SIG_DFL) and runs the guest EXACTLY like a
// standalone engine would -- including the normal proc.c exit_group _exit path (no g_noexit shim), so
// exit prints/hooks/codes are byte-identical. COW gives isolation: a runner's translations / data
// writes never touch the parent or sibling workers.
//
// CODE-CACHE MODE (preserved-arena fork): every runner calls jit_after_fork (engine/cache.c)
// right after fork, exactly like a guest fork does (proc.c fork_child_hooks). The dual-mapped RW/RX
// arena does NOT survive fork on its own -- the RX alias is VM_INHERIT_NONE (a hole in the child), and
// even the MAP_JIT fallback's two aliases COW-split independently -- so an inherited/fresh translation
// would execute stale/empty RX. jit_after_fork() re-remaps a fresh RX of the child's OWN COW RW pages
// at the SAME VA, keeping the ENTIRE prewarmed arena + block maps valid (single-threaded parent: ~1us
// preserve; threaded parent: conservative rebuild, child re-translates on demand). The fork-server thus
// reuses the exact machinery built for guest fork, instead of the old NODUALMAP single-mapping
// hack the x86 research forkserver relied on -- warm workers keep the full dual map with no W^X toggle.
//
// PCACHE DISCIPLINE (wave-2: fork children never save): every runner sets the never-save-from-
// fork-child latch (g_pcache_forked) before running -- its COW arena is the parent's prewarm mix, and
// persisting it under the request binary's identity would poison the cache. A guest execve inside the
// runner re-keys + lifts the bar via pcache_exec_reload (aarch64), exactly like any other fork child.
// The resident PARENT never calls pcache_save (prewarm uses run_loaded directly, not dd_run).
//
// CONFIG MODEL: engine-level env (JT/PROF/DDJIT_*/NODUALMAP...) and the container rootfs are SERVER
// launch config, read once at --server startup. Guest-visible env + the per-request container env the
// cold path re-parses (DDVOL/DD_NETNS/DD_CWD/DD_PUBLISH...) come from the CLIENT per request.

#include <sys/wait.h> // waitpid + W* status macros (also pulled in by sentry.c; idempotent)

extern char **environ;

// ---- full mapped span [base,base+span) of a loaded image, from its in-memory program headers.
// out->phdr = base + phoff and phnum/phent come from struct loaded; rd32/rd64 are the target's elf.c
// helpers (this file is #included after elf.c in the unity TU). Matches elf.c's span computation so
// the pristine-image snapshot/restore covers the identical region load_elf() mmap'd.
static uint64_t loaded_span(const struct loaded *L) {
    const uint8_t *ph = (const uint8_t *)L->phdr;
    uint64_t minv = ~0ull, maxv = 0;
    for (int i = 0; i < L->phnum; i++) {
        const uint8_t *p = ph + (size_t)i * (size_t)L->phent;
        if (rd32(p) != 1) continue; // PT_LOAD
        uint64_t v = rd64(p + 16), msz = rd64(p + 40);
        if (v < minv) minv = v;
        if (v + msz > maxv) maxv = v + msz;
    }
    uint64_t basepage = minv & ~0xFFFull;
    return (maxv - basepage + 0xFFFF) & ~0xFFFFull;
}

// ---- per-target seam knobs (define before #include to override) ----
#ifndef FSRV_SET_LOADBASE // x86 keeps a g_loadbase global the warm re-run must re-point
#define FSRV_SET_LOADBASE(b) ((void)0)
#endif
#ifndef FSRV_WARM_CHDIR_ROOTFS // x86's container model chdir()s the engine into the rootfs
#define FSRV_WARM_CHDIR_ROOTFS() ((void)0)
#endif
// Around the pristine-image restore memcpy: aarch64's load_elf applies per-segment W^X (.text R+X,
// .rodata R; and the prewarm guest may have mprotect()ed more, e.g. musl's RELRO) so the restore must
// open the span RW first and re-apply the PRISTINE load-time protections after. x86's loader keeps the
// whole guest image writable (guest code is only ever read by the translator there), so no-op default.
#ifndef FSRV_RESTORE_PREP
#define FSRV_RESTORE_PREP(L, span) ((void)0)
#endif
#ifndef FSRV_RESTORE_DONE
#define FSRV_RESTORE_DONE(L, span) ((void)0)
#endif

// ---- warm preload state (parent-side; inherited COW by every worker) ----
static int g_warm_ready;                      // set once the parent has pre-loaded + pre-translated g_wprog
static struct loaded g_wmain, g_winterp;      // parent-loaded guest image (same base in every COW worker)
static uint64_t g_wmain_span, g_winterp_span; // mapped span of each (for the pristine snapshot/restore)
static uint64_t g_wjump, g_wat_base;          // entry + AT_BASE for the warm re-run
static int g_whave_interp;
static void *g_wsnap_main, *g_wsnap_interp; // pristine copies of the writable image (data+bss)
static char g_wprog[1024];                  // the prewarmed program (warm path only fires on a match)
static char g_srv_rootfs[4200];

// Restore the pristine guest image (main + interp) over the current (dirtied) mapping, honoring the
// per-target segment-protection seam. Used by the server between prewarm runs and by every warm runner.
static void fsrv_restore_pristine(void) {
    FSRV_RESTORE_PREP(&g_wmain, g_wmain_span);
    memcpy((void *)g_wmain.base, g_wsnap_main, g_wmain_span);
    FSRV_RESTORE_DONE(&g_wmain, g_wmain_span);
    if (g_whave_interp) {
        FSRV_RESTORE_PREP(&g_winterp, g_winterp_span);
        memcpy((void *)g_winterp.base, g_wsnap_interp, g_winterp_span);
        FSRV_RESTORE_DONE(&g_winterp, g_winterp_span);
    }
}

// ---- SCM_RIGHTS fd passing ----
static int send_fds_msg(int sock, const void *buf, size_t len, const int *fds, int nfd) {
    struct iovec iov = {.iov_base = (void *)buf, .iov_len = len};
    char cbuf[CMSG_SPACE(sizeof(int) * 8)];
    memset(cbuf, 0, sizeof cbuf);
    struct msghdr mh;
    memset(&mh, 0, sizeof mh);
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    if (nfd > 0) {
        mh.msg_control = cbuf;
        mh.msg_controllen = CMSG_SPACE(sizeof(int) * nfd);
        struct cmsghdr *cm = CMSG_FIRSTHDR(&mh);
        cm->cmsg_level = SOL_SOCKET;
        cm->cmsg_type = SCM_RIGHTS;
        cm->cmsg_len = CMSG_LEN(sizeof(int) * nfd);
        memcpy(CMSG_DATA(cm), fds, sizeof(int) * nfd);
    }
    ssize_t r;
    do {
        r = sendmsg(sock, &mh, 0);
    } while (r < 0 && errno == EINTR);
    return r < 0 ? -1 : 0;
}

static int recv_fds_msg(int sock, void *buf, size_t len, int *fds, int *nfd) {
    struct iovec iov = {.iov_base = buf, .iov_len = len};
    char cbuf[CMSG_SPACE(sizeof(int) * 8)];
    struct msghdr mh;
    memset(&mh, 0, sizeof mh);
    mh.msg_iov = &iov;
    mh.msg_iovlen = 1;
    mh.msg_control = cbuf;
    mh.msg_controllen = sizeof cbuf;
    ssize_t r;
    do {
        r = recvmsg(sock, &mh, 0);
    } while (r < 0 && errno == EINTR);
    if (r < 0) return -1;
    *nfd = 0;
    for (struct cmsghdr *cm = CMSG_FIRSTHDR(&mh); cm; cm = CMSG_NXTHDR(&mh, cm)) {
        if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS) {
            int n = (int)((cm->cmsg_len - CMSG_LEN(0)) / sizeof(int));
            memcpy(fds, CMSG_DATA(cm), sizeof(int) * n);
            *nfd = n;
        }
    }
    return (int)r;
}

// send/recv exactly n bytes on a stream socket (EINTR-safe). 0 on success, -1 on error/EOF.
static int send_full(int sock, const void *buf, size_t n) {
    const char *p = buf;
    while (n) {
        ssize_t r = send(sock, p, n, 0);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) return -1;
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

static int recv_full(int sock, void *buf, size_t n) {
    char *p = buf;
    while (n) {
        ssize_t r = recv(sock, p, n, 0);
        if (r < 0 && errno == EINTR) continue;
        if (r <= 0) return -1;
        p += r;
        n -= (size_t)r;
    }
    return 0;
}

#define FSRV_MAGIC 0x32464444u // "DDF2" LE
#define FSRV_BUFSZ (256 * 1024)
#define FSRV_MAXARG 256
#define FSRV_MAXENV 1024

// Append one length-prefixed string-vector section [i32 n][n*(i32 len + bytes-with-NUL)] at out+*o.
// Returns 0 on success, -1 if it would overflow cap.
static int pack_strvec(char *out, size_t cap, size_t *o, int n, char *const v[]) {
    if (*o + 4 > cap) return -1;
    int32_t n32 = n;
    memcpy(out + *o, &n32, 4);
    *o += 4;
    for (int i = 0; i < n; i++) {
        int32_t l = (int32_t)strlen(v[i]) + 1; // include NUL
        if (*o + 4 + (size_t)l > cap) return -1;
        memcpy(out + *o, &l, 4);
        *o += 4;
        memcpy(out + *o, v[i], (size_t)l);
        *o += (size_t)l;
    }
    return 0;
}

// Parse one string-vector section into v[] (pointers into `in`; NULL-terminated). Returns the count
// (>= 0) and advances *o, or -1 on malformed/oversized input.
static int unpack_strvec(const char *in, size_t len, size_t *o, char **v, int max) {
    if (*o + 4 > len) return -1;
    int32_t n;
    memcpy(&n, in + *o, 4);
    *o += 4;
    if (n < 0 || n >= max) return -1;
    for (int i = 0; i < n; i++) {
        if (*o + 4 > len) return -1;
        int32_t l;
        memcpy(&l, in + *o, 4);
        *o += 4;
        if (l < 1 || *o + (size_t)l > len || in[*o + (size_t)l - 1] != 0) return -1;
        v[i] = (char *)in + *o;
        *o += (size_t)l;
    }
    v[n] = NULL;
    return n;
}

// ---- runner: the guest process (ONE fork off the resident server); never returns ----
// The SERVER keeps the control conn: it reports the runner pid right after fork and the raw wait
// status when kqueue (EVFILT_PROC NOTE_EXIT|NOTE_EXITSTATUS) reports the exit -- one fork per launch,
// not a supervisor pair (fork of the engine's large address space is THE marginal cost; see).
#define FSRV_MAXLIVE 256

static struct {
    pid_t pid;
    int conn;
} g_fsrv_live[FSRV_MAXLIVE]; // in-flight launches (server-side)

static int g_fsrv_ls = -1, g_fsrv_kq = -1;

static void ddjitd_runner(int conn, int *fds, int nfd, int argc, char **argv, char **envv, const char *cwd) {
    // Shed every server-side fd so nothing leaks into the guest's fd table: the listener, the kqueue
    // slot (kqueues are not inherited across fork, but the fd number is), every concurrently live
    // launch's control conn, and our OWN control conn (the server reports pid/status, not us).
    if (g_fsrv_ls >= 0) close(g_fsrv_ls);
    if (g_fsrv_kq >= 0) close(g_fsrv_kq);
    for (int i = 0; i < FSRV_MAXLIVE; i++)
        if (g_fsrv_live[i].pid && g_fsrv_live[i].conn >= 0) close(g_fsrv_live[i].conn);
    close(conn);
    // Reset the dispositions the resident parent installed (the SIGINT/SIGTERM stop handler,
    // SIGPIPE=SIG_IGN; SIGCHLD stays default in the server). A cold-spawned engine starts with
    // defaults; an inherited SIGINT handler would SWALLOW a forwarded ^C instead of killing the guest.
    signal(SIGCHLD, SIG_DFL);
    signal(SIGINT, SIG_DFL);
    signal(SIGTERM, SIG_DFL);
    signal(SIGPIPE, SIG_DFL);
    // the client's stdio onto 0/1/2 so guest tty-detection/reads/writes hit the client's terminal.
    if (nfd >= 1 && fds[0] != 0) dup2(fds[0], 0);
    if (nfd >= 2 && fds[1] != 1) dup2(fds[1], 1);
    if (nfd >= 3 && fds[2] != 2) dup2(fds[2], 2);
    for (int i = 0; i < nfd; i++)
        if (fds[i] > 2) close(fds[i]);
    // Adopt the client's environment WHOLESALE (guest envp is built from environ) + its cwd, so
    // `FOO=bar cd dir && ddjit --client ...` behaves exactly like the standalone engine invocation.
    static char *envvec[FSRV_MAXENV + 1];
    int ne = 0;
    for (; envv && envv[ne] && ne < FSRV_MAXENV; ne++)
        envvec[ne] = envv[ne];
    envvec[ne] = NULL;
    environ = envvec;
    if (cwd && cwd[0] && chdir(cwd) != 0) { /* client dir unreachable server-side: keep server cwd */
    }
    // W^X / APRR per-thread execute state is NOT reliably inherited across fork() on Apple Silicon
    // (see the proc.c fork path) -- re-assert RX so the first run_block can fetch executable code
    // (no-op under the dual map, which never toggles W^X).
    pthread_jit_write_protect_np(1);
    // preserved-arena fork: re-couple the dual map's RX alias to THIS child's COW RW pages at the
    // same VA (single-threaded parent: ~1us preserve; threaded parent: rebuild fresh), so the
    // COW-inherited warm arena + block maps stay valid and this runner executes real code instead of
    // stale/empty RX. Same hook a guest fork runs (proc.c fork_child_hooks); inheriting these warm
    // translations intact is the whole point of a warm worker. Also sheds the parent's thread registry
    // + reinits the engine locks a dead peer could have held (jit_after_fork).
    jit_after_fork();
    // wave-2 discipline: this process is a fork child on a COW copy of the PARENT's arena +
    // recording state -- it must NEVER pcache_save under the request binary's identity. A guest
    // execve re-keys + lifts the bar (pcache_exec_reload), same as any other fork child.
    g_pcache_forked = 1;
    g_noexit = 0; // the parent's prewarm shim must not leak in: guest exit_group _exits normally

    int warm = g_warm_ready && argc >= 1 && strcmp(argv[0], g_wprog) == 0;
    if (warm) {
        // Restore the pristine writable image over the COW-inherited (prewarm-dirtied) copy, so the
        // guest starts from byte-identical initial memory -- then re-run from the same entry/base.
        // The translated arena is already warm (COW), so run_guest finds every startup block mapped.
        fsrv_restore_pristine();
        FSRV_SET_LOADBASE(g_wmain.base);
        // per-request container identity: THIS process is the container init, not the server.
        if (g_srv_rootfs[0]) g_init_hostpid = getpid();
        // Re-anchor cross-process cgroup accounting to a FRESH slot table for THIS warm container: the
        // worker inherited the SERVER's table on fork, but each launch is its own container (state.c).
        if (g_srv_rootfs[0]) acct_container_reset();
        FSRV_WARM_CHDIR_ROOTFS();
        const char *icwd = getenv("DD_CWD"); // docker -w from the CLIENT's env
        if (icwd && icwd[0]) confine(icwd, g_cwd, sizeof g_cwd);
        _exit(run_loaded(argc, argv, &g_wmain, g_wjump, g_wat_base));
    }
    // Cold: no matching prewarm. Pay a full per-launch load + translate in the runner (still no
    // spawn/dyld/engine-init -- those were paid by the resident parent). container_init re-runs
    // against the CLIENT's env (DDVOL/DD_NETNS/DD_CWD...); engine_global_init is a no-op
    // (g_engine_inited). Translations are COW-private to this runner and discarded on exit.
    // A PREWARMED arena bars the persistent cache in a cold runner: pcache_load() restores over the
    // arena from offset 0 WITHOUT clearing the prewarm block map (stale entries would point into the
    // clobbered bytes), and the pcache fixed-VA image base (PC_IMG_BASE) is already occupied by the
    // prewarm image. DDJIT_NOPCACHE is the engine's own kill-switch and always wins inside dd_run.
    if (g_warm_ready) setenv("DDJIT_NOPCACHE", "1", 1);
    _exit(dd_run(g_srv_rootfs[0] ? g_srv_rootfs : NULL, argc, argv));
}

// ---- server ----
static volatile sig_atomic_t g_srv_stop;

static void srv_sigint(int s) {
    (void)s;
    g_srv_stop = 1;
}

static int ddjitd_server_main(int argc, char **argv) {
    const char *sock = NULL, *rootfs = NULL, *prewarm = NULL;
    for (int i = 1; i < argc; i++) {
        if (strcmp(argv[i], "--server") == 0 && i + 1 < argc)
            sock = argv[++i];
        else if (strcmp(argv[i], "--rootfs") == 0 && i + 1 < argc)
            rootfs = argv[++i];
        else if (strcmp(argv[i], "--prewarm") == 0 && i + 1 < argc)
            prewarm = argv[++i];
    }
    if (!sock) {
        fprintf(stderr, "usage: ddjit --server SOCK [--rootfs DIR] [--prewarm PROG]\n");
        return 2;
    }
    if (rootfs) snprintf(g_srv_rootfs, sizeof g_srv_rootfs, "%s", rootfs);

    // keep the default dual-mapped RW/RX arena. It doesn't survive fork on its own, but every
    // runner re-couples it via jit_after_fork() (see ddjitd_runner) -- the same preserved-arena hook a
    // guest fork uses -- so we get the fast no-W^X-toggle dual map AND correct COW inheritance, without
    // the old NODUALMAP single-mapping hack the x86 research forkserver needed.

    // Pay the expensive, per-launch-amortizable work ONCE.
    container_init(rootfs);
    if (engine_global_init()) return 1;

    if (prewarm && prewarm[0]) {
        snprintf(g_wprog, sizeof g_wprog, "%s", prewarm);
        // Load the guest image (this fixes the base every COW worker will share).
        load_program(prewarm, &g_wmain, &g_winterp, &g_wjump, &g_wat_base, &g_whave_interp);
        g_wmain_span = loaded_span(&g_wmain);
        if (g_whave_interp) g_winterp_span = loaded_span(&g_winterp);
        // Snapshot the PRISTINE writable image BEFORE running anything (data+bss are about to be
        // dirtied by the prewarm run; workers restore from these snapshots).
        g_wsnap_main = malloc(g_wmain_span);
        memcpy(g_wsnap_main, (void *)g_wmain.base, g_wmain_span);
        if (g_whave_interp) {
            g_wsnap_interp = malloc(g_winterp_span);
            memcpy(g_wsnap_interp, (void *)g_winterp.base, g_winterp_span);
        }
        // Pre-translate: run the program to completion in the PARENT so its blocks land in the COW
        // arena. g_noexit makes the guest's exit_group unwind run_guest instead of killing us. To
        // cover not just the shared ld.so+startup but the per-APPLET code paths too, we run a small
        // UNION of common busybox applets -- COW lets every later warm worker inherit all of them.
        // The pristine image is restored between runs so each applet starts from clean memory.
        int devnull = open("/dev/null", O_WRONLY);
        int sv1 = dup(1), sv2 = dup(2);
        if (devnull >= 0) {
            dup2(devnull, 1);
            dup2(devnull, 2);
        }
        g_noexit = 1;
        char pa0[1024];
        snprintf(pa0, sizeof pa0, "%s", prewarm);
        // Each row is an argv vector starting with the prewarmed program. Extra rows widen coverage;
        // the default set covers the launch-test workloads (true/echo/pwd/ls/cat/uname/sh).
        const char *applets[] = {NULL, "true", "echo", "pwd", "ls", "cat", "uname", "sh"};
        int nap = (int)(sizeof applets / sizeof applets[0]);
        for (int ai = 0; ai < nap; ai++) {
            // restore pristine before each prewarm run so it starts from clean guest memory
            fsrv_restore_pristine();
            char a1[256] = "x";
            char *pargv[4];
            int pac = 1;
            pargv[0] = pa0;
            if (applets[ai]) {
                pargv[pac++] = (char *)applets[ai];
                // give applets that need an operand a harmless one (echo x / ls / cat reads stdin=null)
                if (strcmp(applets[ai], "echo") == 0) pargv[pac++] = a1;
            }
            pargv[pac] = NULL;
            run_loaded(pac, pargv, &g_wmain, g_wjump, g_wat_base);
        }
        g_noexit = 0;
        if (devnull >= 0) {
            dup2(sv1, 1);
            dup2(sv2, 2);
            close(devnull);
        }
        close(sv1);
        close(sv2);
        g_warm_ready = 1;
        fprintf(stderr, "[ddjitd] prewarmed %s: arena=%lld KB\n", prewarm, (long long)((g_cp - g_cache) / 1024));
    }

    // SIGCHLD stays at the DEFAULT disposition: runners are reaped by the kqueue EVFILT_PROC handler
    // below (waitpid after NOTE_EXIT), so no SIG_IGN auto-reap -- an ignored SIGCHLD could race a
    // pid's disappearance before its EVFILT_PROC registration.
    // SIGINT/SIGTERM stop the event loop. Install WITHOUT SA_RESTART (BSD signal() defaults to
    // restarting): the handler must make the blocking kevent() return EINTR so the loop rechecks
    // g_srv_stop -- with SA_RESTART the server is unkillable by anything but SIGKILL.
    struct sigaction ssa;
    memset(&ssa, 0, sizeof ssa);
    ssa.sa_handler = srv_sigint;
    sigaction(SIGINT, &ssa, NULL);
    sigaction(SIGTERM, &ssa, NULL);
    signal(SIGPIPE, SIG_IGN);

    int ls = socket(AF_UNIX, SOCK_STREAM, 0);
    if (ls < 0) {
        perror("socket");
        return 1;
    }
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", sock);
    unlink(sock);
    if (bind(ls, (struct sockaddr *)&un, sizeof un) < 0) {
        perror("bind");
        return 1;
    }
    if (listen(ls, 128) < 0) {
        perror("listen");
        return 1;
    }
    fprintf(stderr, "[ddjitd] listening on %s (warm=%d rootfs=%s)\n", sock, g_warm_ready,
            g_srv_rootfs[0] ? g_srv_rootfs : "(none)");

    g_fsrv_ls = ls;
    for (int i = 0; i < FSRV_MAXLIVE; i++) {
        g_fsrv_live[i].pid = 0;
        g_fsrv_live[i].conn = -1;
    }
    int kq = kqueue();
    if (kq < 0) {
        perror("kqueue");
        return 1;
    }
    g_fsrv_kq = kq;
    struct kevent reg;
    EV_SET(&reg, ls, EVFILT_READ, EV_ADD, 0, 0, NULL);
    if (kevent(kq, &reg, 1, NULL, 0, NULL) < 0) {
        perror("kevent(listener)");
        return 1;
    }

    while (!g_srv_stop) {
        struct kevent ev;
        int ne = kevent(kq, NULL, 0, &ev, 1, NULL);
        if (ne < 0) {
            if (errno == EINTR) continue;
            perror("kevent");
            break;
        }
        if (ne == 0) continue;

        if (ev.filter == EVFILT_PROC) {
            // A runner terminated: reap it and report its RAW wait status on the saved control conn.
            // NOTE_EXITSTATUS puts a wait4-style status in ev.data; the waitpid result (which also
            // clears the zombie) is authoritative when available.
            int slot = (int)(intptr_t)ev.udata;
            int st = (int)ev.data, wst;
            if (waitpid((pid_t)ev.ident, &wst, WNOHANG) > 0) st = wst;
            if (slot >= 0 && slot < FSRV_MAXLIVE && g_fsrv_live[slot].pid == (pid_t)ev.ident) {
                if (g_fsrv_live[slot].conn >= 0) {
                    int32_t s32 = (int32_t)st;
                    (void)send_full(g_fsrv_live[slot].conn, &s32, 4);
                    close(g_fsrv_live[slot].conn); // also drops its EVFILT_READ registration
                }
                g_fsrv_live[slot].pid = 0;
                g_fsrv_live[slot].conn = -1;
            }
            continue;
        }
        if (ev.filter == EVFILT_READ && (int)ev.ident != ls) {
            // A live launch's control conn. EOF = the CLIENT died mid-run (e.g. SIGKILL); a standalone
            // engine would have died with it, so approximate parity by SIGHUPing the runner instead of
            // orphaning a guest forever. Anything else (stray client data) is drained and ignored.
            for (int i = 0; i < FSRV_MAXLIVE; i++)
                if (g_fsrv_live[i].pid && g_fsrv_live[i].conn == (int)ev.ident) {
                    if (ev.flags & EV_EOF) {
                        kill(g_fsrv_live[i].pid, SIGHUP);
                        close(g_fsrv_live[i].conn);
                        g_fsrv_live[i].conn = -1; // exit still reaps via EVFILT_PROC; nothing to report
                    } else {
                        char junk[256];
                        (void)recv(g_fsrv_live[i].conn, junk, sizeof junk, 0);
                    }
                    break;
                }
            continue;
        }
        if (ev.filter != EVFILT_READ) continue;

        int conn = accept(ls, NULL, NULL);
        if (conn < 0) {
            if (errno == EINTR) continue;
            perror("accept");
            break;
        }
        static char buf[FSRV_BUFSZ];
        int fds[8];
        int nfd = 0;
        // First recvmsg carries the 8-byte header (+ usually the whole body) and the SCM_RIGHTS fds;
        // a stream socket may split large bodies, so top up with plain recv until body_len arrives.
        int n = recv_fds_msg(conn, buf, sizeof buf, fds, &nfd);
        uint32_t magic = 0, blen = 0;
        int ok = n >= 8;
        if (ok) {
            memcpy(&magic, buf, 4);
            memcpy(&blen, buf + 4, 4);
            ok = magic == FSRV_MAGIC && blen <= sizeof buf - 8;
            if (ok && (size_t)n < 8 + (size_t)blen) ok = recv_full(conn, buf + n, 8 + (size_t)blen - (size_t)n) == 0;
        }
        char *wargv[FSRV_MAXARG + 1], *wenv[FSRV_MAXENV + 1];
        int wac = -1;
        size_t o = 8;
        const char *wcwd = "";
        if (ok) {
            wac = unpack_strvec(buf, 8 + blen, &o, wargv, FSRV_MAXARG);
            int wec = wac >= 1 ? unpack_strvec(buf, 8 + blen, &o, wenv, FSRV_MAXENV) : -1;
            if (wec < 0) wac = -1;
            if (wac >= 1) { // trailing [i32 cwdlen][cwd bytes]
                int32_t cl = 0;
                if (o + 4 <= 8 + blen) {
                    memcpy(&cl, buf + o, 4);
                    o += 4;
                    if (cl >= 1 && o + (size_t)cl <= 8 + blen && buf[o + (size_t)cl - 1] == 0)
                        wcwd = buf + o;
                    else if (cl != 0)
                        wac = -1;
                } else
                    wac = -1;
            }
        }
        int slot = -1;
        if (wac >= 1)
            for (int i = 0; i < FSRV_MAXLIVE; i++)
                if (!g_fsrv_live[i].pid) {
                    slot = i;
                    break;
                }
        if (wac < 1 || slot < 0) { // malformed request, or at capacity
            close(conn);
            for (int i = 0; i < nfd; i++)
                close(fds[i]);
            continue;
        }
        pid_t pid = fork();
        if (pid == 0) ddjitd_runner(conn, fds, nfd, wac, wargv, wenv, wcwd); // never returns
        for (int i = 0; i < nfd; i++)
            close(fds[i]); // ours were only for the runner
        int32_t p32 = (int32_t)(pid > 0 ? pid : -1);
        (void)send_full(conn, &p32, 4); // the client forwards ^C/kill to this pid while it waits
        if (pid < 0) {
            close(conn); // fork failure: EOF-without-status -> the client reports 125
            continue;
        }
        g_fsrv_live[slot].pid = pid;
        g_fsrv_live[slot].conn = conn;
        // Watch the conn for client-death EOF, and the runner for exit (+status). If the PROC
        // registration races an already-exited runner (ESRCH), fall back to a synchronous reap.
        EV_SET(&reg, conn, EVFILT_READ, EV_ADD, 0, 0, (void *)(intptr_t)slot);
        (void)kevent(kq, &reg, 1, NULL, 0, NULL);
        EV_SET(&reg, pid, EVFILT_PROC, EV_ADD | EV_ONESHOT, NOTE_EXIT | NOTE_EXITSTATUS, 0, (void *)(intptr_t)slot);
        if (kevent(kq, &reg, 1, NULL, 0, NULL) < 0) {
            int st = 127 << 8;
            pid_t r;
            do {
                r = waitpid(pid, &st, 0);
            } while (r < 0 && errno == EINTR);
            if (r < 0) st = 127 << 8;
            int32_t s32 = (int32_t)st;
            (void)send_full(conn, &s32, 4);
            close(conn);
            g_fsrv_live[slot].pid = 0;
            g_fsrv_live[slot].conn = -1;
        }
    }
    close(ls);
    unlink(sock);
    return 0;
}

// ---- client ----
static volatile pid_t g_fwd_pid; // the remote runner; forward received signals to it

static void fwd_sig(int s) {
    pid_t p = g_fwd_pid;
    if (p > 0) kill(p, s);
}

static int ddjitd_client_main(int argc, char **argv) {
    const char *sock = NULL;
    int ai = 1;
    while (ai < argc) {
        if (strcmp(argv[ai], "--client") == 0 && ai + 1 < argc) {
            sock = argv[ai + 1];
            ai += 2;
        } else if (strcmp(argv[ai], "--rootfs") == 0 && ai + 1 < argc) {
            ai += 2; // server already holds the rootfs; accept+ignore for CLI symmetry
        } else
            break;
    }
    if (!sock || ai >= argc) {
        fprintf(stderr, "usage: ddjit --client SOCK [--rootfs DIR] PROG [args...]\n");
        return 2;
    }
    int s = socket(AF_UNIX, SOCK_STREAM, 0);
    if (s < 0) {
        perror("socket");
        return 1;
    }
    struct sockaddr_un un;
    memset(&un, 0, sizeof un);
    un.sun_family = AF_UNIX;
    snprintf(un.sun_path, sizeof un.sun_path, "%s", sock);
    if (connect(s, (struct sockaddr *)&un, sizeof un) < 0) {
        perror("connect");
        return 1;
    }
    static char buf[FSRV_BUFSZ];
    size_t o = 8; // header written last (magic + body length)
    int nenv = 0;
    while (environ && environ[nenv])
        nenv++;
    if (nenv > FSRV_MAXENV) nenv = FSRV_MAXENV;
    char cwd[4096];
    if (!getcwd(cwd, sizeof cwd)) cwd[0] = 0;
    int32_t cl = cwd[0] ? (int32_t)strlen(cwd) + 1 : 0;
    if (pack_strvec(buf, sizeof buf, &o, argc - ai, argv + ai) != 0 ||
        pack_strvec(buf, sizeof buf, &o, nenv, environ) != 0 || o + 4 + (size_t)cl > sizeof buf) {
        fprintf(stderr, "ddjit --client: request too large\n");
        return 1;
    }
    memcpy(buf + o, &cl, 4);
    o += 4;
    if (cl) {
        memcpy(buf + o, cwd, (size_t)cl);
        o += (size_t)cl;
    }
    uint32_t magic = FSRV_MAGIC, blen = (uint32_t)(o - 8);
    memcpy(buf, &magic, 4);
    memcpy(buf + 4, &blen, 4);
    int fds[3] = {0, 1, 2};
    if (send_fds_msg(s, buf, o, fds, 3) < 0) {
        perror("sendmsg");
        return 1;
    }
    // 1st reply: the runner's pid -- forward job-control-ish signals to it while we wait, so ^C /
    // kill on the client behaves exactly as if the engine ran in this process.
    int32_t rpid = -1;
    if (recv_full(s, &rpid, 4) != 0 || rpid <= 0) {
        close(s);
        return 125; // server died / protocol error (matches the W3D client's error code)
    }
    g_fwd_pid = (pid_t)rpid;
    int fwd[] = {SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2, SIGWINCH};
    for (size_t i = 0; i < sizeof fwd / sizeof fwd[0]; i++)
        signal(fwd[i], fwd_sig);
    // 2nd reply: the runner's raw wait status. Reproduce it exactly: re-raise a fatal signal on
    // ourselves (so the caller's WIFSIGNALED view matches a cold spawn), else exit with its code.
    int32_t st = 0;
    if (recv_full(s, &st, 4) != 0) {
        close(s);
        return 125;
    }
    close(s);
    if (WIFSIGNALED(st)) {
        int sig = WTERMSIG(st);
        signal(sig, SIG_DFL);
        raise(sig);
        return 128 + sig; // unreachable for default-fatal signals; belt-and-braces
    }
    return WIFEXITED(st) ? WEXITSTATUS(st) : 125;
}
