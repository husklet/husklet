// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int exec_resolve_proc_path(uint64_t *path, int *self_executable) {
    static char resolved_path[4200];
    char link_path[4200];
    const char *guest_path = (const char *)(uintptr_t)*path;
    *self_executable = 0;
    if (proc_self_exe(guest_path, link_path, sizeof link_path)) {
        snprintf(resolved_path, sizeof resolved_path, "%s", link_path);
        *path = (uint64_t)(uintptr_t)resolved_path;
        *self_executable = 1;
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
    if (hl_linux_writable_identity_open(g_linux_box, (uint64_t)image->st_dev, (uint64_t)image->st_ino)) return 1;
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
        busy = exec_writable_fd_matches(fds[index].descriptor, image);
    }
    free(fds);
    return busy;
}

typedef struct exec_image {
    int descriptor;
    struct stat status;
    hl_dac_snapshot dac;
    hl_linux_image bytes;
    int script;
    char path[4200];
} exec_image;

static void exec_image_release(exec_image *image) {
    if (image == NULL) return;
    hl_linux_image_release(&image->bytes);
    if (image->descriptor >= 0) close(image->descriptor);
    memset(image, 0, sizeof *image);
    image->descriptor = -1;
}

static int exec_image_adopt(int descriptor, const char *path, exec_image *image) {
    if (descriptor < 0 || path == NULL || image == NULL) {
        if (descriptor >= 0) close(descriptor);
        return -ENOENT;
    }
    memset(image, 0, sizeof *image);
    image->descriptor = -1;
#ifdef O_PATH
    int descriptor_flags = fcntl(descriptor, F_GETFL);
    if (descriptor_flags >= 0 && (descriptor_flags & O_PATH) != 0) {
        char descriptor_path[64];
        snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", descriptor);
        int readable = open(descriptor_path, O_RDONLY | O_CLOEXEC);
        close(descriptor);
        if (readable < 0) return -errno;
        descriptor = readable;
    }
#endif
    image->descriptor = descriptor;
    if (fstat(descriptor, &image->status) != 0) {
        int error = -errno;
        exec_image_release(image);
        return error;
    }
    if (!S_ISREG(image->status.st_mode)) {
        exec_image_release(image);
        return -EACCES;
    }
    stat_virt_ids(&image->status, NULL, descriptor, &image->dac.uid, &image->dac.gid);
    image->dac.mode = (uint32_t)stat_virt_mode(&image->status, NULL, descriptor);
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    if (hl_dac_authorize_access(&image->dac, &credentials, HL_DAC_EXECUTE) != 0) {
        exec_image_release(image);
        return -EACCES;
    }
    if (exec_image_is_write_open(&image->status)) {
        exec_image_release(image);
        return -ETXTBSY;
    }
    if (hl_linux_image_read_fd(descriptor, &image->bytes) != 0) {
        exec_image_release(image);
        return -EACCES;
    }
    const unsigned char *header = image->bytes.bytes;
    size_t got = image->bytes.size < 20 ? image->bytes.size : 20;
    int is_elf = got >= 4 && header[0] == 0x7f && header[1] == 'E' && header[2] == 'L' && header[3] == 'F';
    image->script = got >= 2 && header[0] == '#' && header[1] == '!';
    if (!is_elf && !image->script) {
        exec_image_release(image);
        return -ENOEXEC;
    }
    if (is_elf) {
        hl_linux_elf64_layout layout;
        if (hl_linux_elf64_validate(&image->bytes, HL_EXEC_ELF_MACHINE, &layout) != 0) {
            exec_image_release(image);
            return -ENOEXEC;
        }
    }
    snprintf(image->path, sizeof image->path, "%s", path);
    return 0;
}

