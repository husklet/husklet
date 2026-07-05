// frontend/x86_64/legacy.c -- x86-64 has 58 legacy syscalls aarch64 lacks (open/stat/mkdir/pipe/...).
// The shared os/linux/service.c only knows the canonical *at forms. This rewrites each legacy call into
// its *at-equivalent x86 syscall (number + args) BEFORE service() runs canon_x86() + the canonical
// switch, so one shared service serves both guests.
//
// Some x86-only syscalls have no canonical (aarch64) form at all (arch_prctl = the x86 TLS register);
// those are handled HERE and return 1 ("done, don't run the shared switch"). The rest are rewritten in
// place and return 0 to fall through to the shared service.
//
// x86-64 ABI reg map: rax=r[0], rdi=r[7], rsi=r[6], rdx=r[2], r10=r[10], r8=r[8], r9=r[9].
#define ATFD ((uint64_t)-100) // AT_FDCWD

// A legacy TIME syscall (utime/utimes/futimesat) hands us a `struct utimbuf` / `struct timeval[2]` POINTER
// that we must dereference HERE to convert it to a `struct timespec[2]` for utimensat -- and we run BEFORE
// dispatch.c's non-PIE pointer-arg rebase (nonpie_p) block. So a non-PIE ET_EXEC's .data/.bss times pointer
// (a low link vaddr, real bytes at +bias in the high-mapped image) must be rebased by us before the deref.
// g_nonpie_lo/g_nonpie_bias are the (tentative) globals engine_glue.c declares above; g_nonpie_hi is defined
// later by container/vfs.c -- forward it tentatively here (all three tentative defs merge to one object, the
// exact pattern engine_glue.c documents). Inert identity for PIE/static-PIE (g_nonpie_lo == 0).
static uint64_t g_nonpie_hi;

static inline uint64_t x86_nonpie(uint64_t a) {
    return (g_nonpie_lo && a >= g_nonpie_lo && a < g_nonpie_hi) ? a + g_nonpie_bias : a;
}

// Convert a guest `struct timeval[2]` (utimes/futimesat) at guest pointer `p` into `ts`. On x86-64 Linux a
// `struct timeval` is {s64 tv_sec; s64 tv_usec} (16 bytes) -- NOT the host macOS layout (suseconds_t is 32b)
// -- so read the four fields as raw s64 rather than casting to the host struct. Returns 1 if `ts` was filled,
// 0 if p==NULL ("set to now": the caller then passes a NULL times pointer, which utimensat maps to UTIME_NOW
// on both fields, matching Linux utimes(NULL)/futimesat(...,NULL)).
static int x86_tv2ts(uint64_t p, struct timespec ts[2]) {
    if (!p) return 0;
    int64_t *tv = (int64_t *)x86_nonpie(p);
    ts[0].tv_sec = (time_t)tv[0];
    ts[0].tv_nsec = (long)tv[1] * 1000L;
    ts[1].tv_sec = (time_t)tv[2];
    ts[1].tv_nsec = (long)tv[3] * 1000L;
    return 1;
}

// fork/vfork register snapshot. The fork(57)/vfork(58) -> clone(SIGCHLD) rewrite below repurposes the
// guest's argument registers, but glibc's __vfork/__fork asm wrappers keep LIVE state in them across the
// syscall (the kernel ABI preserves every GPR but rax/rcx/r11). The shared clone handler restores them
// post-fork via G_FORK_PRESERVE(), making the rewrite invisible to the guest. Per-thread + one-shot
// (set only here, consumed in the clone case), so a real clone(56) call never triggers a restore.
static __thread uint64_t g_x86_forksave[5];
static __thread int g_x86_forksave_on;
#define G_FORK_PRESERVE(c)                                                                                             \
    do {                                                                                                               \
        if (g_x86_forksave_on) {                                                                                       \
            (c)->r[7] = g_x86_forksave[0];  /* rdi */                                                                  \
            (c)->r[6] = g_x86_forksave[1];  /* rsi */                                                                  \
            (c)->r[2] = g_x86_forksave[2];  /* rdx */                                                                  \
            (c)->r[10] = g_x86_forksave[3]; /* r10 */                                                                  \
            (c)->r[8] = g_x86_forksave[4];  /* r8  */                                                                  \
            g_x86_forksave_on = 0;                                                                                     \
        }                                                                                                              \
    } while (0)

