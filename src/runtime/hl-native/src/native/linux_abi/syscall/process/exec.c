// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int exec_resolve_proc_path(uint64_t *path) {
    static char resolved_path[4200];
    char link_path[4200];
    const char *guest_path = (const char *)(uintptr_t)*path;
    if (proc_self_exe(guest_path, link_path, sizeof link_path)) {
        snprintf(resolved_path, sizeof resolved_path, "%s", link_path);
        *path = (uint64_t)(uintptr_t)resolved_path;
        return 0;
    }
    int fd = procfd_num(guest_path);
    if (fd < 0) return 0;
    char host_path[4200];
    if (hl_native_fd_path(fd, host_path, sizeof host_path) != 0 || !host_path[0]) return 0;
    if (g_rootfs) {
        char mapped_path[4200];
        int mapped = guest_from_host_raw(host_path, mapped_path, sizeof mapped_path);
        if (mapped <= 0) return mapped < 0 ? mapped : -EACCES;
        snprintf(resolved_path, sizeof resolved_path, "%s", mapped_path);
    } else {
        snprintf(resolved_path, sizeof resolved_path, "%s", host_path);
    }
    *path = (uint64_t)(uintptr_t)resolved_path;
    return 0;
}

static int exec_writable_fd_matches(int fd, const struct stat *image) {
    if (exec_fd_is_engine(fd)) return 0;
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0 || (flags & O_ACCMODE) == O_RDONLY) return 0;
    struct stat open_file;
    return fstat(fd, &open_file) == 0 && open_file.st_dev == image->st_dev && open_file.st_ino == image->st_ino;
}

static int exec_image_is_write_open_scan(const struct stat *image, int limit) {
    if (limit < 0 || limit > (1 << 20)) limit = 4096;
    for (int fd = 0; fd < limit; fd++)
        if (exec_writable_fd_matches(fd, image)) return 1;
    return 0;
}

