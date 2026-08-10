// linux_abi/fork.c -- W3D: resident engine server fork-server, SHARED by both Linux engines.

#include "../core/engine_result.h"

#include <errno.h>
#include <fcntl.h>
#include "host_fd.h"
#include "host_poll.h"
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "host_socket.h"
#include "host_socket.h"
#include "host_wait.h"
#include <unistd.h>

#include "../host/child.h"
#include "fork_codec.h"
#include "../host/fork_wire.h"
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
// hl_run_linux_guest(), all defined by the including target TU before this file. One implementation, two engines.
//
// PROTOCOL v2 (AF_UNIX SOCK_STREAM; v2 extends the W3D research protocol with env/cwd + signal
// propagation so a forkserver launch is INDISTINGUISHABLE from a cold spawn):
//   client -> server:  [u32 magic 'HLF3'][u32 body_len] body, where body =
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
// runner re-keys persistent-cache lookup; AArch64 keeps the runner barred from publishing while x86
// applies its own exec-epoch policy.
// The resident parent never calls pcache_save; prewarm uses the loaded-program path directly.
//
// Config model: engine-level HL options and the container rootfs are server
// launch config, read once at --server startup. Guest-visible env + the per-request container env the
// Cold-path launch options such as volumes, namespace, cwd, and published ports come from each client request.

#include "host_wait.h" // waitpid + W* status macros (also pulled in by sentry.c; idempotent)

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
// Around the pristine-image restore memcpy. Both loaders apply per-segment W^X (.text R+X, .rodata R)
// and the prewarm guest may have narrowed more (musl's RELRO), so the restore opens the span RW, copies,
// then re-applies the PRISTINE load-time protections AND read-only registry -- through the loaders' own
// hl_elf_protect_segments, so "pristine" means the same thing on both sides of the fork. Without the
// re-open the first restore memcpy faults on the now-R+X .text.
static void fsrv_restore_prep(const struct loaded *L, uint64_t span) {
    (void)mprotect((void *)(uintptr_t)L->base, (size_t)span, PROT_READ | PROT_WRITE);
}

static void fsrv_restore_done(const struct loaded *L, uint64_t span) {
    (void)span;
    const uint8_t *ph = (const uint8_t *)L->phdr;
    uint64_t minv = ~0ull;
    for (int i = 0; i < L->phnum; i++) {
        const uint8_t *p = ph + (size_t)i * (size_t)L->phent;
        if (hl_elf_ph32(p) != 1) continue; // PT_LOAD
        uint64_t v = hl_elf_ph64(p + 16);
        if (v < minv) minv = v;
    }
    hl_elf_protect_segments(NULL, ph, L->phnum, L->phent, L->base - (minv & ~0xFFFull)); // load_elf's bias
}

#ifndef FSRV_RESTORE_PREP
#define FSRV_RESTORE_PREP(L, span) fsrv_restore_prep((L), (span))
#endif
#ifndef FSRV_RESTORE_DONE
#define FSRV_RESTORE_DONE(L, span) fsrv_restore_done((L), (span))
#endif
// Per-guest host-service table init that hl_run_linux_guest() performs before it runs a guest (the
// futex / seq-ref / eventfd-counter / fdvis / task-state SHARED arenas). The prewarm path runs guests via
// run_loaded() directly and thus BYPASSES that init; without it an eventfd guest dereferences the still-NULL
// g_eventfd_count and SIGSEGVs the resident parent (and other table-consulting paths silently misbehave in a
// warm runner). These arenas are process-shared (MAP_SHARED) and each init is idempotent (early-returns once
// bound), so binding them ONCE in the parent -- exactly the constructor model the tables document -- lets
// every COW worker inherit the same physical arenas. Defined by the including target TU (aarch64/x86_64).
#ifndef FSRV_GUEST_HOST_INIT
#define FSRV_GUEST_HOST_INIT() ((void)0)
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

#define FSRV_MAGIC UINT32_C(0x33464c48) // "HLF3" (LE)
#define FSRV_BUFSZ (256 * 1024)
#define FSRV_MAXARG 256
#define FSRV_MAXENV 1024

