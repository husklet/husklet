#ifndef HL_LINUX_ABI_SYSCALL_NONPIE_ARGS_H
#define HL_LINUX_ABI_SYSCALL_NONPIE_ARGS_H

// THE table of which syscall ARGUMENTS are guest pointers.
//
// A non-PIE ET_EXEC is mapped HIGH (+g_nonpie_bias) while every pointer baked into it keeps its LOW link
// vaddr, so a pointer-typed argument in [g_nonpie_lo,g_nonpie_hi) must be folded before anything
// dereferences it. Per-syscall AND per-position: a blanket a0..a5 fold would corrupt a count/fd that
// happened to land in the link range. Numbers are the canonical (aarch64) ones G_NR() maps an x86 guest
// onto, so one case serves both guests. Inert for PIE/static-PIE (g_nonpie_lo == 0).
//
// There is exactly ONE list because there were two: service_local's and the sentry trust boundary's,
// hand-maintained side by side. Each had cases the other lacked -- the sentry knew ioctl/sendfile/splice/
// copy_file_range/get+setsockopt/memfd_create and the accept/getsockname/getpeername/recvfrom in-out
// socklen, service_local knew 103 numbers the sentry did not -- and a gap here is a guest pointer
// dereferenced unfolded, which is how this bug family keeps recurring. The subsets are now DERIVED: the
// sentry applies this same table restricted to sentry_forwarded(nr), because a call it does not forward
// reaches service_local, which applies the table in full.
//
// Folding is idempotent (the image occupies [lo+bias,hi+bias), disjoint from [lo,hi)), so the sentry
// folding a register that service_local then folds again is a no-op -- the same property guest_span
// relies on.