// A real kernel pins an executable against writable opens while committing exec. This engine reloads the image
// in-process instead, so reproduce the pre-commit ETXTBSY check against every live guest open description. Match
// by inode rather than tracked pathname: dup aliases, renamed files, unlinked files, and overlay-resolved names must
// all retain the same text-busy identity. This check intentionally precedes thread_exec_owner_handoff; a failed exec
// must not retire sibling guest threads. Guest descriptor operations have no process-wide table lock today, so this
// is the same live-table snapshot used by the CLOEXEC sweep below rather than a claim of atomic host exec exclusion.
static int exec_image_is_write_open(const struct stat *image) {
    size_t need = 0;
    if (!hl_host_process_fds(getpid(), NULL, 0, &need)) return exec_image_is_write_open_scan(image, getdtablesize());
    size_t capacity = need <= SIZE_MAX - 32 ? need + 32 : need;
    hl_host_process_fd *fds = capacity != 0 ? malloc(capacity * sizeof *fds) : NULL;
    if (!fds) return exec_image_is_write_open_scan(image, getdtablesize());
    size_t count = 0;
    if (!hl_host_process_fds(getpid(), fds, capacity, &count) || count > capacity) {
        free(fds);
        return exec_image_is_write_open_scan(image, getdtablesize());
    }
    int busy = 0;
    for (size_t index = 0; index < count && !busy; index++) {
        if ((fds[index].flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0) continue;
        busy = exec_writable_fd_matches(fds[index].descriptor, image);
    }
    free(fds);
    return busy;
}

static int exec_validate_image(const char *path, int *script_image) {
    struct stat status;
    *script_image = 0;
    if (stat(path, &status) == 0) {
        if (S_ISDIR(status.st_mode)) return -EACCES;
        if (S_ISREG(status.st_mode)) {
            if (exec_image_is_write_open(&status)) return -ETXTBSY;
            FILE *image = fopen(path, "rb");
            if (image) {
                unsigned char header[20] = {0};
                size_t got = fread(header, 1, sizeof header, image);
                fclose(image);
                int is_elf = got >= 4 && header[0] == 0x7f && header[1] == 'E' && header[2] == 'L' && header[3] == 'F';
                *script_image = got >= 2 && header[0] == '#' && header[1] == '!';
                if (!is_elf && !*script_image) return -ENOEXEC;
                if (is_elf &&
                    (got < 20 || header[4] != 2 || (unsigned)(header[18] | (header[19] << 8)) != HL_EXEC_ELF_MACHINE))
                    return -ENOEXEC;
            }
        }
    }
    return access(path, F_OK) == 0 ? 0 : -ENOENT;
}

static int exec_collect_argv(uint64_t argv_address, char **argv, int *argc) {
    size_t argument_bytes = 0;
    char *probe = malloc(HL_EXEC_ARGUMENT_BYTES + 1u);
    if (!probe) return -ENOMEM;
    *argc = 0;
    int error = 0;
    while (argv_address && *argc < HL_MAXARGV - 1) {
        uint64_t guest_argument = 0;
        if (guest_copy_from(&guest_argument, argv_address + (uint64_t)*argc * sizeof guest_argument,
                            sizeof guest_argument) != sizeof guest_argument) {
            error = -EFAULT;
            break;
        }
        if (!guest_argument) break;
        size_t remaining = HL_EXEC_ARGUMENT_BYTES - argument_bytes;
        int length = guest_copy_string(probe, remaining + 1, guest_argument);
        if (length == -EFAULT) {
            error = -EFAULT;
            break;
        }
        if (length < 0 || (size_t)length + 1 > remaining) {
            error = -E2BIG;
            break;
        }
        argument_bytes += (size_t)length + 1;
        argv[(*argc)++] = (char *)nonpie_p(guest_argument);
    }
    free(probe);
    argv[*argc] = NULL;
    return error;
}

static int svc_proc_221(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4, uint64_t a5) {
    switch (nr) {
    case 221: {
        memf_materialize_all(); // non-CLOEXEC scratch fds survive exec -> flush RAM into the real files
        // Linux comm = last component of the path PASSED to execve, captured BEFORE the /proc magic-link
        // rewrite below and before binfmt_script (execve("/proc/self/exe") -> comm "exe"; "./run.sh"
        // keeps "run.sh"). Applied at the committed point further down, only once the exec cannot fail.
        char comm_src[256];
        int script_image = 0;
        {
            const char *cb = (const char *)a0, *cs = cb ? strrchr(cb, '/') : NULL;
            const char *name = cs ? cs + 1 : (cb ? cb : "");
            size_t name_length = strnlen(name, sizeof comm_src - 1);
            memcpy(comm_src, name, name_length);
            comm_src[name_length] = 0; // Linux path components are NAME_MAX; set_guest_comm applies TASK_COMM_LEN
        }
        // exec THROUGH the /proc magic links: execve("/proc/self/exe") (busybox re-exec, daemons,
        // test harnesses) and execve("/proc/self/fd/N") (glibc fexecve fallback) must exec the link's
        // TARGET -- the rootfs /proc is empty, so resolving them as ordinary paths ENOENTed.
        int path_error = exec_resolve_proc_path(&a0);
        if (path_error) {
            G_RET(c) = (uint64_t)(int64_t)path_error;
            break;
        }
        char pb[4200];
        const char *p =
            // resolve the exec path through the SAME resolver openat uses (atpath): overlay-aware
            // (upper then lowers), bind-mount/volume aware, AND relative-path aware -- a RELATIVE exec
            // (`./x`, `./binary` from `go build`/`make`, `./script`) is joined to the guest cwd (g_cwd),
            // not the host cwd. The old xresolve_overlay bailed on any non-'/' path and returned it raw,
            // so `./x` was access()'d against the host process cwd (never the mounted guest cwd) -> ENOENT.
            atpath(-100, (const char *)a0, pb, sizeof pb, 0);
        // execve(2) error classification, matching Linux binfmt semantics, applied to the resolved target:
        // a directory is EACCES, and a regular file that is neither an ELF this engine can translate nor a
        // #! script is ENOEXEC. A missing path stat()s ENOENT and falls through to the access() check below.
        // A bare-mode (no-rootfs) launch used to additionally require the target to BE the launched image,
        // collapsing every other target to ENOENT -- `sh -c 'exec bash'` reported "not found" while the same
        // binary launched directly ran. The engine reads the target through the same host services either
        // way, so the gate isolated nothing; an unreadable or unloadable image still fails right here.
        int image_error = exec_validate_image(p, &script_image);
        if (image_error) {
            G_RET(c) = (uint64_t)(int64_t)image_error;
            break;
        }
        char *argv[HL_MAXARGV]; // Linux allows far more than 255 args within ARG_MAX -- a fixed 256 silently
        int ac = 0;             // dropped the tail (a different command ran, and /proc/self/cmdline diverged)
        int argument_error = exec_collect_argv(a1, argv, &ac);
        if (argument_error) {
            G_RET(c) = (uint64_t)(int64_t)argument_error;
            break;
        }
        // Forward the guest's ACTUAL environment across the exec: build_stack rebuilds the new process env
        // from HL_GUEST_ENV, so serialize envp (a2) into it NOW while guest memory is still mapped. A guest
        // that set/modified env vars (FOO=bar, a tweaked PATH) thus sees them survive; a NULL envp keeps the
        // container's HL_GUEST_ENV defaults (a2 is NOT rebased by the dispatch redirect, unlike a0/a1).
        exec_forward_env(a2);
        // Capture the guest-absolute exec path NOW (a0 is still mapped) so /proc/self/exe can name the new
        // image after the teardown below. ld.so resolves a binary's $ORIGIN (DT_RUNPATH) via readlink of
        // /proc/self/exe; a stale value makes an exec'd dynamic binary fail to find its own libraries (e.g.
        // rustup's proxy execs the real rustc, whose RUNPATH $ORIGIN/../lib must point into the toolchain).
        char gexe[4200];
        if (g_rootfs)
            abs_guest(-100, (const char *)a0, gexe, sizeof gexe);
        else
            // bare mode: abs_guest would join the untracked g_cwd ("/"); keep the raw path and let
            // exe_canon below join the LIVE host cwd (the engine chdir()s for real without a rootfs)
            snprintf(gexe, sizeof gexe, "%s", (const char *)a0);
        // shebang: exec the #! interpreter instead (resolve_shebang_chain is shared with the initial loader).
        // RECURSIVE -- the interpreter may itself be a #! script (e.g. /usr/bin/env -> coreutils multicall);
        // resolve the whole chain (Linux binfmt_script, up to SHEBANG_MAX levels) and load the FINAL interp.
        char sh_store[SHEBANG_MAX * 2][256], shpb[4200];
        char *na[HL_MAXARGV];
        int nn = 0;
        // Linux passes the execve path (a0) as the script-path arg; the original argv[0] is discarded.
        na[nn++] = (char *)a0;
        for (int i = 1; i < ac && nn < HL_MAXARGV - 1; i++)
            na[nn++] = argv[i];
        na[nn] = NULL;
        const char *sh_finalhost;
        int sh_new = resolve_shebang_chain(na, nn, HL_MAXARGV, p, sh_store, shpb, sizeof shpb, &sh_finalhost);
        if (sh_new < 0) {
            // too many nested #! -> ELOOP. `-ELOOP` is the host macOS errno 62; svc_done's boundary translation
            // maps it to Linux ELOOP (40) at the syscall boundary, exactly like the vfs symlink-loop path.
            G_RET(c) = (uint64_t)(-ELOOP);
            break;
        }
        if (script_image && sh_new == nn) {
            // A file beginning with #! is accepted only when binfmt_script resolved a non-empty
            // interpreter.  Falling through to the ELF loader after an empty/malformed shebang
            // commits the exec, tears down the old image, and interprets text bytes as an ELF.
            G_RET(c) = (uint64_t)(int64_t)-ENOEXEC;
            break;
        }
        if (sh_new != nn) { // a shebang chain resolved -> load the final interpreter, not the script
            snprintf(gexe, sizeof gexe, "%s", na[0]); // /proc/self/exe names the interpreter
            // the final interp host is already overlay-resolved (the #! interp, e.g. /bin/sh, may live only
            // in a read-only lower in a fresh container; the chain resolves each level through the overlay)
            p = sh_finalhost;
            if (access(p, F_OK) != 0) {
                G_RET(c) = (uint64_t)(-2);
                break;
            }
            for (int i = 0; i <= sh_new; i++)
                argv[i] = na[i];
            ac = sh_new;
        }
        // /proc/self/exe must name the new image as an ABSOLUTE, CANONICAL guest path -- fold "."/".."
        // and resolve symlinks to the backing file (an exec of /bin/sh -> busybox reports /bin/busybox,
        // and a relative "./x" exec reports "<cwd>/x", exactly like Linux d_path). glibc static-pie
        // asserts on a non-canonical value at startup (dl-origin.c).
        {
            char gcanon[4200];
            exe_canon(gexe, gcanon, sizeof gcanon);
            snprintf(gexe, sizeof gexe, "%s", gcanon);
        }
        // Committed to the exec now (all ENOENT early-returns are behind us). execve makes the process
        // single-threaded -- the kernel terminates every OTHER thread in the group -- so before we flush the
        // address space and CLOEXEC fds below, tear down any sibling guest threads (a Go all-threads setuid,
        // e.g. gosu/su-exec, leaves netpoller/idle Ms live; a surviving M would run the old image against the
        // freed state). Blocks until all peers have left run_guest, so the teardown below is race-free.
        if (!thread_exec_owner_handoff(c)) {
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        thread_exit_others(c);
        // All failure returns are behind us: Linux releases a vfork parent
        // when exec commits, before the new image begins executing.
        vfork_release_parent();
        set_guest_comm(comm_src); // comm := basename of the exec'd NAME (captured pre-rewrite above)
        cred_after_exec(p);       // apply set-id ownership, recompute caps, and clear KEEPCAPS
#ifdef PCACHE_SAVE_HOOK
        // the exec below flushes this image's translated arena and RE-KEYS the cache identity for
        // the new image (pcache_exec_reload), so the exit-time save can never again cover this epoch.
        // Persist the outgoing image under its OWN (current) key now -- e.g. the `sh` of a `sh -c tar`
        // chain, which otherwise never gets cached because the shell always ends in an exec. Every save
        // refusal gate applies unchanged (fork child, restored-from-cache, poisoned, SMC, mixed-base); a
        // restored epoch records its revival stats instead (pcache_warm_note, the policy input).
        // Single-threaded here by construction (thread_exit_others above), so the snapshot cannot tear.
        PCACHE_SAVE_HOOK;
#endif
        // emulate the kernel's close-on-exec sweep. No real host exec runs below -- we re-load the new image
        // in this same process -- so FD_CLOEXEC fds must be closed by hand or they leak into the new program.
        exec_close_cloexec();
        sysv_after_exec(); // detach SysV shm + clear semadj across execve (registry itself survives)
        // Tear down the inherited guest address space before loading the new image: a post-fork exec
        // otherwise keeps the parent's DENSE layout, and load_elf must bias a non-PIE ET_EXEC off its
        // fixed vaddr (__PAGEZERO blocks the low 4 GB) -> its baked absolute refs collide -> SIGSEGV.
        // argv + path live in guest memory we're about to munmap, so copy them to the host heap first.
        char *xpath = strdup(p);
        char *xargv[HL_MAXARGV];
        for (int i = 0; i < ac && i < HL_MAXARGV - 1; i++)
            xargv[i] = strdup(argv[i]);
        xargv[ac < HL_MAXARGV - 1 ? ac : HL_MAXARGV - 1] = NULL;
        bound_mapping_reset();
        hl_gmap_reset();
        gna_reset();                   // the old image's PROT_NONE ranges are gone with its address space
        hl_gmap_lock_reset();          // ... and so are its mlock'd ranges (VmLck resets across execve)
        g_nonpie_lo = g_nonpie_hi = 0; // reset; load_elf re-sets it iff the new main image is non-PIE
        p = xpath;
        for (int i = 0; i < ac && i < HL_MAXARGV - 1; i++)
            argv[i] = xargv[i];
        argv[ac < HL_MAXARGV - 1 ? ac : HL_MAXARGV - 1] = NULL;
        struct loaded lm;
        char pc_ihost[4200];
        const char *pc_interp_host = NULL;
        (void)pc_ihost;
        (void)pc_interp_host;
#ifdef PCACHE_EXEC_HOOKS
        pcache_exec_force_main(); // map the new image at the fixed VA so its cached arena is reusable
#endif
        load_elf(p, &lm, NULL);
        uint64_t jump = lm.entry, at_base = 0;
        char interp[256];
        if (elf_interp(p, interp, sizeof interp) == 0) {
            char ib[4200];
            // follow+confine ld.so symlink (through the overlay)
            const char *ih = xresolve_overlay(interp, ib, sizeof ib);
#ifdef PCACHE_EXEC_HOOKS
            snprintf(pc_ihost, sizeof pc_ihost, "%s", ih); // outlive `ib` for the cache id below
            pc_interp_host = pc_ihost;
            pcache_exec_force_interp();
#endif
            struct loaded li;
            load_elf(ih, &li, NULL);
            jump = li.entry;
            at_base = li.base;
        }
        g_cp = g_cache;
        /* Translation-map visibility is generation-tagged. Clearing only the
           record payload leaves the old generation slots logically live and
           lets the new exec image observe zero/stale translation records. */
        map_clear();
        // flush old translations
        pend_reset();
        memset(g_ibtc, 0, sizeof g_ibtc);
#ifdef PCACHE_EXEC_HOOKS
        // the new image is loaded + the arena is flushed -> try to restore its warm translated arena
        // from the persistent cache (this is what makes the go-build fork+execve storm fast). Graceful MISS
        // translates fresh + saves on exit.
        pcache_exec_reload(p, pc_interp_host, argv[0], jump);
#endif
        // execve is a wholesale code-cache flush (g_cp reset + g_map/g_ibtc zeroed above), so it must ALSO
        // run the per-arch wholesale-flush hook the dispatcher uses (jit/dispatch.c) -- not just the lighter
        // fork/exec G_SHADOW_RESET. On x86 that hook drops the 2-way g_xibtc (G_SHADOW_RESET is a NO-OP there,
        // so g_xibtc was surviving execve); on aarch64 it resets the §B shadow stack. Without it a forked
        // child that execve's a new image (apt http method / gzip / cc1 / git child) keeps the OLD image's
        // g_xibtc entries -- keyed by guest PC the new image REUSES, bodies pointing into the freed cache --
        // and an indirect branch resolves into garbage host code -> SIGSEGV/SIGBUS (/ /).
        G_SHADOW_CLEAR(c);
        // POSIX execve resets CAUGHT signal handlers to SIG_DFL (SIG_IGN stays ignored). Without this, a
        // handler the calling shell installed (e.g. busybox sh's SIGCHLD job-control handler) survives into
        // the new image and is later delivered to a now-garbage handler address -> crash (redis/valkey run
        // via `sh -c …`). handler>1 == a real caught handler; 0=DFL, 1=IGN.
        for (int s = 1; s < 65; s++)
            if (g_sigact[s].handler > 1) {
                g_sigact[s].handler = 0;
                g_sigact[s].flags = 0;
                g_sigact[s].mask = 0;
            }
        uint64_t heap;
        if (hl_gmap_map_anonymous(0, 256u << 20, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE,
                                  &heap) != HL_STATUS_OK)
            _exit(127);
        brk_lo = brk_cur = heap;
        brk_hi = brk_lo + (256u << 20);
        // Publish the new image's exec path BEFORE build_stack: build_stack points AT_EXECFN at
        // g_exe_path (the canonical guest exec pathname), and /proc/self/exe reads it too.
        snprintf(g_exe_path_store, sizeof g_exe_path_store, "%s", gexe); // /proc/self/exe -> the new image
        g_exe_path = g_exe_path_store;
        uint64_t sp = build_stack(ac, argv, &lm, at_base);
        proc_reg_publish(gexe, ac, argv); // republish the process table entry (comm/argv changed on exec)
        free(xpath);
        for (int i = 0; i < ac && i < HL_MAXARGV - 1; i++) // mirror the strdup loop bound above; a 255 cap
            free(xargv[i]);                                // leaked xargv[255..ac-1] on every argc>255 execve
        G_RESET_REGS(c);
        c->nzcv = 0;
        G_TLS(c) = 0;
        G_SP(c) = sp;
        G_PC(c) = jump;
        // jump to new program; don't advance pc
        c->redirect = 1;
        break;
    }
    // wait4(pid, *status, opts, *rusage)
    default: return 0;
    }
    return 1;
}