// Append one length-prefixed string-vector section [i32 n][n*(i32 len + bytes-with-NUL)] at out+*o.
// Returns 0 on success, -1 if it would overflow cap.
// ---- runner: the guest process (ONE fork off the resident server); never returns ----
// The SERVER keeps the control conn: it reports the runner pid right after fork and the raw wait
// status when the portable child-wake loop reaps it -- one fork per launch,
// not a supervisor pair (fork of the engine's large address space is THE marginal cost; see).
#define FSRV_MAXLIVE 256

static struct {
    pid_t pid;
    int conn;
} g_fsrv_live[FSRV_MAXLIVE]; // in-flight launches (server-side)

static int g_fsrv_ls = -1;
static hl_host_child_watch g_fsrv_wake = {.read_descriptor = -1, .write_descriptor = -1};

static int hl_forkserver_guest_environment(char *const envv[]) {
    size_t size = 1;
    size_t offset = 0;
    char *serialized;
    int index;
    for (index = 0; envv != NULL && envv[index] != NULL; index++) {
        size_t length = strlen(envv[index]);
        if (length > (SIZE_MAX - size - 1) / 2) return -1;
        size += length * 2 + 1;
    }
    serialized = (char *)malloc(size);
    if (serialized == NULL) return -1;
    for (index = 0; envv != NULL && envv[index] != NULL; index++) {
        const char *input = envv[index];
        while (*input != 0) {
            if (*input == '\\' || *input == '\n') serialized[offset++] = '\\';
            serialized[offset++] = *input == '\n' ? 'n' : *input;
            input++;
        }
        serialized[offset++] = '\n';
    }
    serialized[offset] = 0;
    if (hl_option_set("HL_GUEST_ENV", serialized, 1) != 0) {
        free(serialized);
        return -1;
    }
    free(serialized);
    if (hl_option_set("HL_GUEST_ENV_ESC", "1", 1) != 0 || hl_option_set("HL_GUEST_ENV_EXACT", "1", 1) != 0) return -1;
    return 0;
}

static void hl_forkserver_runner(int conn, int *fds, int nfd, int argc, char **argv, char **envv, const char *cwd) {
    hl_engine_child_result_after_fork();
    // Shed every server-side fd so nothing leaks into the guest's fd table: the listener, the child-wake
    // pipe, every concurrently live
    // launch's control conn, and our OWN control conn (the server reports pid/status, not us).
    if (g_fsrv_ls >= 0) close(g_fsrv_ls);
    if (g_fsrv_wake.read_descriptor >= 0) close(g_fsrv_wake.read_descriptor);
    if (g_fsrv_wake.write_descriptor >= 0) close(g_fsrv_wake.write_descriptor);
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
    // The client environment is explicit guest data. It never becomes the engine process environment.
    if (hl_forkserver_guest_environment(envv) != 0) _exit(125);
    if (cwd && cwd[0] && chdir(cwd) != 0) { /* client dir unreachable server-side: keep server cwd */
    }
    // W^X / APRR per-thread execute state is NOT reliably inherited across fork() on Apple Silicon
    // (see the proc.c fork path) -- re-assert RX so the first run_block can fetch executable code
    // (no-op under the dual map, which never toggles W^X).
    if (!jit_wprot(1)) _exit(70);
    // preserved-arena fork: re-couple the dual map's RX alias to THIS child's COW RW pages at the
    // same VA (single-threaded parent: ~1us preserve; threaded parent: rebuild fresh), so the
    // COW-inherited warm arena + block maps stay valid and this runner executes real code instead of
    // stale/empty RX. Same hook a guest fork runs (proc.c fork_child_hooks); inheriting these warm
    // translations intact is the whole point of a warm worker. Also sheds the parent's thread registry
    // + reinits the engine locks a dead peer could have held (jit_after_fork).
    // Only a WARM runner (same binary as the prewarm) may inherit the parent's block map: a cold
    // runner running a DIFFERENT guest would mis-dispatch the prewarmed binary's cached blocks at
    // colliding guest PCs. Warm runners get preserve=1 (inherit the prewarmed arena, skip re-translate);
    // cold runners keep the proven preserve=0 fresh-arena path.
    int warm = g_warm_ready && argc >= 1 && strcmp(argv[0], g_wprog) == 0;
    g_fsrv_preserve = warm; // inherit the resident parent's prewarmed arena (warm fork-server runner only)
    int fsrv_pres_ok = jit_after_fork();
    g_fsrv_preserve = 0; // a GUEST fork inside this runner must use the preserve=0 nested-fork cure
    if (!fsrv_pres_ok) _exit(70);
    // wave-2 discipline: this process is a fork child on a COW copy of the PARENT's arena +
    // recording state -- it must NEVER pcache_save under the request binary's identity. A guest
    // execve re-keys persistent-cache lookup and applies the frontend's exec-epoch save policy.
    g_pcache_forked = 1;
    g_noexit = 0; // the parent's prewarm shim must not leak in: guest exit_group _exits normally

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
        if (g_srv_rootfs[0]) acct_container_reset(effective_host_services());
        FSRV_WARM_CHDIR_ROOTFS();
        const char *icwd = hl_option_get("HL_CWD"); // docker -w from the CLIENT's env
        if (icwd && icwd[0]) confine(icwd, g_cwd, sizeof g_cwd);
        _exit(run_loaded(argc, argv, &g_wmain, g_wjump, g_wat_base));
    }
    // Cold: no matching prewarm. Pay a full per-launch load + translate in the runner (still no
    // spawn/dyld/engine-init -- those were paid by the resident parent). container_init re-runs
    // against the client's centralized HL options; engine_global_init is a no-op
    // (g_engine_inited). Translations are COW-private to this runner and discarded on exit.
    // A PREWARMED arena bars the persistent cache in a cold runner: pcache_load() restores over the
    // arena from offset 0 WITHOUT clearing the prewarm block map (stale entries would point into the
    // clobbered bytes), and the pcache fixed-VA image base (PC_IMG_BASE) is already occupied by the
    // prewarm image. Persistent cache activation is removed in this short-lived cold runner.
    if (g_warm_ready) {
        hl_option_unset("HL_PCACHE");
        hl_option_unset("HL_PCACHE_DIR");
    }
    _exit(hl_run_linux_guest(hl_target_services_effective(&g_target_services), g_linux_box,
                             g_srv_rootfs[0] ? g_srv_rootfs : NULL, HL_HOST_HANDLE_INVALID, NULL, 0, (uint32_t)argc,
                             argv));
}

