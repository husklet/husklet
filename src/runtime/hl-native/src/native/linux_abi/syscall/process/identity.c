// Cohesive process-syscall handlers. Included by ../proc.c after shared process state.
static int svc_proc_92(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 92: {
        unsigned prev = g_persona;
        if ((unsigned)a0 != 0xffffffffu) g_persona = (unsigned)a0;
        G_RET(c) = (uint64_t)prev;
        break;
    }
    // ===================== Process & scheduling — clone/exec/wait/ids/prctl/futex/caps/sched =====================
    // capget(hdrp, datap): the container runs as root, so report every capability present -- but ALSO
    // honour the kernel's ABI-version negotiation, which libcap-ng/libcap (and thus setpriv) probe
    // for. hdrp->version selects the layout; an UNSUPPORTED value makes the real kernel rewrite it to its
    // preferred version (v3) and fail EINVAL. The old stub ignored the header entirely and always returned
    // 0, so libcap-ng negotiated a bogus (0) version and capng_apply() then failed WITHOUT setting errno
    // -> setpriv aborts "activate capabilities: Success" before it ever reaches capset. Model it properly.
    default: return 0;
    }
    return 1;
}

static int svc_proc_90(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 90: {
        uint32_t header[2];
        // Import through the logical-VMA resolver: the header may live in canonical storage rather than at
        // its Linux-visible address.
        if (!a0 || guest_copy_from(header, a0, sizeof header) != sizeof header) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ver = header[0];
        int u32s = 0; // number of __user_cap_data_struct the version spans
        switch (ver) {
        case 0x19980330: u32s = 1; break; // _LINUX_CAPABILITY_VERSION_1 (1 u32 mask)
        case 0x20071026:                  // _LINUX_CAPABILITY_VERSION_2 (deprecated)
        case 0x20080522: u32s = 2; break; // _LINUX_CAPABILITY_VERSION_3 (2 u32 masks, 64 caps)
        default:
            // kernel cap_validate_magic: rewrite header->version to its preferred (v3). A pure version
            // probe (data==NULL) then succeeds; otherwise it is EINVAL (LTP capget02 "bad version" +
            // the libcap-ng negotiation probe). The rewrite is what the test asserts on afterwards.
            header[0] = 0x20080522;
            if (guest_copy_to(a0, header, sizeof(uint32_t)) != sizeof(uint32_t)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = a1 ? (uint64_t)(-EINVAL) : 0;
            goto cap_done;
        }
        // header->pid selects the target task: <0 -> EINVAL, a dead pid -> ESRCH (LTP capget02
        // "bad pid"/"unused pid"). 0/self/our own tid/pid resolve to this process (capget01 uses getpid()).
        int tpid = (int)header[1];
        if (tpid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (tpid != 0 && tpid != container_pid() && tpid != (int)getpid() && !thread_tid_alive(tpid) &&
            kill((pid_t)tpid, 0) < 0 && errno == ESRCH) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        // datap (a1) is {effective, permitted, inheritable}[u32s]; NULL on a pure version probe. A bad
        // non-NULL datap -> EFAULT (kernel copy_to_user; capget02 "bad address data"). Report the guest's
        // ACTUAL effective set -- g_cap_eff, narrowed by any capset() drop (e.g. a dropped CAP_NET_RAW,
        // LTP capget01 / task D) -- rather than a blanket all-ones that over-reports capabilities.
        if (a1) {
            uint32_t d[6] = {0};
            for (int i = 0; i < u32s; i++) {
                uint32_t eff = (i == 0) ? (uint32_t)g_cap_eff : (uint32_t)(g_cap_eff >> 32);
                // permitted = the docker default 14-cap set (HL_CAP_DEFAULT), NOT a blanket all-ones: a
                // default `docker run` root container has CapPrm=00000000a80425fb, matching /proc/self/status
                // exactly. The old 0xffffffff over-reported caps (e.g. CAP_SYS_ADMIN) the container lacks.
                uint32_t prm = (i == 0) ? (uint32_t)g_cap_prm : (uint32_t)(g_cap_prm >> 32);
                d[i * 3 + 0] = eff; // effective: the guest's live effective set (respects drops)
                d[i * 3 + 1] = prm; // permitted: the docker default bounding/permitted set
                d[i * 3 + 2] = (i == 0) ? (uint32_t)g_cap_inh : (uint32_t)(g_cap_inh >> 32);
            }
            size_t bytes = (size_t)u32s * 12;
            if (guest_copy_to(a1, d, bytes) != (ssize_t)bytes) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = 0;
    cap_done:
        break;
    }
    // capset(hdrp, datap): reject an unsupported ABI version the same way the kernel does (EINVAL, header
    // rewritten to v3), so a libcap-ng probe sees a consistent kernel; otherwise honour the request (the
    // container is root -- we don't model per-cap enforcement, so any well-formed set "succeeds").
    default: return 0;
    }
    return 1;
}

static int svc_proc_91(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 91: {
        uint32_t header[2];
        if (!a0 || guest_copy_from(header, a0, sizeof header) != sizeof header) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ver = header[0];
        if (ver != 0x19980330 && ver != 0x20071026 && ver != 0x20080522) {
            uint32_t preferred = 0x20080522;
            if (guest_copy_to(a0, &preferred, sizeof preferred) != sizeof preferred)
                G_RET(c) = (uint64_t)(-EFAULT);
            else
                G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // Track the effective set the guest just asked for, so a capability-gated prctl (PR_SET_SECUREBITS /
        // PR_CAPBSET_DROP) reflects a dropped CAP_SETPCAP. datap is {effective,permitted,inheritable}[u32s];
        // effective words are at data[i*3+0]. v1 spans the low 32 caps, v3 the full 64.
        int u32s = (ver == 0x19980330u) ? 1 : 2;
        uint32_t d[6];
        size_t bytes = (size_t)u32s * 12;
        if (!a1 || guest_copy_from(d, a1, bytes) != (ssize_t)bytes) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint64_t eff = d[0];
        if (u32s == 2) eff |= (uint64_t)d[3] << 32;
        uint64_t prm = d[1];
        if (u32s == 2) prm |= (uint64_t)d[4] << 32;
        uint64_t inh = d[2];
        if (u32s == 2) inh |= (uint64_t)d[5] << 32;
        cred_init();
        uint64_t inheritable_allowed = g_cap_inh | prm;
        if (g_cap_eff & (1ull << CAP_SETPCAP)) inheritable_allowed |= g_cap_bnd;
        if ((eff & ~prm) != 0 || (prm & ~g_cap_prm) != 0 || (inh & ~inheritable_allowed) != 0) {
            G_RET(c) = (uint64_t)(int64_t)-EPERM;
            break;
        }
        g_cap_eff = eff;
        g_cap_prm = prm;
        g_cap_inh = inh;
        g_cap_amb &= g_cap_prm & g_cap_inh;
        g_cap_setid_perm = (prm & ((1ull << 6) | (1ull << 7))) != 0;
        g_cap_setid_eff = (eff & ((1ull << 6) | (1ull << 7))) != 0;
        G_RET(c) = 0;
        break;
    }
    // chroot(path): re-root the guest WITHIN the rootfs jail. Resolve the target through the active jail to
    // its host backing -- this validates it exists as a directory inside the rootfs and can NEVER name a
    // host path -- then record it as the new chroot prefix. Subsequent absolute guest paths are walked
    // under this prefix yet stay confined to g_root_fd, so the guest cannot escape to the real host fs.
    default: return 0;
    }
    return 1;
}

static int svc_proc_51(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 51: {
        char guest_path[4200], gabs[4200];
        int imported = guest_copy_string(guest_path, sizeof guest_path, a0);
        if (imported < 0) {
            G_RET(c) = (uint64_t)(int64_t)imported;
            break;
        }
        abs_guest(-100, guest_path, gabs, sizeof gabs); // (AT_FDCWD, path) -> guest-view abs
        if (g_rootfs && synth_proc_fd_dir_is(gabs)) {
            char isolated[128], backing[4200];
            snprintf(isolated, sizeof isolated, "/.hl-proc-chroot-%d", (int)getpid());
            confine_in(g_rootfs_canon, g_rootfs_canon_len, isolated, backing, sizeof backing, 1);
            if (hl_compat_mkdir(backing, 0700) != 0 && errno != EEXIST) {
                G_RET(c) = (uint64_t)(int64_t)(-errno);
                break;
            }
            snprintf(g_chroot, sizeof g_chroot, "%s", isolated);
            snprintf(g_cwd, sizeof g_cwd, "/");
            hl_fdcache_reset();
            G_RET(c) = 0;
            break;
        }
        char hp[4200];
        const char *h = xresolve_overlay(gabs, hp, sizeof hp); // host backing (honors any chroot already set)
        struct stat st;
        if (stat(h, &st) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        if (!S_ISDIR(st.st_mode)) {
            G_RET(c) = (uint64_t)(-ENOTDIR);
            break;
        }
        char nc[4200];
        chroot_apply(gabs, nc, sizeof nc);                          // fold under any active chroot -> rootfs-abs
        snprintf(g_chroot, sizeof g_chroot, "%s", nc[1] ? nc : ""); // chroot("/") clears (rootfs IS the root)
        hl_fdcache_reset(); // drop cached guest->host path mappings -- they predate the re-root
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return 1;
}

/* Defined by the checkpoint domain (linux_abi/sink_stream.h), which this translation unit includes after
   the syscall layer. Silent for every process that is not a restored member of a checkpoint image. */
static void ckpt_restored_member_exit_code(int status);

static void process_last_thread_exit(int status) {
    launch_reg_terminate_peers();
    socket_ref_process_exit();
    acct_proc_leave();
    proc_reg_unlink();
    proc_fdvis_cleanup();
    hl_host_process_fd_private_cleanup();
    poslk_on_exit();
    sysv_on_exit();
    hl_engine_child_result_publish(status, HL_STATUS_OK, 0);
    /* A restored member is not a child of the host that holds it, so its status cannot be reaped -- it has
       to be reported. This is the process-scoped exit hook both guest exit paths share, which is why the
       report belongs here: exit_group(2) reaches the host _exit straight from the syscall and never returns
       through the restore driver that used to own the only call. */
    ckpt_restored_member_exit_code(status);
    (void)hl_backend_tree_finalize(0);
}

static int svc_proc_93(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 93:
        vfork_publish_exit();
        c->exited = 1;
        // Linux exposes only the low eight bits of an exit status to waiters.
        // Preserve that contract on the translated `exit` path too: unlike the
        // `exit_group` path below, this unwinds through the engine instead of
        // reaching the host `_exit`, so the kernel cannot normalize it for us.
        c->exit_code = (int)(a0 & 0xffu);
        // exit: end THIS thread. The thread trampoline owns peer-thread cleanup;
        // the process owner reaches this path directly, so when it is the last
        // live thread it must also retire process-scoped shared registries before
        // returning through the embedding host. Otherwise fork children leave
        // their descriptor-visibility rows behind until the fixed arena fills.
        futex_robust_exit(c);
        if (thread_live_count() == 1) process_last_thread_exit(c->exit_code);
        break;
    // exit_group: end the whole process
    default: return 0;
    }
    return 1;
}

// Profile records are a machine-readable interface.  Render the variable-width transliterator record at its exact
// size and issue it as one checked write: appending snprintf's would-have-written length to a fixed dispatcher buffer
// silently truncated the tail and its newline, so the following diagnostic write became part of the same line.
static int profile_record_write(const char *record, size_t length) {
    return record != NULL && length != 0 && record[length - 1u] == '\n' &&
           hl_linux_write(g_linux_box, STDERR_FILENO, record, length) == (int64_t)length;
}

static int translit_profile_write(void) {
    int required = translit_report(NULL, 0);
    if (required <= 0) return 0;
    size_t length = (size_t)required;
    char *record = malloc(length + 1u);
    if (record == NULL) return 0;
    int rendered = translit_report(record, length + 1u);
    int complete = rendered == required && record[length] == '\0' && record[length - 1u] == '\n';
    int written = complete && profile_record_write(record, length);
    free(record);
    return written;
}

static int svc_proc_94(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 94:
        translit_sampling_receipt("svc-exit-group");
        vfork_publish_exit();
        translit_perf_map_flush_at_exit();
        translit_sampling_receipt("svc-exit-group-flushed");
        HL_LOGF(&g_jit_log, HL_LOG_TAG_NETWORK, "exit_group pid=%d code=%d", (int)getpid(), (int)a0);
        hl_dispatch_profile_report(&g_dispatch_profile, &g_jit_log, translation_log_summary);
        if (g_prof && g_profile_output_owner) {
            char profile[1024];
            int profile_size = snprintf(
                profile, sizeof profile,
                "[prof] crossings=%llu syscalls=%llu ibtc_miss=%llu branch_cross=%llu translations=%llu lse=%llu "
                "wx_toggles=%llu dualmap=%d xlate_ms=%.3f service_ms=%.3f mtibtc=%d mtfill=%llu "
                "fwake_fast=%llu fwake_slow=%llu fwait=%llu soft_sample_shift=6 soft_sampled_sites=%llu "
                "soft_hull_direct_sampled=%llu soft_cached_hit_sampled=%llu soft_miss=%llu soft_span=%llu "
                "soft_bounce_prepare=%llu soft_bounce_commit=%llu smc_queued=%llu smc_commit=%llu "
                "soft_guard_bytes=%llu shared_sites=%llu inline_sites=%llu authorized_hits=%llu fork_authorized_hits=%llu byte_unstable=%llu\n",
                (unsigned long long)g_prof_cross, (unsigned long long)g_prof_sys, (unsigned long long)g_prof_miss,
                (unsigned long long)(g_prof_cross - g_prof_sys - g_prof_miss), (unsigned long long)g_prof_xlate,
                (unsigned long long)g_lse_n, (unsigned long long)g_wx_toggles, g_dualmap, g_xlate_ns / 1e6,
                g_service_ns / 1e6, g_mtibtc, (unsigned long long)g_mtfill, (unsigned long long)g_futex_wake_fast,
                (unsigned long long)g_futex_wake_slow, (unsigned long long)g_futex_wait_n,
                (unsigned long long)g_prof_soft_sites_sampled, (unsigned long long)g_prof_soft_hull_sampled,
                (unsigned long long)g_prof_soft_cached_sampled, (unsigned long long)g_prof_soft_miss,
                (unsigned long long)g_prof_soft_span, (unsigned long long)g_prof_soft_bounce_prepare,
                (unsigned long long)g_prof_soft_bounce_commit, (unsigned long long)g_prof_smc_queued,
                (unsigned long long)g_prof_smc_commit, (unsigned long long)g_prof_soft_guard_bytes,
                (unsigned long long)g_prof_soft_shared_sites, (unsigned long long)g_prof_soft_inline_sites,
                (unsigned long long)hl_x86_decode_authorized_hits(),
                (unsigned long long)hl_x86_decode_authorized_hits_after_fork(),
                (unsigned long long)atomic_load_explicit(&g_exec_bytes_unstable, memory_order_acquire));
            if (profile_size > 0 && (size_t)profile_size < sizeof profile)
                (void)profile_record_write(profile, (size_t)profile_size);
#if defined(HL_NATIVE_TEST_HOOKS) && defined(HL_TRANSLATOR_GUEST_X86_64_CPU_H)
            char redispatch[512];
            int redispatch_size = snprintf(
                redispatch, sizeof redispatch,
                "[prof] redispatch attempted=%llu hit=%llu stale-hit=%llu threaded-hit=%llu map-miss=%llu stale=%llu threaded=%llu irq=%llu "
                "signal=%llu fatal=%llu exited=%llu budget=%llu\n",
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_ATTEMPTED], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_HIT], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_STALE_HIT], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_THREADED_HIT], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_MAP_MISS], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_STALE], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_THREADED], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_IRQ], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_SIGNAL], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_FATAL], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_EXITED], memory_order_relaxed),
                (unsigned long long)atomic_load_explicit(&g_dispatch_redispatch[REDISPATCH_BUDGET], memory_order_relaxed));
            if (redispatch_size > 0 && (size_t)redispatch_size < sizeof redispatch)
                (void)profile_record_write(redispatch, (size_t)redispatch_size);
