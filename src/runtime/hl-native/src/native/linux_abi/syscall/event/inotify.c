/* Included by event.c: unity-build access with bounded syscall handlers. */

static int svc_inotify_init1(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                             uint64_t a4, uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
    case 26: {
        // inotify_init1(flags) -> kqueue. Only IN_NONBLOCK(0x800) and IN_CLOEXEC(0x80000) are defined;
        // Linux rejects any other flag bit with EINVAL, so a bad-flag probe must not read as supported.
        if ((int)a0 & ~(0x800 | 0x80000)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int r;
#if defined(__linux__)
        r = inotify_init1((int)a0);
#else
        r = kqueue();
        if (r >= 0) {
            if (r < HL_NFD) {
                g_inotify[r] = 1;
                g_epoll_family_seen = 1;
                g_inotify_nb[r] = (a0 & 0x800) ? 1 : 0; // remember IN_NONBLOCK for the fork-child kqueue rebuild
            }
            if (a0 & 0x800) fcntl(r, F_SETFL, O_NONBLOCK);
            // macOS kqueue() defaults FD_CLOEXEC SET; Linux inotify_init1(0) leaves it CLEAR. Set it exactly
            // per IN_CLOEXEC (clearing the kqueue default otherwise) so an inotify fd created without the
            // flag survives exec instead of being swept by hl's close-on-exec pass.
            fcntl(r, F_SETFD, (a0 & 0x80000) ? FD_CLOEXEC : 0);
        }
#endif
        if (r >= 0 && r < HL_NFD) {
            g_inotify[r] = 1;
            g_epoll_family_seen = 1;
            g_inotify_nb[r] = (a0 & 0x800) ? 1 : 0;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // inotify_add_watch(fd, path, mask) -- kqueue EVFILT_VNODE
    default: return 0;
    }
    return svc_done_host(c);
}

static int svc_inotify_add_watch(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                 uint64_t a4, uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
    case 27: {
        char pb[4200];
        char imported_path[4200];
        // EFAULT on an inaccessible path pointer BEFORE atpath dereferences it -- inotify_add_watch(fd, NULL,
        // mask) and a wild/unmapped path both return -EFAULT on Linux; without this guard atpath reads the
        // unmapped guest address and the engine SIGSEGVs (guest-triggerable crash). guest_bad_ptr also catches
        // a PROT_NONE guard page that host_range_mapped alone would miss. Mirrors the sibling *at path syscalls
        // in fs.c (openat/newfstatat/unlinkat all guard `!a1 || guest_bad_ptr(a1, 1)` first).
        if (guest_copy_string(imported_path, sizeof imported_path, a1) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // confined (realpath gate)
        const char *p = atpath(-100, imported_path, pb, sizeof pb, 0);
#if defined(__linux__)
        int wd = inotify_add_watch((int)a0, p, (uint32_t)a2);
        if (wd >= 0 && wd < HL_NFD) {
            struct stat dst;
            snprintf(g_inotify_wpath[wd], sizeof g_inotify_wpath[wd], "%s", p);
            g_inotify_mask[wd] = (uint32_t)a2;
            g_inotify_isdir[wd] = stat(p, &dst) == 0 && S_ISDIR(dst.st_mode);
            if (g_inotify_isdir[wd]) {
                free(g_inotify_snap[wd]);
                g_inotify_snap[wd] = dir_snapshot(p);
            }
            g_inotify_owner[wd] = (int)a0;
        }
        G_RET(c) = wd < 0 ? (uint64_t)(-errno) : (uint64_t)wd;
#else
        int wfd = hl_native_open_watch(p);
        if (wfd < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        struct kevent kv;
        EV_SET(&kv, wfd, EVFILT_VNODE, EV_ADD | EV_CLEAR,
               NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND, 0, (void *)(intptr_t)wfd);
        if (kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0) {
            int e = errno;
            close(wfd);
            G_RET(c) = (uint64_t)(-(int64_t)e);
            break;
        }
        // Remember every watched path/mask; directories also retain a name snapshot for create/delete diffs.
        struct stat dst;
        if (wfd >= 0 && wfd < HL_NFD) {
            snprintf(g_inotify_wpath[wfd], sizeof g_inotify_wpath[wfd], "%s", p);
            g_inotify_mask[wfd] = (uint32_t)a2;
            g_inotify_isdir[wfd] = stat(p, &dst) == 0 && S_ISDIR(dst.st_mode);
            if (g_inotify_isdir[wfd]) {
                free(g_inotify_snap[wfd]);
                g_inotify_snap[wfd] = dir_snapshot(p);
            }
            g_inotify_owner[wfd] = (int)a0; // the inotify instance this watch belongs to (for the move queue)
        }
        G_RET(c) = (uint64_t)wfd;
#endif
        break;
        // watch descriptor = the watched fd
    }
    default: return 0;
    }
    return svc_done_host(c);
}

static int svc_inotify_rm_watch(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3,
                                uint64_t a4, uint64_t a5) {
    (void)a0;
    (void)a1;
    (void)a2;
    (void)a3;
    (void)a4;
    (void)a5;
    switch (nr) {
    case 28: {
#if defined(__linux__)
        int result = inotify_rm_watch((int)a0, (int)a1);
        G_RET(c) = result < 0 ? (uint64_t)(-errno) : 0;
#else
        struct kevent kv;
        // inotify_rm_watch(fd, wd). The wd is the watched fd; deleting the EVFILT_VNODE knote from THIS
        // inotify kqueue is the source of truth for "is this a real watch of this instance". If the knote
        // is not registered here (bad/foreign wd), kevent fails ENOENT -- Linux returns EINVAL and leaves
        // the fd alone, so we must NOT close(wd) or we would silently destroy an unrelated guest fd.
        EV_SET(&kv, (int)a1, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
        if (kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        close((int)a1);
        G_RET(c) = 0;
#endif
        break;
    }
    default: return 0;
    }
    return svc_done_host(c);
}