// ---- server ----
static volatile sig_atomic_t g_srv_stop;

static void srv_sigint(int s) {
    (void)s;
    g_srv_stop = 1;
    hl_host_child_watch_notify(&g_fsrv_wake);
}

static int hl_server_main(int argc, char **argv) {
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
        fprintf(stderr, "usage: hl-engine --server SOCK [--rootfs DIR] [--prewarm PROG]\n");
        return 2;
    }
    if (rootfs) snprintf(g_srv_rootfs, sizeof g_srv_rootfs, "%s", rootfs);

    // keep the default dual-mapped RW/RX arena. It doesn't survive fork on its own, but every
    // runner re-couples it via jit_after_fork() (see hl_forkserver_runner) -- the same preserved-arena hook a
    // guest fork uses -- so we get the fast no-W^X-toggle dual map AND correct COW inheritance, without
    // the old NODUALMAP single-mapping hack the x86 research forkserver needed.

    // Pay the expensive, per-launch-amortizable work ONCE.
    if (hl_target_services_bind(&g_target_services) != 0) return 1;
    hl_target_services_inject(&g_target_services, hl_target_services_bound(&g_target_services));
    hl_gmap_bind_host(hl_target_services_effective(&g_target_services));
    if (container_init(rootfs) != 0) return 1;
    if (engine_global_init()) return 1;

    if (prewarm && prewarm[0]) {
        snprintf(g_wprog, sizeof g_wprog, "%s", prewarm);
        // Bind the per-guest host-service tables (eventfd counter, futex, seq-ref, fdvis, task-state) in
        // the resident parent BEFORE running any guest. The prewarm run below goes through run_loaded()
        // rather than hl_run_linux_guest(), which is where a cold launch does this init -- so without it an
        // eventfd guest NULL-derefs g_eventfd_count and crashes the parent. The arenas are MAP_SHARED and
        // created once, so every warm COW worker inherits them.
        FSRV_GUEST_HOST_INIT();
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
        int devnull = open(HL_LINUX_HOST_NULL_DEVICE, O_WRONLY);
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
        fprintf(stderr, "[hl-engine-server] prewarmed %s: arena=%lld KB\n", prewarm,
                (long long)((g_cp - g_cache) / 1024));
    }

    // A nonblocking self-pipe turns SIGCHLD into ordinary poll readiness without losing the race between
    // reaping and blocking. This is the same portable mechanism on macOS and Linux.
    if (hl_host_child_watch_init(&g_fsrv_wake) != 0) {
        perror("child watch");
        return 1;
    }
    struct sigaction ssa;
    memset(&ssa, 0, sizeof ssa);
    sigemptyset(&ssa.sa_mask);
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
    fprintf(stderr, "[hl-engine-server] listening on %s (warm=%d rootfs=%s)\n", sock, g_warm_ready,
            g_srv_rootfs[0] ? g_srv_rootfs : "(none)");

    g_fsrv_ls = ls;
    for (int i = 0; i < FSRV_MAXLIVE; i++) {
        g_fsrv_live[i].pid = 0;
        g_fsrv_live[i].conn = -1;
    }
    while (!g_srv_stop) {
        struct pollfd watched[FSRV_MAXLIVE + 2];
        int slots[FSRV_MAXLIVE + 2];
        nfds_t watched_count = 2;
        watched[0] = (struct pollfd){.fd = ls, .events = POLLIN};
        watched[1] = (struct pollfd){.fd = hl_host_child_watch_descriptor(&g_fsrv_wake), .events = POLLIN};
        slots[0] = slots[1] = -1;
        for (int i = 0; i < FSRV_MAXLIVE; i++)
            if (g_fsrv_live[i].pid && g_fsrv_live[i].conn >= 0) {
                watched[watched_count] =
                    (struct pollfd){.fd = g_fsrv_live[i].conn, .events = POLLIN | POLLHUP | POLLERR};
                slots[watched_count++] = i;
            }
        int ne = poll(watched, watched_count, -1);
        if (ne < 0) {
            if (errno == EINTR) continue;
            perror("poll");
            break;
        }
        if (ne == 0) continue;
        if (watched[1].revents != 0) { hl_host_child_watch_drain(&g_fsrv_wake); }
        for (int i = 0; i < FSRV_MAXLIVE; i++)
            if (g_fsrv_live[i].pid) {
                int status;
                pid_t reaped = waitpid(g_fsrv_live[i].pid, &status, WNOHANG);
                if (reaped == g_fsrv_live[i].pid) {
                    if (g_fsrv_live[i].conn >= 0) {
                        int32_t status32 = (int32_t)status;
                        (void)hl_fork_wire_send(g_fsrv_live[i].conn, &status32, 4);
                        close(g_fsrv_live[i].conn);
                    }
                    g_fsrv_live[i].pid = 0;
                    g_fsrv_live[i].conn = -1;
                }
            }
        for (nfds_t index = 2; index < watched_count; ++index) {
            int slot = slots[index];
            if (watched[index].revents == 0 || slot < 0 || !g_fsrv_live[slot].pid ||
                g_fsrv_live[slot].conn != watched[index].fd)
                continue;
            if ((watched[index].revents & (POLLHUP | POLLERR | POLLNVAL)) != 0) {
                kill(g_fsrv_live[slot].pid, SIGHUP);
                close(g_fsrv_live[slot].conn);
                g_fsrv_live[slot].conn = -1;
            } else if ((watched[index].revents & POLLIN) != 0) {
                char junk[256];
                ssize_t received = recv(g_fsrv_live[slot].conn, junk, sizeof junk, 0);
                if (received == 0) {
                    kill(g_fsrv_live[slot].pid, SIGHUP);
                    close(g_fsrv_live[slot].conn);
                    g_fsrv_live[slot].conn = -1;
                }
            }
        }
        if ((watched[0].revents & POLLIN) == 0) continue;

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
        int n = hl_fork_wire_receive_descriptors(conn, buf, sizeof buf, fds, &nfd);
        uint32_t magic = 0, blen = 0;
        int ok = n >= 8;
        if (ok) {
            memcpy(&magic, buf, 4);
            memcpy(&blen, buf + 4, 4);
            ok = magic == FSRV_MAGIC && blen <= sizeof buf - 8;
            if (ok && (size_t)n < 8 + (size_t)blen)
                ok = hl_fork_wire_receive(conn, buf + n, 8 + (size_t)blen - (size_t)n) == 0;
        }
        char *wargv[FSRV_MAXARG + 1], *wenv[FSRV_MAXENV + 1];
        int wac = -1;
        size_t o = 8;
        const char *wcwd = "";
        if (ok) {
            wac = hl_fork_wire_unpack_strings(buf, 8 + blen, &o, wargv, FSRV_MAXARG);
            int wec = wac >= 1 ? hl_fork_wire_unpack_strings(buf, 8 + blen, &o, wenv, FSRV_MAXENV) : -1;
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
        if (pid == 0) hl_forkserver_runner(conn, fds, nfd, wac, wargv, wenv, wcwd); // never returns
        for (int i = 0; i < nfd; i++)
            close(fds[i]); // ours were only for the runner
        int32_t p32 = (int32_t)(pid > 0 ? pid : -1);
        (void)hl_fork_wire_send(conn, &p32, 4); // the client forwards ^C/kill to this pid while it waits
        if (pid < 0) {
            close(conn); // fork failure: EOF-without-status -> the client reports 125
            continue;
        }
        g_fsrv_live[slot].pid = pid;
        g_fsrv_live[slot].conn = conn;
    }
    close(ls);
    hl_host_child_watch_close(&g_fsrv_wake);
    unlink(sock);
    return 0;
}