#endif
            // The transliterator record is variable-width and owns its newline; keep it out of the fixed dispatcher
            // record so neither can silently consume the other's capacity.
            (void)translit_profile_write();
#if defined(HL_NATIVE_TEST_HOOKS) && defined(HL_HOST_CPU_X86_64)
            char unsupported[1024];
            int unsupported_size = translit_unsupported_report(unsupported, sizeof unsupported);
            if (unsupported_size > 0 && (size_t)unsupported_size < sizeof unsupported)
                (void)hl_linux_write(g_linux_box, STDERR_FILENO, unsupported, (size_t)unsupported_size);
#endif
            char boundary[1024];
            int boundary_size = snprintf(
                boundary, sizeof boundary,
                "[diag] boundary sample_shift=6 sample_offsets=1,65,... samples=%llu poll_ns=%llu resolve_ns=%llu stw_ns=%llu "
                "block_ns=%llu reason_ns=%llu map_hit=%llu map_miss=%llu threaded=%llu stw_retry=%llu "
                "pending_irq=%llu reason_total=%llu r0=%llu r1=%llu r2=%llu r3=%llu r4=%llu r5=%llu "
                "r6=%llu r7=%llu r8=%llu r9=%llu r10=%llu r11=%llu r12=%llu r13=%llu r14=%llu r15=%llu "
                "r_stw_retry=%llu r_other=%llu\n",
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.sampled),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.poll_ns),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.resolve_ns),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.stw_ns),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.block_ns),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason_ns),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.map_hit),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.map_miss),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.threaded_crossings),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.stw_retries),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.pending_interrupts),
                (unsigned long long)hl_dispatch_profile_reason_total(&g_dispatch_profile),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[0]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[1]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[2]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[3]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[4]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[5]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[6]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[7]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[8]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[9]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[10]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[11]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[12]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[13]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[14]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[15]),
                (unsigned long long)hl_dispatch_profile_load(
                    &g_dispatch_profile.reason[HL_DISPATCH_REASON_STW_RETRY]),
                (unsigned long long)hl_dispatch_profile_load(&g_dispatch_profile.reason[HL_DISPATCH_REASON_OTHER]));
            if (boundary_size > 0) {
                size_t bounded = (size_t)boundary_size < sizeof boundary ? (size_t)boundary_size : sizeof boundary - 1;
                (void)hl_linux_write(g_linux_box, STDERR_FILENO, boundary, bounded);
            }
        }
