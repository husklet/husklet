/* Included by io.c: unity-build access with bounded I/O capability handlers. */

static int svc_dup(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 23: {
        struct fdvis_reservation fdvis;
        // dup -- a 2nd fd would share the description; flush the RAM cache so both see the real file
        memf_materialize((int)a0);
        if (fd_virt_reserve((int)a0, &fdvis) != 0) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        int r = nofile_gate(dup((int)a0)); // EMFILE if the new fd would be >= the guest's soft RLIMIT_NOFILE
        if (r < 0) proc_fdvis_reservation_cancel(&fdvis);
        // carry path + socket-emulation metadata to the new fd
        if (r >= 0 && r < HL_NFD && (int)a0 >= 0 && (int)a0 < HL_NFD) {
            strcpy(g_fdpath[r], g_fdpath[(int)a0]);
            strcpy(g_proc_text_desc[r], g_proc_text_desc[(int)a0]);
            g_proc_text_ro[r] = g_proc_text_ro[(int)a0]; // dup shares the open file description (dup3/F_DUPFD do too)
            g_pagemap_fd[r] = g_pagemap_fd[(int)a0];
            if (memfd_ensure_fd((int)a0)) {
                g_memfd_is[r] = 1;
                g_memfd_seal[r] = g_memfd_seal[(int)a0];
                memfd_reg_set_fd(r, g_memfd_seal[r]);
            }
            fd_carry_sock(r, (int)a0);
            fd_carry_virt(r, (int)a0, &fdvis); // eventfd/timerfd share the same object across a dup
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_dup3(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 24: {
        struct fdvis_reservation fdvis;
        // dup3(old,new,flags). x86's legacy dup2 arrives here rewritten to the dup3 form (see
        // translator/guest/x86_64/legacy.c) because the two calls DIVERGE on oldfd==newfd: dup3 ->
        // EINVAL, but dup2 -> returns newfd unchanged (EBADF if oldfd is invalid), with no close and no
        // CLOEXEC change. (LTP dup201) Which of the two arrived is reported OUT OF BAND -- it used to
        // ride as bit 30 of the flags, which made a guest's dup3(old,old,0x40000000) succeed where Linux
        // returns EINVAL, because no dup3 flag other than O_CLOEXEC is legal.
        unsigned d3flags = (unsigned)a2;
        int is_dup2 = G_IS_DUP2_COMPAT();
        int oldfd = (int)a0, newfd = (int)a1, nofile = guest_nofile_cur();
        if (oldfd == newfd) {
            if (is_dup2) {
                // dup2(fd,fd): a no-op returning fd iff it is a valid open fd, else EBADF.
                G_RET(c) = (oldfd < 0 || fcntl(oldfd, F_GETFD) < 0) ? (uint64_t)(-EBADF) : (uint64_t)(unsigned)newfd;
                break;
            }
            G_RET(c) = (uint64_t)(-EINVAL); // genuine dup3(old,old,*): EINVAL (before any fd/flag validation)
            break;
        }
        // dup3 flag validation: only O_CLOEXEC is a valid flag (dup2 carries none -> the marker was stripped).
        if (!is_dup2 && (d3flags & ~0x80000u)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // newfd must lie within the guest's descriptor range (the emulated soft RLIMIT_NOFILE); the host fd
        // table is far larger, so a raw dup2/dup3(.., newfd>=cap) would wrongly succeed -> EBADF. (LTP dup201)
        if (newfd < 0 || newfd >= nofile) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // oldfd must be an open descriptor -> EBADF, and (per Linux) checked WITHOUT closing newfd first.
        if (oldfd < 0 || fcntl(oldfd, F_GETFD) < 0) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        if (fd_virt_reserve_at(oldfd, newfd, &fdvis) != 0) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        memf_materialize((int)a0); // source: a 2nd fd shares the description -> flush RAM cache
        memf_close((int)a1);       // target fd is about to be reused; drop any cache it held
        engine_fd_vacate((int)a1); // move any engine-private fd off the target before dup2 overwrites it
        fd_reset_emul((int)a1);    // dup2 atomically closes newfd -> shed ALL its emulation tables (timerfd/
                                   // eventfd/inotify/epoll/sock/...) so the reused number isn't left misrouted; the
                                   // real close is dup2's, and fd_carry_sock below repopulates from oldfd
        int r = dup2((int)a0, (int)a1);
        if (r < 0) proc_fdvis_reservation_cancel(&fdvis);
        if (r >= 0) {
            if (d3flags & 0x80000) fcntl(r, F_SETFD, FD_CLOEXEC); // O_CLOEXEC
            if ((int)a1 >= 0 && (int)a1 < HL_NFD && (int)a0 >= 0 && (int)a0 < HL_NFD) {
                strcpy(g_fdpath[(int)a1], g_fdpath[(int)a0]);
                strcpy(g_proc_text_desc[(int)a1], g_proc_text_desc[(int)a0]);
                g_proc_text_ro[(int)a1] = g_proc_text_ro[(int)a0];
                g_pagemap_fd[(int)a1] = g_pagemap_fd[(int)a0];
                if (memfd_ensure_fd((int)a0)) {
                    g_memfd_is[(int)a1] = 1;
                    g_memfd_seal[(int)a1] = g_memfd_seal[(int)a0];
                    memfd_reg_set_fd((int)a1, g_memfd_seal[(int)a1]);
                }
                fd_carry_sock((int)a1, (int)a0);
                fd_carry_virt((int)a1, (int)a0, &fdvis); // eventfd/timerfd share the same object across a dup
            }
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_fcntl(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 25: {
        struct fdvis_reservation fdvis;
        // fcntl -- Linux cmd# -> macOS (they diverge!)
        int lcmd = (int)a1;
        // F_DUPFD(_CLOEXEC): the floor arg must be a valid descriptor index -- Linux rejects a negative or
        // >= RLIMIT_NOFILE floor with EINVAL (before allocating). (LTP: fcntl bad-arg matrix.)
        if (lcmd == 0 || lcmd == 1030) {
            int floor = (int)a2;
            if (floor < 0 || floor >= guest_nofile_cur()) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // F_DUPFD(_CLOEXEC) makes a 2nd fd sharing the description; F_SETFL O_APPEND changes write-offset
        // semantics. Either way, flush a RAM-backed fd so the real host fd takes over with correct bytes.
        if (lcmd == 0 || lcmd == 1030 || (lcmd == 4 && ((int)a2 & 0x400))) memf_materialize((int)a0);
        // F_GETFL: macOS O_* -> Linux O_*
        if (lcmd == 3) {
            int r = fcntl((int)a0, F_GETFL, 0);
            if (r < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            // access mode identical
            int lf = r & 0x3;
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_proc_text_ro[(int)a0]) lf = 0;
            char fgetpath_buf[4096] = {0};
            int have_fgetpath = 0;
            if ((lf & 0x3) && hl_native_fd_path((int)a0, fgetpath_buf, sizeof fgetpath_buf) == 0) {
                have_fgetpath = 1;
                if (proc_text_host_path(fgetpath_buf)) lf &= ~0x3;
            }
            // Preserve the architecture's native F_GETFL representation (see G_O_LARGEFILE above).
            lf |= G_O_LARGEFILE;
            if (r & O_APPEND) lf |= 0x400;
            if (r & O_NONBLOCK) lf |= 0x800;
            // APPEND/NONBLOCK/ASYNC
            if (r & O_ASYNC) lf |= 0x2000;
#if defined(__linux__) && defined(O_DIRECT)
            // O_DIRECT is a settable status flag on Linux; the guest fd is a real host fd whose kernel
            // O_DIRECT bit is authoritative. Translate host O_DIRECT -> the guest-arch G_O_DIRECT so an
            // fd opened (or F_SETFL'd) O_DIRECT round-trips instead of being silently dropped.
            if (r & O_DIRECT) lf |= G_O_DIRECT;
#endif
            // eventfd: the host read end is kept permanently O_NONBLOCK internally, so report the guest's
            // OWN blocking/non-blocking intent (g_eventfd_gnb), not the host flag. See vfs.c g_eventfd_gnb.
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_eventfd_peer[(int)a0]) {
                lf = eventfd_guest_nb((int)a0) ? (lf | 0x800) : (lf & ~0x800);
            }
            int proc_text_for_log = ((int)a0 >= 0 && (int)a0 < HL_NFD && g_proc_text_ro[(int)a0]) ||
                                    (have_fgetpath && proc_text_host_path(fgetpath_buf));
            if (0 && proc_text_for_log) {
                char p[4096] = {0};
                if (have_fgetpath) {
                    snprintf(p, sizeof p, "%s", fgetpath_buf);
                } else {
                    (void)hl_native_fd_path((int)a0, p, sizeof p);
                }
                fprintf(stderr, "[HLFCNTL] pid=%d cpid=%d fd=%d mflags=0x%x lflags=0x%x path=%s\n", getpid(),
                        container_pid(), (int)a0, r, lf, p);
            }
            G_RET(c) = (uint64_t)(unsigned)lf;
            break;
        }
        // F_SETFL: Linux O_* -> macOS O_*
        if (lcmd == 4) {
            int la = (int)a2, mf = 0;
            if (la & 0x400) mf |= O_APPEND;
            if (la & 0x800) mf |= O_NONBLOCK;
            // APPEND/NONBLOCK/ASYNC
            if (la & 0x2000) mf |= O_ASYNC;
#if defined(__linux__) && defined(O_DIRECT)
            // Forward an O_DIRECT status-flag change straight to the real host fd (guest-arch G_O_DIRECT ->
            // host O_DIRECT). Previously the bit was dropped, so F_SETFL(O_DIRECT) wrongly returned success
            // without setting it (and a filesystem that rejects O_DIRECT never produced the EINVAL Linux does).
            if (la & G_O_DIRECT) mf |= O_DIRECT;
#endif
            // eventfd: record the guest's blocking/non-blocking intent in the shadow and NEVER clear the
            // host read end's O_NONBLOCK (the internal drains rely on it; clearing it would let a drain
            // block). Other flag changes still apply to the host fd. See vfs.c g_eventfd_gnb.
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_eventfd_peer[(int)a0]) {
                eventfd_guest_nb_set((int)a0, (la & 0x800) != 0);
                mf |= O_NONBLOCK; // keep host O_NONBLOCK on regardless of the guest's request
            }
            int r = fcntl((int)a0, F_SETFL, mf);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        // F_GETLK/SETLK/SETLKW: xlate struct flock + cmd
        if (lcmd == 5 || lcmd == 6 || lcmd == 7) {
            // macOS F_GETLK=7,SETLK=8,SETLKW=9
            int mc = lcmd == 5 ? F_GETLK : lcmd == 6 ? F_SETLK : F_SETLKW;
            uint8_t lf[32];
            // Linux order (SYSCALL_DEFINE3(fcntl) -> fcntl_getlk/setlk): the fd is validated (EBADF) BEFORE the
            // flock is copied in, so a bad fd wins over a bad pointer / bad l_whence. (LTP fcntl13: fcntl(-1,...).)
            if ((int)a0 < 0 || fcntl((int)a0, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            // The Linux struct flock at a2 (fields up to lf+24) is read directly and written back for F_GETLK;
            // validate the 32-byte struct before any deref so a bad pointer returns -EFAULT, not a crash. A guest
            // PROT_NONE flock buffer (LTP fcntl13 uses one) is force-mapped host-writable by hl, but
            // host_range_mapped rejects it via its internal gna_hit check so it still EFAULTs like Linux.
            if (guest_copy_from(lf, a2, sizeof(lf)) != (ssize_t)sizeof(lf)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            // l_whence must be SEEK_SET/SEEK_CUR/SEEK_END; Linux rejects anything else with EINVAL in
            // flock_to_posix_lock -- BEFORE the fd type is consulted, so it applies to a pipe fd too. (LTP fcntl13)
            {
                short whence;
                memcpy(&whence, lf + 2, sizeof(whence));
                if (whence != 0 && whence != 1 && whence != 2) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
            }
            // service advisory byte-range locks on regular files from the in-engine cross-process
            // table (no host round-trip). F_SETLKW blocks by poll-retry, interruptible by a deliverable
            // pending signal (g_pending/tpending, honouring the per-thread block mask) -> EINTR, exactly
            // as a real F_SETLKW returns. poslk_op returns 0 only for non-regular fds -> host path below.
            {
                int pout = 0, claimed;
                for (;;) {
                    claimed = poslk_op((int)a0, lcmd, lf, &pout);
                    if (!claimed) break; // not a regular file -> fall through to the host fcntl path
                    if (lcmd == 7 && pout == -EAGAIN) {
                        uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) |
                                     __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
                        int intr = 0;
                        for (int s = 1; s < 64; s++)
                            if ((p & (1ull << s)) && !(c->sigmask & (1ull << (s - 1)))) {
                                intr = 1;
                                break;
                            }
                        if (intr) {
                            G_RET(c) = (uint64_t)(-EINTR);
                            break;
                        }
                        struct timespec ts = {0, 1000000}; // 1 ms poll
                        nanosleep(&ts, NULL);
                        continue;
                    }
                    G_RET(c) = (uint64_t)(int64_t)pout;
                    if (lcmd == 5 && pout == 0 && guest_copy_to(a2, lf, sizeof(lf)) != (ssize_t)sizeof(lf))
                        G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                if (claimed) break; // handled in-engine (or interrupted); done
            }
            struct flock fl;
            // Linux flock: type/whence/pad/start@8/len@16/pid@24
            memset(&fl, 0, sizeof fl);
            short lt;
            memcpy(&lt, lf, sizeof(lt));
            // Linux RDLCK=0,WRLCK=1,UNLCK=2 -> macOS
            fl.l_type = lt == 0 ? F_RDLCK : lt == 1 ? F_WRLCK : F_UNLCK;
            memcpy(&fl.l_whence, lf + 2, sizeof(fl.l_whence));
            memcpy(&fl.l_start, lf + 8, sizeof(fl.l_start));
            memcpy(&fl.l_len, lf + 16, sizeof(fl.l_len));
            memcpy(&fl.l_pid, lf + 24, sizeof(fl.l_pid));
            int r = fcntl((int)a0, mc, &fl), e = errno;
            // F_GETLK writes the conflicting lock back
            if (r >= 0 && lcmd == 5) {
                short type = fl.l_type == F_RDLCK ? 0 : fl.l_type == F_WRLCK ? 1 : 2;
                int32_t pid = (int32_t)fl.l_pid;
                memcpy(lf, &type, sizeof(type));
                memcpy(lf + 2, &fl.l_whence, sizeof(fl.l_whence));
                memcpy(lf + 8, &fl.l_start, sizeof(fl.l_start));
                memcpy(lf + 16, &fl.l_len, sizeof(fl.l_len));
                memcpy(lf + 24, &pid, sizeof(pid));
                if (guest_copy_to(a2, lf, sizeof(lf)) != (ssize_t)sizeof(lf)) {
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)e) : (uint64_t)r;
            break;
        }
        // F_SETPIPE_SZ(1031)/F_GETPIPE_SZ(1032). The guest's non-O_DIRECT pipe fd is a REAL host pipe
        // (case 59 pipe()), so on a Linux host the kernel already implements these with exact Linux
        // semantics: power-of-two rounding (roundup_pow_of_two, NOT page rounding), a real capacity change
        // that the subsequent fill/EAGAIN reflects, EBUSY when shrinking below the currently buffered data,
        // and EBADF on a non-pipe fd (incl. an O_DIRECT pipe backed by a socketpair). Forward straight
        // through. The macOS build keeps the size emulation below (Darwin has no pipe-size fcntl), which
        // only records a number and never resizes the buffer, so it must stay guarded to that host.
        if (lcmd == 1031 || lcmd == 1032) {
#if defined(__linux__)
            long r = (lcmd == 1032) ? fcntl((int)a0, F_GETPIPE_SZ) : fcntl((int)a0, F_SETPIPE_SZ, (int)a2);
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)errno) : (uint64_t)(unsigned)r;
            break;
#else
            // Linux's pipe_fcntl first rejects a non-pipe object with EBADF, so validate the fd is a real
            // FIFO before fabricating a size -- otherwise a regular file/socket or bad fd was reported as a
            // pipe with a plausible size.
            struct stat pst;
            if (fstat((int)a0, &pst) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (!S_ISFIFO(pst.st_mode)) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (lcmd == 1031) {
                int want = (int)a2;
                long pg = (long)hl_linux_host_page_size();
                int rounded = (int)(((want + pg - 1) / pg) * pg);
                if (rounded < (int)pg) rounded = (int)pg;
                if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_pipesz[(int)a0] = rounded;
                G_RET(c) = (uint64_t)(unsigned)rounded;
                break;
            }
            // lcmd == 1032
            {
                int sz = ((int)a0 >= 0 && (int)a0 < HL_NFD && g_pipesz[(int)a0]) ? g_pipesz[(int)a0] : 65536;
                G_RET(c) = (uint64_t)(unsigned)sz;
                break;
            }
#endif
        }
#if defined(__linux__) && defined(F_GET_RW_HINT)
        // Write life-time hints F_GET_RW_HINT(1035)/F_SET_RW_HINT(1036)/F_GET_FILE_RW_HINT(1037)/
        // F_SET_FILE_RW_HINT(1038): the arg is a pointer to a uint64 hint. The guest fd is a real host fd,
        // so forward straight through to the host kernel (which owns the actual per-inode/per-fd hint) --
        // otherwise the do_fcntl default wrongly returns EINVAL where native returns the real hint. macOS
        // has no such command, so this stays Linux-only and the default switch keeps EINVAL there.
        if (lcmd >= 1035 && lcmd <= 1038) {
            uint64_t hint = 0;
            int is_get = lcmd == 1035 || lcmd == 1037;
            if (!a2 || (!is_get && guest_copy_from(&hint, a2, sizeof(hint)) != (ssize_t)sizeof(hint)) ||
                (is_get && guest_accessible_prefix(a2, sizeof(hint), HL_LOGICAL_VMA_WRITE) != sizeof(hint))) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            long r = fcntl((int)a0, lcmd, &hint);
            if (r >= 0 && is_get && guest_copy_to(a2, &hint, sizeof(hint)) != (ssize_t)sizeof(hint)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = r < 0 ? (uint64_t)(-(int64_t)errno) : (uint64_t)(unsigned)r;
            break;
        }
#endif
        int mcmd = lcmd;
        if (lcmd == 8)
            mcmd = F_SETOWN;
        else if (lcmd == 9)
            // owner cmds also swapped on macOS
            mcmd = F_GETOWN;
        else if (lcmd == 1030)
            mcmd = F_DUPFD_CLOEXEC;
        // memfd sealing: F_ADD_SEALS(1033) / F_GET_SEALS(1034) are honoured on an anonymous memfd (macOS has
        // no native seals, so the state + the F_SEAL_WRITE write-guard are emulated). For a NON-memfd the
        // guest fd is a real host fd, so on Linux forward the command to the host kernel, which knows whether
        // the underlying filesystem supports sealing: a regular tmpfs/shmem file reports its real seal state
        // (born F_SEAL_SEAL, so F_GET_SEALS -> 1 and a further F_ADD_SEALS -> EPERM) while a non-sealing fs
        // (ext4/overlay) answers EINVAL -- exactly like native. The old unconditional EINVAL shadowed that,
        // returning EINVAL where native tmpfs returns the real seal set. macOS keeps EINVAL (no host seals).
        else if (lcmd == 1033) { // F_ADD_SEALS(fd, seals)
            int fd = (int)a0;
            memfd_ensure_fd(fd);
            if (fd < 0 || fd >= HL_NFD || !g_memfd_is[fd]) {
#if defined(__linux__) && defined(F_ADD_SEALS)
                // A memfd not tracked here (e.g. a real host memfd whose host fd landed >= HL_NFD) would
                // otherwise let the HOST KERNEL decide the F_SEAL_WRITE-while-mapped verdict. That verdict
                // depends on whether the ENGINE happens to hold a writable host-side MAP_SHARED alias of the
                // object, which varies by host kernel/fs -- EBUSY on this VM but 0 on the CI runner (the
                // memfd-seal-busy divergence). Make F_SEAL_WRITE (0x8) deterministic from the engine's OWN
                // mapping registry, exactly like the emulated branch below: if a live MAP_SHARED mapping of
                // this object exists, refuse with EBUSY (Linux mm/shmem.c writable-mapping guard), host
                // independent. With no outstanding shared mapping, forward so the host still applies the seal
                // and reports the real seal state (F_GET_SEALS / a later writable-shared-mmap EPERM keep working).
                if (((int)a2 & 0x8) && filemap_has_shared_mapping(fd)) {
                    G_RET(c) = (uint64_t)(-EBUSY);
                    break;
                }
                int r = fcntl(fd, F_ADD_SEALS, (int)a2);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : 0;
#else
                G_RET(c) = (uint64_t)(-EINVAL);
#endif
                break;
            }
            if (g_memfd_seal[fd] & 0x1) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            } // already F_SEAL_SEAL'd
            // F_SEAL_WRITE (0x8) is refused with EBUSY while an outstanding MAP_SHARED mapping of this memfd
            // is live (Linux mm/shmem.c writable-mapping guard). F_SEAL_FUTURE_WRITE (0x10) only blocks new
            // writable maps, so it is unaffected.
            if (((int)a2 & 0x8) && !(g_memfd_seal[fd] & 0x8) && filemap_has_shared_mapping(fd)) {
                G_RET(c) = (uint64_t)(-EBUSY);
                break;
            }
            g_memfd_seal[fd] |= (int)a2 & 0x1f; // SEAL|SHRINK|GROW|WRITE|FUTURE_WRITE
            memfd_reg_set_fd(fd, g_memfd_seal[fd]);
            G_RET(c) = 0;
            break;
        } else if (lcmd == 1034) { // F_GET_SEALS(fd)
            int fd = (int)a0;
            memfd_ensure_fd(fd);
            if (fd < 0 || fd >= HL_NFD || !g_memfd_is[fd]) {
#if defined(__linux__) && defined(F_GET_SEALS)
                int r = fcntl(fd, F_GET_SEALS);
                G_RET(c) = r < 0 ? (uint64_t)(int64_t)(-errno) : (uint64_t)(unsigned)r;
#else
                G_RET(c) = (uint64_t)(-EINVAL);
#endif
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)g_memfd_seal[fd];
            break;
        } else if (lcmd == 1025) { // F_GETLEASE: report the tracked lease for this fd (F_UNLCK if none).
            // Returning a fixed value fabricated/erased lease state; consult g_lease so F_GETLEASE round-trips
            // whatever F_SETLEASE last set on this fd. Encoding: g_lease[fd] = type+1, 0 = no lease -> F_UNLCK(2).
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            int held = (fd < HL_NFD && g_lease[fd]) ? g_lease[fd] - 1 : 2; // stored type, else F_UNLCK
            G_RET(c) = (uint64_t)(unsigned)held;
            break;
        } else if (lcmd == 1024) { // F_SETLEASE(fd, F_RDLCK|F_WRLCK|F_UNLCK)
            int fd = (int)a0, arg = (int)a2;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (arg != 0 && arg != 1 && arg != 2) { // not F_RDLCK/F_WRLCK/F_UNLCK -> EINVAL (Linux)
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            struct stat lst;
            if (fstat(fd, &lst) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (!S_ISREG(lst.st_mode)) { // leases are only for regular files (Linux: EINVAL)
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            // A read lease (F_RDLCK) may only be taken on a descriptor NOT open for writing -- Linux
            // generic_add_lease returns EAGAIN when the inode has a writer, and the requesting fd itself
            // counts (an O_RDWR/O_WRONLY fd -> EAGAIN). This single-fd check matches the kernel exactly for
            // the common case. A write lease (F_WRLCK) requires the fd be the SOLE opener; hl cannot
            // enumerate other openers across guest processes, so it is tracked but its BREAK on a conflicting
            // open is never delivered (see syscall-compat.md). Both states round-trip through F_GETLEASE.
            if (arg == 0) { // F_RDLCK
                int fl = fcntl(fd, F_GETFL);
                if (fl >= 0 && (fl & O_ACCMODE) != O_RDONLY) {
                    G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
                    break;
                }
            }
            if (fd < HL_NFD) g_lease[fd] = (arg == 2) ? 0 : (int8_t)(arg + 1); // F_UNLCK clears
            G_RET(c) = 0;
            break;
        } else if (lcmd == 1026) { // F_NOTIFY(fd, DN_* mask): arm a real host directory-change watch.
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            int sig = (fd < HL_NFD && g_fsig[fd]) ? g_fsig[fd] : 0; // F_SETSIG override, else default SIGIO
            G_RET(c) = (uint64_t)(int64_t)dnotify_apply(fd, (uint32_t)a2, sig);
            break;
        } else if (lcmd == 10) { // F_SETSIG(fd, signo): record the signal for O_ASYNC/dnotify on this fd.
            int fd = (int)a0, sig = (int)a2;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            if (sig < 0 || sig > 64) { // 0 restores the SIGIO default; anything above the signal range is EINVAL
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (fd < HL_NFD) g_fsig[fd] = (uint8_t)sig;
#if defined(__linux__)
            // The guest fd is a REAL host fd; forward F_SETSIG so the host's own O_ASYNC signal-driven I/O
            // delivers the requested signal (with SI_SIGIO siginfo) instead of the default SIGIO, which the
            // engine then routes to the guest. Without this the custom async signal is silently ignored and
            // an app arming F_SETSIG waits for a signal that never comes. g_fsig is still recorded for the
            // engine-serviced dnotify path (F_NOTIFY), which raises the signal itself. Errors are non-fatal.
            (void)fcntl(fd, F_SETSIG, sig);
#endif
            G_RET(c) = 0;
            break;
        } else if (lcmd == 11) { // F_GETSIG(fd): the signal set by F_SETSIG (0 = default SIGIO).
            int fd = (int)a0;
            if (fd < 0 || fcntl(fd, F_GETFD) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)((fd < HL_NFD) ? g_fsig[fd] : 0);
            break;
        }
        // A command this kernel does not recognize is EINVAL (Linux do_fcntl default), NOT forwarded to
        // macOS -- whose fcntl cmd numbering DIVERGES, so a stray Linux cmd# would mean a different op there.
        // Everything valid was handled above or is one of these benign pass-throughs; reject the rest. (LTP
        // fcntl13 F_BADCMD=999.) 10/11=SETSIG/GETSIG, 15/16/17=SET/GETOWN_EX+GETOWNER_UIDS, 36/37/38=OFD locks.
        switch (lcmd) {
        case 0:
        case 1:
        case 2:
        case 8:
        case 9:
        case 10:
        case 11:
        case 15:
        case 16:
        case 17:
        case 36:
        case 37:
        case 38:
        case 1030: break; // recognized Linux command -> proceed to the host fcntl
        default: G_RET(c) = (uint64_t)(-EINVAL); goto fcntl_done;
        }
        if ((lcmd == 0 || lcmd == 1030) && fd_virt_reserve((int)a0, &fdvis) != 0) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            goto fcntl_done;
        }
        if (lcmd == 0 || lcmd == 1030) engine_fd_vacate((int)a2);
        if ((lcmd == 0 || lcmd == 1030) && bound_sentinel_vacate((int)a2) != 0) {
            proc_fdvis_reservation_cancel(&fdvis);
            G_RET(c) = (uint64_t)(-EMFILE);
            goto fcntl_done;
        }
        int r = fcntl((int)a0, mcmd, a2);
        if (lcmd == 0 || lcmd == 1030) r = nofile_gate(r); // F_DUPFD(_CLOEXEC): EMFILE past the guest fd cap
        if (r < 0 && (lcmd == 0 || lcmd == 1030)) proc_fdvis_reservation_cancel(&fdvis);
        if (r >= 0 && (lcmd == 0 || lcmd == 1030) && r < HL_NFD && (int)a0 >= 0 && (int)a0 < HL_NFD) {
            // F_DUPFD(_CLOEXEC)
            strcpy(g_fdpath[r], g_fdpath[(int)a0]);
            strcpy(g_proc_text_desc[r], g_proc_text_desc[(int)a0]);
            g_proc_text_ro[r] = g_proc_text_ro[(int)a0];
            g_pagemap_fd[r] = g_pagemap_fd[(int)a0];
            fd_carry_sock(r, (int)a0);
            fd_carry_virt(r, (int)a0, &fdvis); // eventfd/timerfd share the same object across a dup
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
    fcntl_done:
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_ioctl(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 29: {
        // ioctl(fd, req, arg). Almost every request (termios/winsize/job-control) is owned by svc_fs below;
        // we only claim FIOASYNC here. On a SOCKET/PIPE Linux's FIOASYNC toggles signal-driven I/O and
        // returns 0, but svc_fs's terminal-centric handler answers ENOTTY for it -- and nginx's master arms
        // ioctl(listenfd, FIOASYNC, &on) on its listen socket, so an ENOTTY aborts worker startup and every
        // connection then hangs. Translate it to the O_ASYNC file-status flag (fcntl), exactly like Linux,
        // and defer every other request to svc_fs by returning "not handled".
        if (a1 != 0x5452) return 0; // not FIOASYNC -> let svc_fs handle it
        int on = 0;
        if (!a2 || guest_copy_from(&on, a2, sizeof(on)) != (ssize_t)sizeof(on)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int fl = fcntl((int)a0, F_GETFL);
        if (fl < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        fl = on ? (fl | O_ASYNC) : (fl & ~O_ASYNC);
        G_RET(c) = fcntl((int)a0, F_SETFL, fl) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}

static int svc_pipe2(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 59: {
        // pipe2(fds, flags). O_DIRECT requests "packet mode": each write is a distinct record that reads
        // back whole, never coalesced. macOS pipes can't do this, but an AF_UNIX SOCK_DGRAM socketpair
        // preserves message boundaries exactly, so back an O_DIRECT pipe with one (SOCK_SEQPACKET would be
        // closer but macOS PF_LOCAL doesn't support it). A plain pipe is fine for the non-O_DIRECT case.
        int fds[2], fl = (int)a1;
        // Validate flags exactly as Linux (fs/pipe.c): only O_CLOEXEC | O_NONBLOCK | O_DIRECT |
        // O_NOTIFICATION_PIPE are defined; any other bit -> EINVAL (mirrors eventfd2, case 19). A bogus flag
        // (e.g. 0x4) previously slipped through and pipe2 wrongly succeeded.
        if ((unsigned)fl & ~(unsigned)(0x800u | 0x80000u | (unsigned)G_O_DIRECT | 0x800000u)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // a0 receives the two result fds (8 bytes). Validate it BEFORE creating the pipe so a bad pointer
        // returns -EFAULT without leaking the freshly-opened fds (and without faulting the engine).
        if (guest_accessible_prefix(a0, 2 * sizeof(int), HL_LOGICAL_VMA_WRITE) != 2 * sizeof(int)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int mk = (fl & G_O_DIRECT) ? socketpair(AF_UNIX, SOCK_DGRAM, 0, fds) : pipe(fds);
        if (mk < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // Either new fd past the guest's soft RLIMIT_NOFILE -> EMFILE (the host table is far larger). Close
        // both so no descriptor leaks, exactly as Linux fails a pipe2 that would exceed the limit.
        {
            int cap = guest_nofile_cur();
            if (fds[0] >= cap || fds[1] >= cap) {
                close(fds[0]);
                close(fds[1]);
                G_RET(c) = (uint64_t)(-EMFILE);
                break;
            }
        }
        if (fl & 0x80000) {
            fcntl(fds[0], F_SETFD, FD_CLOEXEC);
            fcntl(fds[1], F_SETFD, FD_CLOEXEC);
        }
        if (fl & 0x800) {
            fcntl(fds[0], F_SETFL, O_NONBLOCK);
            fcntl(fds[1], F_SETFL, O_NONBLOCK);
        }
        if (proc_fdvis_publish_pipe_pair(fds[0], fds[1]) != 0) {
            close(fds[0]);
            close(fds[1]);
            G_RET(c) = (uint64_t)(-EMFILE);
            break;
        }
        if (guest_copy_to(a0, fds, sizeof(fds)) != (ssize_t)sizeof(fds)) {
            close(fds[0]);
            close(fds[1]);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // An O_DIRECT pipe is backed by a DGRAM socketpair (above). Like a real pipe it must report EOF
        // when the write end closes, but macOS DGRAM sockets don't -- mark both ends so close() sends a
        // zero-length EOF datagram and read() coerces the peer-closed ECONNRESET to 0. (See netns.c.)
        if ((fl & G_O_DIRECT)) {
            if (seq_ref_pair(fds[0], fds[1]) != 0) {
                int e = errno;
                proc_fdvis_close(fds[0]);
                proc_fdvis_close(fds[1]);
                close(fds[0]);
                close(fds[1]);
                G_RET(c) = (uint64_t)(-e);
                break;
            }
            if (fds[0] >= 0 && fds[0] < HL_NFD) {
                g_sock_seqpacket[fds[0]] = 1;
                g_sock_pair_peer[fds[0]] = fds[1] + 1;
            }
            if (fds[1] >= 0 && fds[1] < HL_NFD) {
                g_sock_seqpacket[fds[1]] = 1;
                g_sock_pair_peer[fds[1]] = fds[0] + 1;
            }
            sock_pair_identity_assign(fds[0], fds[1]);
        }
        G_RET(c) = 0;
        break;
    }
    // fsync -- durability policy (S3DB_DURABILITY): default/fast == plain fsync() (legacy path)
    // A RAM-backed scratch file is anonymous/private: fsync has no observable effect -> 0.
    default: return 0;
    }
    return svc_done(c);
}

static int svc_fsync(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 82: G_RET(c) = memf_get((int)a0) ? 0 : s3db_sync_fd((int)a0); break;
    // fdatasync -> fsync (no macOS fdatasync); same durability policy
    default: return 0;
    }
    return svc_done(c);
}

static int svc_fdatasync(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 83: G_RET(c) = memf_get((int)a0) ? 0 : s3db_sync_fd((int)a0); break;
    // copy_file_range(fdin,offin*,fdout,offout*,len,flags)
    default: return 0;
    }
    return svc_done(c);
}

static int svc_copy_file_range(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 285: {
        int fdin = (int)a0, fdout = (int)a2;
        // Linux defines NO flags for copy_file_range: SYSCALL_DEFINE6 rejects a non-zero `flags` with
        // -EINVAL up front. The engine ignored a5 and copied the bytes anyway.
        if (a5) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        memf_materialize(fdin); // copy_file_range moves bytes via the real fds -> flush RAM caches first
        memf_materialize(fdout);
        size_t len = (size_t)a4, done = 0;
        int err = 0;
        off_t input_offset = 0, output_offset = 0;
        off_t *poi = a1 != 0 ? &input_offset : NULL, *poo = a3 != 0 ? &output_offset : NULL;
        // off_in (a1) / off_out (a3) are read here and written back below -> validate before any deref so a
        // bad pointer returns -EFAULT instead of faulting the engine (and before any bytes are copied).
        if (poi && guest_copy_from(poi, a1, sizeof(*poi)) != (ssize_t)sizeof(*poi)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (poo && guest_copy_from(poo, a3, sizeof(*poo)) != (ssize_t)sizeof(*poo)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        off_t oi = poi ? *poi : -1, oo = poo ? *poo : -1;
        // Linux rejects a same-file copy whose source and destination ranges overlap (EINVAL) rather than
        // corrupting the file by copying through the overlap. Compare the underlying file identity and the
        // effective start offsets (explicit offset, else the current file position for a NULL offset).
        if (len > 0) {
            struct stat si, so;
            if (fstat(fdin, &si) == 0 && fstat(fdout, &so) == 0 && si.st_dev == so.st_dev && si.st_ino == so.st_ino) {
                off_t is = poi ? *poi : lseek(fdin, 0, SEEK_CUR);
                off_t os = poo ? *poo : lseek(fdout, 0, SEEK_CUR);
                if (is >= 0 && os >= 0 && is < os + (off_t)len && os < is + (off_t)len) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
            }
        }
        char cb[8192];
        while (done < len) {
            size_t chunk = (len - done > sizeof cb) ? sizeof cb : len - done;
            ssize_t r = (oi >= 0) ? pread(fdin, cb, chunk, oi) : read(fdin, cb, chunk);
            if (r < 0) {
                err = errno;
                break;
            }
            if (r == 0) break;
            ssize_t w = (oo >= 0) ? pwrite(fdout, cb, (size_t)r, oo) : write(fdout, cb, (size_t)r);
            if (w < 0) {
                err = errno;
                break;
            }
            done += (size_t)w;
            if (oi >= 0) oi += w;
            if (oo >= 0) oo += w;
            if (w < r) break;
        }
        if ((poi && guest_copy_to(a1, &oi, sizeof(oi)) != (ssize_t)sizeof(oi)) ||
            (poo && guest_copy_to(a3, &oo, sizeof(oo)) != (ssize_t)sizeof(oo))) {
            G_RET(c) = done != 0 ? (uint64_t)done : (uint64_t)(-EFAULT);
            break;
        }
        hl_fdcache_fd_evict(fdout);
        G_RET(c) = (done == 0 && err) ? (uint64_t)(-(int64_t)err) : (uint64_t)done;
        break;
    }
    // preadv/pwritev: struct iovec layout is identical Linux<->macOS
    default: return 0;
    }
    return svc_done(c);
}

static int svc_sync_file_range(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                     uint64_t a4, uint64_t a5) {
    (void)a0; (void)a1; (void)a2; (void)a3; (void)a4; (void)a5;
    switch (nr) {
    case 84:
        G_RET(c) = memf_get((int)a0) ? 0 : s3db_sync_fd((int)a0);
        break; // sync_file_range -> fsync (no-op for RAM scratch)
    default: return 0;
    }
    return svc_done(c);
}