// ---- client ----
static volatile pid_t g_fwd_pid; // the remote runner; forward received signals to it

static void fwd_sig(int s) {
    pid_t p = g_fwd_pid;
    if (p > 0) kill(p, s);
}

static int hl_client_main(int argc, char **argv) {
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
        fprintf(stderr, "usage: hl-engine --client SOCK [--rootfs DIR] PROG [args...]\n");
        return 2;
    }
    hl_host_fork_client client = HL_HOST_FORK_CLIENT_INIT;
    if (hl_host_fork_client_open(&client, sock) != 0) {
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
    if (hl_fork_wire_pack_strings(buf, sizeof buf, &o, argc - ai, argv + ai) != 0 ||
        hl_fork_wire_pack_strings(buf, sizeof buf, &o, nenv, environ) != 0 || o + 4 + (size_t)cl > sizeof buf) {
        fprintf(stderr, "hl-engine --client: request too large\n");
        hl_host_fork_client_close(&client);
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
    if (hl_host_fork_client_send_launch(&client, buf, o) < 0) {
        perror("sendmsg");
        hl_host_fork_client_close(&client);
        return 1;
    }
    // 1st reply: the runner's pid -- forward job-control-ish signals to it while we wait, so ^C /
    // kill on the client behaves exactly as if the engine ran in this process.
    int32_t rpid = -1;
    if (hl_host_fork_client_receive(&client, &rpid, 4) != 0 || rpid <= 0) {
        hl_host_fork_client_close(&client);
        return 125; // server died / protocol error (matches the W3D client's error code)
    }
    g_fwd_pid = (pid_t)rpid;
    int fwd[] = {SIGINT, SIGTERM, SIGHUP, SIGQUIT, SIGUSR1, SIGUSR2, SIGWINCH};
    for (size_t i = 0; i < sizeof fwd / sizeof fwd[0]; i++)
        signal(fwd[i], fwd_sig);
    // 2nd reply: the runner's raw wait status. Reproduce it exactly: re-raise a fatal signal on
    // ourselves (so the caller's WIFSIGNALED view matches a cold spawn), else exit with its code.
    int32_t st = 0;
    if (hl_host_fork_client_receive(&client, &st, 4) != 0) {
        hl_host_fork_client_close(&client);
        return 125;
    }
    hl_host_fork_client_close(&client);
    if (WIFSIGNALED(st)) {
        int sig = WTERMSIG(st);
        signal(sig, SIG_DFL);
        raise(sig);
        return 128 + sig; // unreachable for default-fatal signals; belt-and-braces
    }
    return WIFEXITED(st) ? WEXITSTATUS(st) : 125;
}