#ifdef PCACHE_SAVE_HOOK
        PCACHE_SAVE_HOOK; // persist the translated arena before one-shot exit when HL_PCACHE is active
#endif
        futex_robust_exit(c); // robust mutexes still held by the calling thread -> OWNER_DIED + wake waiters
        process_last_thread_exit((int32_t)a0);
        _exit((int)a0);
    default: return 0;
    }
    return 1;
}

static int svc_proc_96(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 96:
        // set_tid_address(tidptr): store tidptr as this thread's clear_child_tid so thread exit zeroes it and
        // FUTEX_WAKEs a joiner (futex_wake_addr on c->ctid). Returns the caller's TID (gettid, not the tgid).
        c->ctid = a0;
        G_RET(c) = (uint64_t)cpu_tid(c);
        break;
    default: return 0;
    }
    return 1;
}

static int svc_proc_97(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                       uint64_t a5) {
    switch (nr) {
    case 97: {
        // unshare(flags): this engine does not create a distinct namespace or process-sharing domain.
        // Preserve Linux's flag validation, but report a recognized nonzero request as unavailable instead
        // of claiming isolation that was never established.  Callers can then fall back safely.
        unsigned uf = (unsigned)a0;
        const unsigned UNSHARE_VALID = 0x80u /*NEWTIME*/ | 0x200u /*FS*/ | 0x400u /*FILES*/ | 0x20000u /*NEWNS*/ |
                                       0x40000u /*SYSVSEM*/ | 0x2000000u /*NEWCGROUP*/ | 0x4000000u /*NEWUTS*/ |
                                       0x8000000u /*NEWIPC*/ | 0x10000000u /*NEWUSER*/ | 0x20000000u /*NEWPID*/ |
                                       0x40000000u /*NEWNET*/;
        if (uf & ~UNSHARE_VALID)
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
        else
            G_RET(c) = uf ? (uint64_t)(int64_t)(-ENOSYS) : 0;
        break;
    }
    // setns(fd, nstype): no real namespaces, but a negative/invalid fd must fail EBADF (Linux copies the ns fd
    // first). Fake success on setns(-1, ...) would let isolation setup proceed on a false premise.
    default: return 0;
    }
    return 1;
}