static void nonpie_rebase_args(uint64_t nr, uint64_t a[6]) {
    switch (nr) {
    case 56:  // openat(dfd, PATH, flags, mode)
    case 33:  // mknodat(dfd, PATH, ...)
    case 34:  // mkdirat(dfd, PATH, mode)
    case 35:  // unlinkat(dfd, PATH, flags)
    case 48:  // faccessat(dfd, PATH, mode)
    case 439: // faccessat2(dfd, PATH, mode, flags)
    case 53:  // fchmodat(dfd, PATH, mode, flags)
    case 452: // fchmodat2(dfd, PATH, mode, flags)
    case 54:  // fchownat(dfd, PATH, uid, gid, flags)
        a[1] = nonpie_p(a[1]);
        break; //   path is a[1] for the whole *at family
    case 437:
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break; // openat2(dfd, PATH, open_how*, size)
    case 79:   // newfstatat(dfd, PATH, STATBUF, flags)
    case 78:
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break; // readlinkat(dfd, PATH, BUF, sz)
    case 88:   // utimensat(dfd, PATH, TIMES[2], flags) -- sibling of fchmodat/fchownat in the *at
               // metadata family. Both the path (a[1]) and the struct timespec[2] TIMES (a[2]) can be
               // low link-vaddr pointers in a non-PIE (glibc's utime/utimes/utimensat + LTP's
               // SAFE_TOUCH pass .rodata/.bss addresses); without the rebase the host utimensat reads
               // an unmapped low address and EFAULTs (LTP link02/link05/lstat01/lstat02 BROK in
               // SAFE_TOUCH setup). a[1]==NULL (futimens-by-fd) / a[2]==NULL (times=now) stay 0 via nonpie_p.
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    case 291:
        a[1] = nonpie_p(a[1]);
        a[4] = nonpie_p(a[4]);
        break; // statx(dfd, PATH, flags, mask, STATXBUF)
    case 36:
        a[0] = nonpie_p(a[0]);
        a[2] = nonpie_p(a[2]);
        break; // symlinkat(TARGET, newdfd, LINKPATH)
    case 37:   // linkat(odfd, OLD, ndfd, NEW, flags)
    case 38:   // renameat(odfd, OLD, ndfd, NEW)
    case 276:
        a[1] = nonpie_p(a[1]);
        a[3] = nonpie_p(a[3]);
        break;                              // renameat2(odfd, OLD, ndfd, NEW, flags)
    case 80:                                // fstat(fd, STATBUF)
    case 63:                                // read(fd, BUF, count)
    case 64:                                // write(fd, BUF, count)
    case 67:                                // pread64(fd, BUF, count, off)
    case 68:                                // pwrite64(fd, BUF, count, off)
    case 200:                               // bind(fd, SOCKADDR, alen)     -- alen is a scalar in, never a pointer
    case 203:                               // connect(fd, SOCKADDR, alen)
    case 61:                                // getdents64(fd, DIRENT_BUF, count)
    case 113: a[1] = nonpie_p(a[1]); break; // clock_gettime(clkid, TIMESPEC)
    case 204:                               // getsockname(fd, ADDR, ALEN)
    case 205:                               // getpeername(fd, ADDR, ALEN)
    case 202:                               // accept(fd, ADDR, ALEN)
    case 242:                               // accept4(fd, ADDR, ALEN, flags) -- alen is an in/out socklen_t* here,
        a[1] = nonpie_p(a[1]);              //   not the scalar bind/connect take: fold it too (the sentry's copy
        a[2] = nonpie_p(a[2]);              //   of this table had it, service_local's did not).
        break;
    case 208: // setsockopt(fd, level, opt, OPTVAL, optlen) -- optlen is a scalar in
        a[3] = nonpie_p(a[3]);
        break;
    case 209: // getsockopt(fd, level, opt, OPTVAL, OPTLEN) -- optlen is in/out here
        a[3] = nonpie_p(a[3]);
        a[4] = nonpie_p(a[4]);
        break;
    case 71: a[2] = nonpie_p(a[2]); break; // sendfile(out, in, OFFSET, count) -- offset read + written
    case 76:                               // splice(fd_in, OFF_IN, fd_out, OFF_OUT, len, flags)
    case 285:                              // copy_file_range(fd_in, OFF_IN, fd_out, OFF_OUT, len, flags)
        a[1] = nonpie_p(a[1]);
        a[3] = nonpie_p(a[3]);
        break;
    case 279: a[0] = nonpie_p(a[0]); break; // memfd_create(NAME, flags)
    case 29:                                // ioctl(fd, req, ARG): the termios/winsize/int* the handler reads or
        a[2] = nonpie_p(a[2]);              //   writes directly. A request whose arg is a scalar passes a small
        break;                              //   int, far below the link range, so the fold leaves it alone.
    case 25:                                // fcntl(fd, cmd, ARG): ARG is a struct flock* ONLY for the record-lock
        if (a[1] == 5 || a[1] == 6 || a[1] == 7) a[2] = nonpie_p(a[2]); // cmds F_GETLK/F_SETLK/F_SETLKW (else it is an
        break; //   int flag/floor arg, never a pointer, so leave it untouched). The
               //   handler dereferences the flock directly (host_range_mapped + reads),
               //   so a low link-vaddr flock in a non-PIE (LTP fcntl05/fcntl13) must be
               //   rebased or the guard EFAULTs on the unmapped low address.
    // iovec-carrying calls -- rebase the array base AND every entry's iov_base. A non-PIE's
    // gather/scatter buffers can themselves be low link-vaddr pointers (skalibs' buffer_1 flush issues
    // writev(fd, iov, n) whose iov_base entries point at .rodata baked at 0x40xxxx). Rebasing only the
    // array base (the old behaviour) left the inner pointers LOW, where nothing is mapped -> the host
    // writev EFAULTs and writes nothing. That is exactly why s6-overlay-stat printed an EMPTY line:
    // s6-overlay preinit's `eval $(s6-overlay-stat /run)` then left $uid unset, so `test "$UID" -ne ""`
    // hit busybox's empty-operand path -> "sh: out of range" and the s6-overlay-v3 boot aborted (111).
    // The rebased copy lives in a per-thread scratch array consumed synchronously by svc_io below.
    case 65:  // readv(fd, IOVEC, n)
    case 66:  // writev(fd, IOVEC, n)
    case 75:  // vmsplice(fd, IOVEC, n, flags) -- same (iov=a[1], iovcnt=a[2]) shape; the handler feeds the
              //   array straight to writev/readv, so a non-PIE guest's low link-vaddr iov_base entries
              //   (and a low iovec array itself) made EVERY vmsplice return EFAULT before this.
    case 69:  // preadv(fd, IOVEC, n, off)
    case 286: // preadv2(fd, IOVEC, n, off, off_hi, flags) -- same (iov=a[1], iovcnt=a[2]) shape
    case 287: // pwritev2(fd, IOVEC, n, off, off_hi, flags) -- same shape; inner iov_base rebased too
    case 70:  // pwritev(fd, IOVEC, n, off) -- the ENTRIES are folded by nonpie_rebase_iov (below), which a
              //   caller that hands the array on to a host syscall runs after this table.
        a[1] = nonpie_p(a[1]);
        break;
    case 17:                                // getcwd(BUF, size)
    case 160: a[0] = nonpie_p(a[0]); break; // uname(UTSBUF)
    case 73:                                // ppoll(FDS, n, TMO, sigmask, sz): the handler dereferences BOTH
        a[0] = nonpie_p(a[0]);              //   the pollfd array (a[0]) AND the timespec deadline (a[2], read for
        a[2] = nonpie_p(a[2]);              //   the budget and written back with the remaining time). sigmask
        break;                              //   (a[3]) is ignored by the handler, so only a[0]+a[2] need rebasing.
    case 206:                               // sendto(fd, BUF, len, fl, SOCKADDR, alen) -- alen is a scalar in
        a[1] = nonpie_p(a[1]);
        a[4] = nonpie_p(a[4]);
        break;
    case 207: // recvfrom(fd, BUF, len, fl, SRCADDR, ALEN) -- alen is in/out here
        a[1] = nonpie_p(a[1]);
        a[4] = nonpie_p(a[4]);
        a[5] = nonpie_p(a[5]);
        break;
    case 199: a[3] = nonpie_p(a[3]); break; // socketpair(domain, type, protocol, SV[2])
    case 211:                               // sendmsg(fd, MSGHDR, flags) -- top only
    case 212: a[1] = nonpie_p(a[1]); break; // recvmsg(fd, MSGHDR, flags) -- top only
    case 221:
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        break; // execve(PATH, ARGV, envp); argv base here,
               //   each argv[] element rebased at case 221
    case 281:  // execveat(dfd, PATH, ARGV, envp, flags) -- mirrors 221 (path + argv base; elements
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break; //   rebased at the shared case-221 body after the case-281 arg shift)
    // Syscalls whose result the ENGINE writes/reads into the guest buffer ITSELF (memset/memcpy/
    // struct fill / arc4random_buf), not via a host syscall -- so there is no host EFAULT fixup to
    // rescue a low, un-rebased non-PIE pointer; the handler's host_range_mapped() guard would simply
    // fail on the unmapped low address. Rebase the buffer arg BEFORE the handler runs. ((a):
    // getrandom's a[0] was the one that made python3.11-x86 EFAULT in _Py_HashRandomization_Init.)
    case 169: // gettimeofday(TIMEVAL, TZ) -- the engine writes BOTH tv (a[0]) and the deprecated tz (a[1])
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        break;
    case 278: // getrandom(BUF, len, flags)      -- buffer is a[0]
    case 179: // sysinfo(INFOBUF)
    case 153: // times(TMSBUF)
    case 236: // get_mempolicy(MODE, ...)        -- mode ptr is a[0]
    case 161: // sethostname(NAME, len)          -- name buffer is a[0]
    case 59:  // pipe2(FDS, flags) -- the two result fds are written into a[0] by the engine itself, so a
              //   low non-PIE fds[] (skalibs/s6-linux-init pass a .bss array at 0x42xxxx) must be rebased
              //   or the handler's host_range_mapped guard EFAULTs ("unable to pipe: Bad address")
        a[0] = nonpie_p(a[0]);
        break;
    case 165: // getrusage(who, RUSAGEBUF)       -- buffer is a[1]
    case 114: // clock_getres(clkid, TIMESPEC)
    case 127: // sched_rr_get_interval(pid, TIMESPEC)
    case 44:  // fstatfs(fd, STATFSBUF)
        a[1] = nonpie_p(a[1]);
        break;
    case 122: // sched_setaffinity(pid, len, MASK)  -- mask read directly (a[1] is a size, never rebased)
    case 123: // sched_getaffinity(pid, len, MASK)  -- mask written directly
    case 115: // clock_nanosleep(clkid, flags, REQUEST, remain) -- req read directly in the ABSTIME loop
        a[2] = nonpie_p(a[2]);
        break;
    case 232: // mincore(ADDR, len, VEC) -- vec is written directly by the engine; addr may name image
              //   pages (mincore of the binary's own mapping) so rebase both. An mmap result is high and
              //   outside [nonpie_lo,hi) so nonpie_p leaves it. (LTP mincore02's vec is a .bss static.)
        a[0] = nonpie_p(a[0]);
        a[2] = nonpie_p(a[2]);
        break;
    case 101: // nanosleep(REQUEST, remain) -- both read/written directly by the engine's deadline loop
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        break;
    case 261: // prlimit64(pid, res, NEW, OLD) -- NEW read (a[2]) + OLD written (a[3]), both derefed by the
              // handler (proc.c case 261) with NO host_range_mapped guard. glibc's setrlimit() funnels to
              // prlimit64(pid,res,&new,NULL), so a non-PIE static binary's `static struct rlimit` NEW is a
              // low .bss link vaddr -- rebase a[2] as well or the unguarded `nl[0]/nl[1]` read SIGSEGVs on
              // the unmapped low address. (Latent until x86 lea stopped pre-biasing pointers HIGH: on
              // aarch64 the low a[2] fault was silently served by nonpie_fixup; x86 hard-crashed. Rebasing
              // here fixes both arches directly, no fault-path reliance.)
        a[2] = nonpie_p(a[2]);
        a[3] = nonpie_p(a[3]);
        break;
    case 43:  // statfs(PATH, STATFSBUF)         -- path read + buffer written
    case 168: // getcpu(CPU, NODE, tcache)       -- cpu + node written
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        break;
    // the remaining PATH-taking fs syscalls a non-PIE hands a low.rodata/.bss pointer to. Without
    // the rebase the host syscall (or the engine's own resolve/copy) dereferences the un-relocated low
    // link vaddr -> EFAULT/SIGSEGV on a VALID guest pointer (arm64 LTP truncate02/getcwd02 static-EXEC).
    // These mirror the *at family above but are the "bare path" (a[0]) or fd+name/value forms.
    case 45: // truncate(PATH, length)            -- path a[0] (length is a scalar, never rebased)
    case 49: // chdir(PATH)                        -- path a[0]
    case 51: // chroot(PATH)                       -- path a[0]
        a[0] = nonpie_p(a[0]);
        break;
    case 5: // setxattr(PATH, NAME, VALUE, size, flags)
    case 6: // lsetxattr(PATH, NAME, VALUE, size, flags)
    case 8: // getxattr(PATH, NAME, VALUE, size)
    case 9: // lgetxattr(PATH, NAME, VALUE, size)
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    case 7:  // fsetxattr(fd, NAME, VALUE, size, flags)   -- a[0] is an fd
    case 10: // fgetxattr(fd, NAME, VALUE, size)          -- a[0] is an fd
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    case 11: // listxattr(PATH, LIST, size)
    case 12: // llistxattr(PATH, LIST, size)
    case 14: // removexattr(PATH, NAME)
    case 15: // lremovexattr(PATH, NAME)
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        break;
    case 13: // flistxattr(fd, LIST, size)                -- a[0] is an fd
    case 16: // fremovexattr(fd, NAME)                    -- a[0] is an fd
        a[1] = nonpie_p(a[1]);
        break;
    case 264: // name_to_handle_at(dfd, PATH, HANDLE, MOUNT_ID, flags)
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        a[3] = nonpie_p(a[3]);
        break;
    // Struct-writer/reader time syscalls the engine fills/reads via the guest pointer directly (same
    // class as sysinfo/times/gettimeofday/getrusage above) -- rebase the low non-PIE struct pointer.
    case 102: // getitimer(which, CURR_VALUE)       -- itimerval written to a[1]
    case 266: // clock_adjtime(clkid, TIMEX)        -- timex read+written at a[1]
        a[1] = nonpie_p(a[1]);
        break;
    case 103: // setitimer(which, NEW_VALUE, OLD_VALUE)
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    case 171: // adjtimex(TIMEX)                     -- timex read+written at a[0]
        a[0] = nonpie_p(a[0]);
        break;
    // timer / timerfd / sched / signalfd / epoll_ctl handlers dereference their struct pointers
    // directly (itimerspec / sigevent / sched_param / sigset / epoll_event), so a low link-vaddr pointer
    // in a non-PIE (LTP's static test binaries put these in .bss/.data at ~0x52xxxx) must be rebased or
    // the handler's guest_bad_ptr guard EFAULTs on the unmapped low address.
    case 74:  // signalfd4(fd, MASK, sizemask, flags)     -- sigset read directly
    case 87:  // timerfd_gettime(fd, CURR)                -- itimerspec written by the engine
    case 108: // timer_gettime(timerid, CURR)
    case 118: // sched_setparam(pid, PARAM)               -- sched_param read directly
    case 121: // sched_getparam(pid, PARAM)               -- sched_param written by the engine
        a[1] = nonpie_p(a[1]);
        break;
    case 119: // sched_setscheduler(pid, policy, PARAM)   -- sched_param read directly
        a[2] = nonpie_p(a[2]);
        break;
    case 21: // epoll_ctl(epfd, op, fd, EVENT)           -- epoll_event read directly
        a[3] = nonpie_p(a[3]);
        break;
    case 27: // inotify_add_watch(fd, PATH, mask)        -- path consumed directly by atpath
        a[1] = nonpie_p(a[1]);
        break;
    case 86:  // timerfd_settime(fd, flags, NEW, OLD)     -- new read / old written
    case 110: // timer_settime(timerid, flags, NEW, OLD)
        a[2] = nonpie_p(a[2]);
        a[3] = nonpie_p(a[3]);
        break;
    case 107: // timer_create(clockid, SIGEVENT, TIMERID) -- sigevent read / timer id written
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    // the remaining pointer-arg syscalls whose handler dereferences the guest pointer DIRECTLY (a
    // host-syscall deref, or the engine reading/writing the guest struct itself) that were still missing
    // from this switch -- so a non-PIE guest handing a low .bss/.rodata/.data pointer EFAULTed (or, for the
    // unguarded handlers, SIGSEGV'd the engine) on a VALID pointer. This is the getgroups/semop/msgsnd
    // report plus the WHOLE class audited alongside it: the credential, SysV-IPC, rt_signal, sched/rlimit,
    // poll/select and POSIX-mqueue families. Numbers are the aarch64-canonical ones the x86 guest is
    // normalized onto BEFORE this switch; on x86 the arg was already biased HIGH by the loader/translator
    // (elf.c/mov.c), so nonpie_p is inert there -> ONE case covers both arches with no double-rebase,
    // exactly like the shared cases above. (Inert for PIE/static-PIE: the whole switch is gated on
    // g_nonpie_lo, which the entire test matrix leaves 0.)
    // -- credentials (proc.c: the buffers are written directly / guarded by guest_bad_ptr) --
    case 90: // capget(HDRP, DATAP)
    case 91: // capset(HDRP, DATAP)
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        break;
    case 167: // prctl: only pointer-bearing option arguments are rebased
        if (a[0] == 2 || a[0] == 15 || a[0] == 16 || a[0] == 37) a[1] = nonpie_p(a[1]);
        if (a[0] == 22 && a[1] == 2) a[2] = nonpie_p(a[2]); // PR_SET_SECCOMP(FILTER, sock_fprog *)
        break;
    case 148: // getresuid(RUID, EUID, SUID) -- all three written directly
    case 150: // getresgid(RGID, EGID, SGID)
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    case 158: // getgroups(size, LIST) -- list written directly (or via host getgroups)
        a[1] = nonpie_p(a[1]);
        break;
    // -- SysV IPC (sysv.c) --
    case 188: // msgrcv(msqid, MSGP, sz, typ, flg) -- msgp written by host msgrcv
    case 189: // msgsnd(msqid, MSGP, sz, flg)      -- msgp read by host msgsnd
    case 193: // semop(semid, SOPS, nsops)         -- sops read by host semop
        a[1] = nonpie_p(a[1]);
        break;
    case 192:                  // semtimedop(semid, SOPS, nsops, TIMEOUT) -- sops (+timeout; harmless if the handler,
        a[1] = nonpie_p(a[1]); //   which routes to semop, ignores it)
        a[3] = nonpie_p(a[3]);
        break;
    case 191: // semctl(semid, semnum, CMD, arg): arg(a[3]) is a pointer ONLY for GETALL(13)/SETALL(17);
        if (a[2] == 13 || a[2] == 17) a[3] = nonpie_p(a[3]); //   SETVAL(16)'s a[3] is an int val -> never rebased
        break;
    case 195: // shmctl(shmid, cmd, BUF): IPC_STAT marshals the host struct into buf(a[2]) directly
        a[2] = nonpie_p(a[2]);
        break;
    // -- rt_signal family (signal.c, + rt_tgsigqueueinfo in rare.c): the sigset/siginfo/sigaction/altstack/
    //    timespec structs are read or written through the guest pointer directly (rt_sigaction EFAULTs via
    //    host_range_mapped on a low ptr; the others would fault the engine) --
    case 132: // sigaltstack(NEW, OLD)         -- new read, old written
    case 134: // rt_sigaction(sig, ACT, OLD)   -- act read, old written
    case 135: // rt_sigprocmask(how, SET, OLD) -- set read, old written
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    case 133: // rt_sigsuspend(UNEWSET, sz) -- mask read directly
    case 136: // rt_sigpending(SET, sz)     -- pending set written directly
        a[0] = nonpie_p(a[0]);
        break;
    case 137: // rt_sigtimedwait(SET, INFO, TIMEOUT, sz) -- set read, info written, timeout read
        a[0] = nonpie_p(a[0]);
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    case 138: // rt_sigqueueinfo(tgid, sig, INFO)        -- siginfo read directly
        a[2] = nonpie_p(a[2]);
        break;
    case 240: // rt_tgsigqueueinfo(tgid, tid, sig, INFO)  -- siginfo read directly (rare.c handler)
        a[3] = nonpie_p(a[3]);
        break;
    // -- sched / rlimit / wait (rare.c + proc.c) --
    case 95: // waitid(idtype, id, INFOP, options) -- siginfo written (host_range_mapped guard EFAULTs low)
        a[2] = nonpie_p(a[2]);
        break;
    case 98:                   // futex(UADDR, op, val, TIMEOUT/nr_wake2, UADDR2, val3) -- uaddr/timeout/uaddr2 are
        a[0] = nonpie_p(a[0]); //   dereferenced by futex_op; a non-PIE static libc's lock word / timespec live in
        a[3] = nonpie_p(a[3]); //   .bss at a low link vaddr. uaddr2 (a[4]) is the REQUEUE/WAKE_OP target -- a real
        a[4] = nonpie_p(a[4]); //   guest pointer, so rebase it too (inert for PIE; a[3]-as-nr_wake2 is a small int
        break;                 //   below g_nonpie_lo, so nonpie_p leaves it unchanged).
    case 163:                  // getrlimit(res, RLIM) -- rlim written
    case 164:                  // setrlimit(res, RLIM) -- rlim read
    case 274:                  // sched_setattr(pid, ATTR, flags)        -- attr read directly
    case 275:                  // sched_getattr(pid, ATTR, size, flags) -- attr zeroed+written directly
        a[1] = nonpie_p(a[1]);
        break;
    case 260: // wait4(pid, STATUS, opts, RUSAGE) -- status + rusage written directly
        a[1] = nonpie_p(a[1]);
        a[3] = nonpie_p(a[3]);
        break;
    // -- poll / select (event.c): the pollfd/fd_set/timespec buffers are read+written directly --
    case 22: // epoll_pwait(epfd, EVENTS, max, tmo, SIGMASK) -- events written (sigmask a[4] handler-ignored)
        a[1] = nonpie_p(a[1]);
        a[4] = nonpie_p(a[4]);
        break;
    case 72: // pselect6(n, READFDS, WRITEFDS, EXCEPTFDS, TIMEOUT, sigmask) -- all four deref'd directly
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        a[3] = nonpie_p(a[3]);
        a[4] = nonpie_p(a[4]);
        break;
    // -- POSIX message queues (rare.c: name/msg/attr/timeout read or written directly) --
    case 180: // mq_open(NAME, oflag, mode, ATTR) -- name string + attr read
        a[0] = nonpie_p(a[0]);
        a[3] = nonpie_p(a[3]);
        break;
    case 181: // mq_unlink(NAME)
        a[0] = nonpie_p(a[0]);
        break;
    case 182: // mq_timedsend(mqdes, MSG, len, prio, TIMEOUT) -- msg read (+timeout)
        a[1] = nonpie_p(a[1]);
        a[4] = nonpie_p(a[4]);
        break;
    case 183: // mq_timedreceive(mqdes, MSG, len, PRIO, TIMEOUT) -- msg + prio written (+timeout)
        a[1] = nonpie_p(a[1]);
        a[3] = nonpie_p(a[3]);
        a[4] = nonpie_p(a[4]);
        break;
    case 184: // mq_notify(mqdes, SEVP) -- sigevent read (host-forwarded / broker-parsed)
        a[1] = nonpie_p(a[1]);
        break;
    case 185: // mq_getsetattr(mqdes, NEWATTR, OLDATTR) -- oldattr written (newattr ignored by handler)
        a[1] = nonpie_p(a[1]);
        a[2] = nonpie_p(a[2]);
        break;
    default: break;
    }
}