static int x86_normalize(struct cpu *c) {
    uint64_t *r = c->r;
    switch (r[0]) {
    case 158: // arch_prctl(code, addr): x86 segment-base TLS register; no aarch64 equivalent
        if (r[7] == 0x1002) {
            c->fs_base = r[6];
            r[0] = 0;
            return 1;
        } // ARCH_SET_FS
        if (r[7] == 0x1001) {
            c->gs_base = r[6];
            r[0] = 0;
            return 1;
        } // ARCH_SET_GS
        if (r[7] == 0x1003) {
            *(uint64_t *)r[6] = c->fs_base;
            r[0] = 0;
            return 1;
        } // ARCH_GET_FS
        if (r[7] == 0x1004) {
            *(uint64_t *)r[6] = c->gs_base;
            r[0] = 0;
            return 1;
        } // ARCH_GET_GS
        r[0] = (uint64_t)-22;
        return 1; // EINVAL
    // --- path ops: prepend AT_FDCWD, shift the rest ---
    case 2: // open(path,flags,mode) -> openat(AT_FDCWD,path,flags,mode)
        r[10] = r[2];
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 257;
        return 0;
    case 4: // stat(path,buf) -> newfstatat(AT_FDCWD,path,buf,0)
        r[10] = 0;
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 262;
        return 0;
    case 6: // lstat -> newfstatat(...,AT_SYMLINK_NOFOLLOW)
        r[10] = 0x100;
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 262;
        return 0;
    case 21: // access(path,mode) -> faccessat(AT_FDCWD,path,mode)
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 269;
        return 0;
    case 83: // mkdir(path,mode) -> mkdirat
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 258;
        return 0;
    case 84: // rmdir(path) -> unlinkat(...,AT_REMOVEDIR)
        r[2] = 0x200;
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 263;
        return 0;
    case 87: // unlink(path) -> unlinkat(...,0)
        r[2] = 0;
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 263;
        return 0;
    case 85: // creat(path,mode) -> openat(...,O_CREAT|O_WRONLY|O_TRUNC,mode)
        r[10] = r[6];
        r[2] = 0x241;
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 257;
        return 0;
    case 89: // readlink(path,buf,sz) -> readlinkat(AT_FDCWD,path,buf,sz)
        r[10] = r[2];
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 267;
        return 0;
    case 90: // chmod(path,mode) -> fchmodat(AT_FDCWD,path,mode)
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 268;
        return 0;
    case 88: // symlink(target,link) -> symlinkat(target,AT_FDCWD,link)
        r[2] = r[6];
        r[6] = ATFD;
        r[0] = 266;
        return 0;
    case 92: // chown(path,uid,gid) -> fchownat(AT_FDCWD,path,uid,gid,0)
        r[8] = 0;
        r[10] = r[2];
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 260;
        return 0;
    case 94: // lchown -> fchownat(...,AT_SYMLINK_NOFOLLOW)
        r[8] = 0x100;
        r[10] = r[2];
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 260;
        return 0;
    case 133: // mknod(path,mode,dev) -> mknodat(AT_FDCWD,path,mode,dev)
        r[10] = r[2];
        r[2] = r[6];
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 259;
        return 0;
    // --- legacy time-setters: no aarch64 canonical form (arm64 261 is prlimit64, 132/235 are absent), so
    //     canon_x86 biases them into the x86-only range and they returned ENOSYS-by-normalization. Rewrite
    //     each to x86 utimensat(280) [-> canonical 88], converting the legacy struct utimbuf / struct
    //     timeval[2] into the struct timespec[2] the shared handler wants. The times buffer is a static
    //     __thread scratch (a high engine address, so dispatch.c's nonpie_p leaves it untouched); the PATH
    //     pointer is left raw for dispatch.c to rebase. NULL times -> pass a NULL times pointer, which the
    //     host utimensat maps to UTIME_NOW on both fields -- exactly Linux utime(NULL)/utimes(NULL).
    case 132: { // utime(path, const struct utimbuf{s64 actime; s64 modtime}* | NULL)
        static __thread struct timespec ts[2];
        uint64_t tp = r[6];
        if (tp) {
            int64_t *ub = (int64_t *)x86_nonpie(tp);
            ts[0].tv_sec = (time_t)ub[0];
            ts[0].tv_nsec = 0;
            ts[1].tv_sec = (time_t)ub[1];
            ts[1].tv_nsec = 0;
            r[2] = (uint64_t)ts;
        } else
            r[2] = 0;
        r[6] = r[7];
        r[7] = ATFD;
        r[10] = 0;
        r[0] = 280;
        return 0;
    }
    case 235: { // utimes(path, const struct timeval[2] | NULL)
        static __thread struct timespec ts[2];
        r[2] = x86_tv2ts(r[6], ts) ? (uint64_t)ts : 0;
        r[6] = r[7];
        r[7] = ATFD;
        r[10] = 0;
        r[0] = 280;
        return 0;
    }
    case 261: { // futimesat(dirfd, path, const struct timeval[2] | NULL) -- dirfd(rdi)/path(rsi) already sit
                // in the utimensat arg slots; only the times buffer (rdx) needs conversion.
        static __thread struct timespec ts[2];
        r[2] = x86_tv2ts(r[2], ts) ? (uint64_t)ts : 0;
        r[10] = 0;
        r[0] = 280;
        return 0;
    }
    // time(time_t *tloc): x86-only (aarch64 glibc reads the vDSO / clock_gettime, so there is no `time`
    // syscall and sysmap biases 201 into the x86-only range -> ENOSYS, and glibc's raw-syscall fallback in
    // x86 `time()` failed). Serve from host time(): return seconds since the epoch, and store into *tloc when
    // non-NULL (a non-PIE .bss slot -> rebase before the store).
    case 201: {
        time_t t = time(NULL);
        if (r[7]) *(int64_t *)x86_nonpie(r[7]) = (int64_t)t;
        r[0] = (uint64_t)(int64_t)t;
        return 1;
    }
    // pause(): x86-only (aarch64 glibc's pause() already lands in ppoll -- event.c case 73 notes it). Block
    // until a signal by rewriting to ppoll(NULL, 0, NULL, NULL): the shared ppoll handler poll(NULL,0,-1)s
    // and returns -EINTR when a handler runs (back-edge poll delivers it), matching Linux pause.
    case 34:
        r[7] = 0;
        r[6] = 0;
        r[2] = 0;
        r[10] = 0;
        r[0] = 271;
        return 0;
    case 82: // rename(old,new) -> renameat(AT_FDCWD,old,AT_FDCWD,new)
        r[10] = r[6];
        r[2] = ATFD;
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 264;
        return 0;
    case 86: // link(old,new) -> linkat(AT_FDCWD,old,AT_FDCWD,new,0)
        r[8] = 0;
        r[10] = r[6];
        r[2] = ATFD;
        r[6] = r[7];
        r[7] = ATFD;
        r[0] = 265;
        return 0;
    // poll(fds, nfds, timeout_ms) -> ppoll(fds, nfds, &timespec | NULL, NULL): the canonical handler reads
    // arg2 as a timespec*, but x86 poll's arg2 is an int ms, so synthesize the timespec here.
    case 7: {
        static __thread struct timespec pts;
        long ms = (long)r[2];
        if (ms < 0)
            r[2] = 0; // infinite -> NULL timespec
        else {
            pts.tv_sec = ms / 1000;
            pts.tv_nsec = (ms % 1000) * 1000000L;
            r[2] = (uint64_t)&pts;
        }
        r[10] = 0;
        r[0] = 271;
        return 0; // -> x86 ppoll (271), which canon_x86 maps to canonical 73
    }
    // select(nfds, readfds, writefds, exceptfds, timeval*) -> pselect6(nfds, rd, wr, ex, timespec*, NULL):
    // nfds + the three fd_set pointers already sit in the pselect6 arg slots (rdi/rsi/rdx/r10), and the
    // fd_set byte-layout is identical x86<->arm, so only the timeout differs -- x86 select's arg5 is a
    // `struct timeval` (sec+usec) but pselect6 wants a `struct timespec` (sec+nsec), so synthesize one
    // here. NOTE: Linux select writes the *remaining* time back into the timeval on return; pselect6 (and
    // the host pselect that backs the canonical handler) don't report it, so the guest timeval is left
    // unchanged -- portable code re-inits the timeout each call, and the select users we care about
    // (socat, apr's select-based sleep) do.
    case 23: {
        static __thread struct timespec sts;
        if (r[8]) { // non-NULL timeout: timeval -> timespec
            struct timeval *tv = (struct timeval *)r[8];
            sts.tv_sec = tv->tv_sec;
            sts.tv_nsec = (long)tv->tv_usec * 1000L;
            r[8] = (uint64_t)&sts;
        } // else r[8]==0 (NULL) -> block forever, pass through
        r[9] = 0;
        r[0] = 270;
        return 0; // pselect6 arg6 sigmask = NULL; -> x86 pselect6 (270) -> canonical 72
    }
    case 232:
        r[8] = 0;
        r[0] = 281;
        return 0; // epoll_wait(epfd,ev,max,timeout) -> epoll_pwait(...,NULL sigmask)
    // --- flag-arg variants (append a 0 flags) ---
    case 22:
        r[6] = 0;
        r[0] = 293;
        return 0; // pipe(fds) -> pipe2(fds,0)
    // dup2(old,new) -> dup3(old,new, DUP2_COMPAT). dup2 and dup3 DIVERGE on old==new: dup3 -> EINVAL, but
    // dup2 -> returns new unchanged (EBADF if old is invalid). The shared canonical handler (io.c case 24)
    // reads flag bit 30 (0x40000000, a private marker no real dup3 flag uses) to apply dup2 semantics.
    case 33:
        r[2] = 0x40000000u;
        r[0] = 292;
        return 0;
    case 284:
        r[6] = 0;
        r[0] = 290;
        return 0; // eventfd(n) -> eventfd2(n,0)
    case 282:
        r[10] = 0;
        r[0] = 289;
        return 0; // signalfd(fd,mask,sz) -> signalfd4(...,0)
    case 213:     // epoll_create(size) -> epoll_create1(0). Linux rejects size <= 0 with EINVAL (LTP
                  // epoll_create01); the size is otherwise ignored since 2.6.8.
        if ((int)r[7] <= 0) {
            r[0] = (uint64_t)-22;
            return 1;
        } // EINVAL
        r[7] = 0;
        r[0] = 291;
        return 0;
    case 253:
        r[7] = 0;
        r[0] = 294;
        return 0; // inotify_init() -> inotify_init1(0)
    // clone(flags,stack,ptid,ctid,tls): x86-64 orders the last two args ctid(r10),tls(r8), but the shared
    // service is written against the canonical/aarch64 clone(flags,stack,ptid,tls,ctid) order (tls=a3=r10,
    // ctid=a4=r8). Swap r10<->r8 so the canonical handler reads tls/ctid from the right slots; without this
    // a CLONE_SETTLS thread gets tls=0 -> fs_base=0 -> the child faults on its first %fs TLS access.
    case 56: {
        uint64_t t = r[10];
        r[10] = r[8];
        r[8] = t;
        return 0;
    }
    // --- fork/vfork -> clone(SIGCHLD): the shared clone host-forks when not CLONE_THREAD ---
    // Snapshot the registers we are about to repurpose as clone args; the clone handler restores them after
    // the fork (G_FORK_PRESERVE). Without this, glibc's __vfork (`pop %rdi; syscall; push %rdi; ret`) returns
    // through a clobbered rdi (== SIGCHLD = 0x11) -> jumps to 0x11 -> SIGSEGV: the fork-from-a-shell crash.
    case 57:
    case 58:
        g_x86_forksave[0] = r[7];
        g_x86_forksave[1] = r[6];
        g_x86_forksave[2] = r[2];
        g_x86_forksave[3] = r[10];
        g_x86_forksave[4] = r[8];
        g_x86_forksave_on = 1;
        r[7] = 17;
        r[6] = 0;
        r[2] = 0;
        r[10] = 0;
        r[8] = 0;
        r[0] = 56;
        return 0;
    // getpgrp() -> getpgid(0): x86-only (no aarch64 form, so sysmap leaves it unmapped -> the shared
    // service saw 0x10000|111 and aborted with "unhandled syscall 65647" during bash job-control setup).
    // getpgrp takes no args, so rdi is garbage -> zero it, then run as x86 getpgid(121) -> canonical 155.
    case 111:
        r[7] = 0;
        r[0] = 121;
        return 0;
    // alarm(seconds): x86-only (aarch64 glibc uses setitimer, so there is no canonical `alarm` and sysmap
    // biases 37 into the x86-only range -> it fell to the shared default = "unhandled syscall" and glibc's
    // x86 alarm() got -ENOSYS, so a caught SIGALRM never armed and alarm()-bounded loops hung. Serve it here
    // as host setitimer(ITIMER_REAL) (the host SIGALRM handler is host_sigh, installed by rt_sigaction, and
    // back-edge poll then delivers it into a no-syscall loop). Returns the seconds left on any prior
    // timer, rounding a sub-second remainder up, exactly as Linux alarm() does.
    case 37: {
        struct itimerval nw = {{0, 0}, {0, 0}}, old = {{0, 0}, {0, 0}};
        nw.it_value.tv_sec = (time_t)r[7];
        if (setitimer(ITIMER_REAL, &nw, &old) < 0)
            r[0] = (uint64_t)(-errno);
        else
            r[0] = (uint64_t)old.it_value.tv_sec + (old.it_value.tv_usec ? 1 : 0);
        return 1;
    }
    default: break;
    }
    return 0;
}