static int svc_proc_268(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 268: {
        int fd = (int)a0;
        if (fd < 0 || fd >= HL_NFD || strncmp(g_proc_text_desc[fd], "namespace:", 10)) {
            G_RET(c) = (uint64_t)(int64_t)(fd < 0 || fcntl(fd, F_GETFD) < 0 ? -EBADF : -EINVAL);
            break;
        }
        unsigned actual = ns_clone_flag(g_proc_text_desc[fd] + 10);
        unsigned requested = (unsigned)a1;
        if (!actual || (requested && requested != actual))
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
        else
            // The container's namespace set is immutable and the default capability set lacks
            // CAP_SYS_ADMIN, so a valid matching namespace fd reaches Linux's permission check.
            G_RET(c) = (uint64_t)(int64_t)-EPERM;
        break;
    }
    // futex
    default: return 0;
    }
    return 1;
}

static int svc_proc_172(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 172: G_RET(c) = (uint64_t)container_pid(); break;
    default: return 0;
    }
    return 1;
}

static int svc_proc_173(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 173:
        // getppid (init's parent is 0 in the ns). A restored process reports its recorded guest parent
        // (g_self_gppid), since its live host parent differs after the tree was re-forked.
        // A PID-namespace init has no parent inside its namespace. proc_self_guest_ppid is the single
        // owner of this answer, shared with the /proc/self/{stat,status} rendering (state.c).
        G_RET(c) = (uint64_t)proc_self_guest_ppid(container_pid());
        break;
    // getuid/geteuid -> container uid (0=root by default), reflecting any runtime drop (apt -> _apt).
    default: return 0;
    }
    return 1;
}