static int exec_image_open(const char *path, exec_image *image) {
    if (path == NULL) return -ENOENT;
    int descriptor = open(path, O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (descriptor < 0) return errno == ELOOP ? -EACCES : -errno;
    return exec_image_adopt(descriptor, path, image);
}

static int exec_image_authorized(const char *path, exec_image *image) {
    const unsigned char *header = g_authorized_executable_image;
    if (path == NULL || image == NULL || header == NULL || g_authorized_executable_size < 2 ||
        !g_authorized_executable_metadata_ready)
        return -ENOENT;
    memset(image, 0, sizeof *image);
    image->descriptor = -1;
    image->status = g_authorized_executable_status;
    image->dac = g_authorized_executable_dac;
    uint32_t groups[HL_NGROUPS_MAX];
    hl_dac_credentials credentials = dac_credentials_current(groups);
    if (hl_dac_authorize_access(&image->dac, &credentials, HL_DAC_EXECUTE) != 0) return -EACCES;
    if (hl_linux_image_read_bytes(header, g_authorized_executable_size, &image->bytes) != 0) return -ENOMEM;
    int is_elf = image->bytes.size >= 4 && header[0] == 0x7f && header[1] == 'E' && header[2] == 'L' && header[3] == 'F';
    image->script = header[0] == '#' && header[1] == '!';
    if (!is_elf && !image->script) {
        exec_image_release(image);
        return -ENOEXEC;
    }
    if (is_elf) {
        hl_linux_elf64_layout layout;
        if (hl_linux_elf64_validate(&image->bytes, HL_EXEC_ELF_MACHINE, &layout) != 0) {
            exec_image_release(image);
            return -ENOEXEC;
        }
    }
    snprintf(image->path, sizeof image->path, "%s", path);
    return 0;
}

static void exec_authority_seed_initial(const hl_host_services *host, hl_host_handle executable,
                                        const hl_executable_authority *serialized) {
    const hl_host_posix_attachment_services *attachments = host != NULL ? host->posix_attachment : NULL;
    free(g_authorized_executable_owned);
    g_authorized_executable_owned = NULL;
    g_authorized_executable_path[0] = 0;
    g_authorized_executable_metadata_ready = 0;
    memset(&g_authorized_executable_status, 0, sizeof g_authorized_executable_status);
    memset(&g_authorized_executable_dac, 0, sizeof g_authorized_executable_dac);
    if (attachments == NULL || attachments->borrow_file == NULL || attachments->release == NULL ||
        executable == HL_HOST_HANDLE_INVALID) {
        if (serialized != NULL && serialized->ready) {
            g_authorized_executable_status.st_dev = (dev_t)serialized->stable_device;
            g_authorized_executable_status.st_ino = (ino_t)serialized->stable_object;
            g_authorized_executable_status.st_mode = (mode_t)serialized->mode;
            g_authorized_executable_dac.uid = serialized->user;
            g_authorized_executable_dac.gid = serialized->group;
            g_authorized_executable_dac.mode = serialized->mode;
            g_authorized_executable_metadata_ready = 1;
        }
        return;
    }
    hl_host_result borrowed = attachments->borrow_file(host->context, executable);
    if (borrowed.status != HL_STATUS_OK) return;
    int descriptor = (int)borrowed.value;
    if (fstat(descriptor, &g_authorized_executable_status) == 0) {
        stat_virt_ids(&g_authorized_executable_status, NULL, descriptor, &g_authorized_executable_dac.uid,
                      &g_authorized_executable_dac.gid);
        g_authorized_executable_dac.mode = (uint32_t)stat_virt_mode(&g_authorized_executable_status, NULL, descriptor);
        g_authorized_executable_metadata_ready = 1;
    }
    (void)attachments->release(host->context, borrowed.value);
}

static void exec_authority_rotate(exec_image *image, const char *guest_path) {
    if (image == NULL || image->bytes.bytes == NULL || guest_path == NULL) return;
    free(g_authorized_executable_owned);
    g_authorized_executable_owned = image->bytes.bytes;
    g_authorized_executable_image = image->bytes.bytes;
    g_authorized_executable_size = image->bytes.size;
    g_authorized_executable_status = image->status;
    g_authorized_executable_dac = image->dac;
    g_authorized_executable_metadata_ready = 1;
    snprintf(g_authorized_executable_path, sizeof g_authorized_executable_path, "%s", guest_path);
    image->bytes.bytes = NULL;
    image->bytes.size = 0;
}

static int exec_image_parse_shebang(const exec_image *image, char *interpreter, size_t interpreter_size, char *argument,
                                    size_t argument_size) {
    if (image == NULL || !image->script || image->bytes.size <= 3) return 0;
    char header[258];
    size_t count = image->bytes.size < sizeof header - 1 ? image->bytes.size : sizeof header - 1;
    memcpy(header, image->bytes.bytes, count);
    header[count] = 0;
    char *newline = strchr(header, '\n');
    if (newline != NULL) *newline = 0;
    char *start = header + 2;
    while (*start == ' ' || *start == '\t')
        start++;
    char *end = start;
    while (*end && *end != ' ' && *end != '\t')
        end++;
    char *optional = NULL;
    if (*end) {
        *end = 0;
        optional = end + 1;
        while (*optional == ' ' || *optional == '\t')
            optional++;
        if (!*optional) optional = NULL;
    }
    snprintf(interpreter, interpreter_size, "%s", start);
    if (optional != NULL)
        snprintf(argument, argument_size, "%s", optional);
    else
        argument[0] = 0;
    return interpreter[0] != 0;
}

static int exec_resolve_shebang_images(char **argv, int argc, int capacity, exec_image *image, char store[][256]) {
    int stored = 0;
    for (int level = 0;; level++) {
        char interpreter[256], optional[256];
        if (exec_image_parse_shebang(image, interpreter, sizeof interpreter, optional, sizeof optional) != 1)
            return argc;
        if (level >= SHEBANG_MAX) return -ELOOP;
        int inserted = optional[0] ? 2 : 1;
        if (argc + inserted >= capacity) return -E2BIG;
        char *stored_interpreter = store[stored++];
        snprintf(stored_interpreter, 256, "%s", interpreter);
        char *stored_optional = NULL;
        if (optional[0]) {
            stored_optional = store[stored++];
            snprintf(stored_optional, 256, "%s", optional);
        }
        for (int index = argc; index >= 0; index--)
            argv[index + inserted] = argv[index];
        argv[0] = stored_interpreter;
        if (stored_optional != NULL) argv[1] = stored_optional;
        argc += inserted;
        char backing[4200];
        const char *resolved = xresolve_overlay(stored_interpreter, backing, sizeof backing);
        exec_image next;
        int error = exec_image_open(resolved, &next);
        if (error != 0) return error;
#if defined(HL_NATIVE_TEST_HOOKS)
        exec_pin_test_wait(HL_EXEC_PIN_TEST_SHEBANG_HOP);
#endif
        exec_image_release(image);
        *image = next;
    }
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

static _Thread_local int g_exec_requested_descriptor = -1;

static int svc_proc_221(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 221: {
        int requested_descriptor = g_exec_requested_descriptor;
        g_exec_requested_descriptor = -1;
        memf_materialize_all(); // non-CLOEXEC scratch fds survive exec -> flush RAM into the real files
        // Linux comm = last component of the path PASSED to execve, captured BEFORE the /proc magic-link
        // rewrite below and before binfmt_script (execve("/proc/self/exe") -> comm "exe"; "./run.sh"
        // keeps "run.sh"). Applied at the committed point further down, only once the exec cannot fail.
        char comm_src[256];
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
        int self_executable = 0;
        int path_error = requested_descriptor >= 0 ? 0 : exec_resolve_proc_path(&a0, &self_executable);
        if (path_error) {
            if (requested_descriptor >= 0) close(requested_descriptor);
            G_RET(c) = (uint64_t)(int64_t)path_error;
            break;
        }
        char pb[4200];
        const char *p =
            requested_descriptor >= 0
                ? (const char *)(uintptr_t)a0
                :
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
        exec_image main_image;
        int image_error = requested_descriptor >= 0 ? exec_image_adopt(requested_descriptor, p, &main_image)
                            : self_executable ? exec_image_authorized(p, &main_image) : exec_image_open(p, &main_image);
        if (image_error) {
            G_RET(c) = (uint64_t)(int64_t)image_error;
            break;
        }
#if defined(HL_NATIVE_TEST_HOOKS)
        exec_pin_test_wait(HL_EXEC_PIN_TEST_MAIN);
#endif
        char *argv[HL_MAXARGV]; // Linux allows far more than 255 args within ARG_MAX -- a fixed 256 silently
        int ac = 0;             // dropped the tail (a different command ran, and /proc/self/cmdline diverged)
        int argument_error = exec_collect_argv(a1, argv, &ac);
        if (argument_error) {
            exec_image_release(&main_image);
            G_RET(c) = (uint64_t)(int64_t)argument_error;
            break;
        }
        // Forward the guest's ACTUAL environment across the exec: build_stack rebuilds the new process env
        // from HL_GUEST_ENV, so serialize envp (a2) into it NOW while guest memory is still mapped. A guest
        // that set/modified env vars (FOO=bar, a tweaked PATH) thus sees them survive; a NULL envp keeps the
        // container's HL_GUEST_ENV defaults (a2 is NOT rebased by the dispatch redirect, unlike a0/a1).
        char *staged_environment = exec_stage_env(a2);
        if (staged_environment == NULL) {
            exec_image_release(&main_image);
            G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
            break;
        }
        hl_exec_environment_update environment_update;
        if (hl_exec_environment_prepare(&environment_update, staged_environment) != 0) {
            free(staged_environment);
            exec_image_release(&main_image);
            G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
            break;
        }
        free(staged_environment);
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
        char sh_store[SHEBANG_MAX * 2][256];
        char *na[HL_MAXARGV];
        int nn = 0;
        // Linux passes the execve path (a0) as the script-path arg; the original argv[0] is discarded.
        na[nn++] = (char *)a0;
        for (int i = 1; i < ac && nn < HL_MAXARGV - 1; i++)
            na[nn++] = argv[i];
        na[nn] = NULL;
        int original_nn = nn;
        int sh_new = exec_resolve_shebang_images(na, nn, HL_MAXARGV, &main_image, sh_store);
        if (sh_new < 0) {
            // too many nested #! -> ELOOP. `-ELOOP` is the host macOS errno 62; svc_done's boundary translation
            // maps it to Linux ELOOP (40) at the syscall boundary, exactly like the vfs symlink-loop path.
            exec_image_release(&main_image);
            hl_exec_environment_discard(&environment_update);
            G_RET(c) = (uint64_t)(int64_t)sh_new;
            break;
        }
        if (main_image.script && sh_new == original_nn) {
            // A file beginning with #! is accepted only when binfmt_script resolved a non-empty
            // interpreter.  Falling through to the ELF loader after an empty/malformed shebang
            // commits the exec, tears down the old image, and interprets text bytes as an ELF.
            exec_image_release(&main_image);
            hl_exec_environment_discard(&environment_update);
            G_RET(c) = (uint64_t)(int64_t)-ENOEXEC;
            break;
        }
        if (sh_new != original_nn) { // a shebang chain resolved -> load the final interpreter, not the script
            snprintf(gexe, sizeof gexe, "%s", na[0]); // /proc/self/exe names the interpreter
            for (int i = 0; i <= sh_new; i++)
                argv[i] = na[i];
            ac = sh_new;
        }
        p = main_image.path;
        exec_image program_interpreter;
        memset(&program_interpreter, 0, sizeof program_interpreter);
        program_interpreter.descriptor = -1;
        char interpreter_path[256], interpreter_backing[4200];
        int has_program_interpreter = elf_interp(p, interpreter_path, sizeof interpreter_path, &main_image.bytes) == 0;
        if (has_program_interpreter) {
            const char *resolved = xresolve_overlay(interpreter_path, interpreter_backing, sizeof interpreter_backing);
            int interpreter_error = exec_image_open(resolved, &program_interpreter);
            if (interpreter_error != 0) {
                exec_image_release(&main_image);
                hl_exec_environment_discard(&environment_update);
                G_RET(c) = (uint64_t)(int64_t)interpreter_error;
                break;
            }
        }
        if (exec_image_is_write_open(&main_image.status) ||
            (has_program_interpreter && exec_image_is_write_open(&program_interpreter.status))) {
            exec_image_release(&program_interpreter);
            exec_image_release(&main_image);
            hl_exec_environment_discard(&environment_update);
            G_RET(c) = (uint64_t)(int64_t)-ETXTBSY;
            break;
        }
#if defined(HL_NATIVE_TEST_HOOKS)
        exec_pin_test_wait(HL_EXEC_PIN_TEST_FINAL);
#endif
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
            exec_image_release(&program_interpreter);
            exec_image_release(&main_image);
            hl_exec_environment_discard(&environment_update);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        thread_exit_others(c);
        exec_publish_env(&environment_update);
        // All failure returns are behind us: Linux releases a vfork parent
        // when exec commits, before the new image begins executing.
        vfork_release_parent();
        set_guest_comm(comm_src);                  // comm := basename of the exec'd NAME (captured pre-rewrite above)
        cred_after_exec_snapshot(&main_image.dac); // apply set-id ownership from the pinned executable
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
        hl_identity_digest interp_identity = {0};
        char pc_ihost[4200];
        const char *pc_interp_host = NULL;
        (void)pc_ihost;
        (void)pc_interp_host;
#ifdef PCACHE_EXEC_HOOKS
        pcache_exec_force_main(); // map the new image at the fixed VA so its cached arena is reusable
#endif
        load_elf(p, &lm, NULL, &main_image.bytes);
        uint64_t jump = lm.entry, at_base = 0;
        if (has_program_interpreter) {
            const char *ih = program_interpreter.path;
#ifdef PCACHE_EXEC_HOOKS
            snprintf(pc_ihost, sizeof pc_ihost, "%s", ih); // outlive `ib` for the cache id below
            pc_interp_host = pc_ihost;
            pcache_exec_force_interp();
#endif
            struct loaded li;
            load_elf(ih, &li, NULL, &program_interpreter.bytes);
            interp_identity = li.identity;
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
        pcache_exec_reload(lm.identity, interp_identity, argv[0], jump);
#endif
        exec_authority_rotate(&main_image, gexe);
        exec_image_release(&program_interpreter);
        exec_image_release(&main_image);
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
        // Linux disables the calling thread's alternate signal stack on a
        // successful exec. The guest exec is an in-process image reload, so
        // reset the emulated state explicitly rather than inheriting storage
        // that belonged to the unmapped predecessor image.
        c->alt_sp = 0;
        c->alt_size = 0;
        c->alt_flags = 2; // SS_DISABLE
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

static int svc_proc_exec(struct cpu *cpu, const char *logical_path, uint64_t argv, uint64_t environment,
                         int owned_descriptor) {
    if (owned_descriptor < 0)
        return svc_proc_221(cpu, 221, (uint64_t)(uintptr_t)logical_path, argv, environment, 0, 0, 0);
    if (g_exec_requested_descriptor >= 0) {
        close(owned_descriptor);
        return 0;
    }
    g_exec_requested_descriptor = owned_descriptor;
    int handled = svc_proc_221(cpu, 221, (uint64_t)(uintptr_t)logical_path, argv, environment, 0, 0, 0);
    if (g_exec_requested_descriptor >= 0) close(g_exec_requested_descriptor);
    g_exec_requested_descriptor = -1;
    return handled;
}