// Second pass for the callers that hand the iovec ARRAY STRAIGHT to a host readv/writev: the entries'
// own iov_base can each be a low link vaddr too (skalibs' buffer_1 flush passes .rodata baked at
// 0x40xxxx), and folding only the array base leaves them unmapped -> the host call EFAULTs and writes
// nothing. Substitutes a per-thread rebased copy for the array. `count` is the iovcnt argument.
// The sentry does NOT use this: it copies the payload into the ring rather than passing the array on,
// and folds each iov_base inside its own flatten (case 65/66 there).
static void nonpie_rebase_iov(uint64_t *iov, uint64_t count) {
    int niov = (int)count;
    if (niov <= 0 || niov > 1024) return;
    static _Thread_local struct iovec reb[1024];
    if (guest_iov_import(*iov, (size_t)niov, reb) != 0) return;
    for (int i = 0; i < niov; i++)
        reb[i].iov_base = (void *)nonpie_p((uint64_t)(uintptr_t)reb[i].iov_base);
    *iov = (uint64_t)(uintptr_t)reb;
}

// Nonzero for the numbers whose a1 is an iovec array + a2 its count (the nonpie_rebase_iov set above).
static int nonpie_iov_carrier(uint64_t nr) {
    return nr == 65 || nr == 66 || nr == 69 || nr == 70 || nr == 75 || nr == 286 || nr == 287;
}

#endif