static int svc_proc_174(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 174:
        cred_init();
        G_RET(c) = (uint64_t)g_ruid;
        break;
    default: return 0;
    }
    return 1;
}

static int svc_proc_175(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 175: G_RET(c) = (uint64_t)cred_euid(); break;
    // getgid/getegid
    default: return 0;
    }
    return 1;
}

static int svc_proc_176(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 176:
        cred_init();
        G_RET(c) = (uint64_t)g_rgid;
        break;
    default: return 0;
    }
    return 1;
}

static int svc_proc_177(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 177: G_RET(c) = (uint64_t)cred_egid(); break;
    // gettid -- a UNIQUE per-thread id (unlike getpid, which is the shared tgid). The init thread keeps
    // c->tid==0 and reports the container pid (==1, where tid==tgid as on Linux); each spawned thread
    // carries its own id (spawn_thread). A correct gettid is load-bearing for runtimes that key thread
    // state on it (e.g. Go stores it in m.procid and tgkill()s it to preempt) -- collapsing every thread
    // to tid 1 makes their cross-thread signalling target the wrong thread and live-lock.
    default: return 0;
    }
    return 1;
}

static int svc_proc_178(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                        uint64_t a5) {
    switch (nr) {
    case 178: G_RET(c) = (uint64_t)(c->tid ? c->tid : container_pid()); break;
    // clone(flags,stack,ptid,tls,ctid)
    default: return 0;
    }
    return 1;
}
