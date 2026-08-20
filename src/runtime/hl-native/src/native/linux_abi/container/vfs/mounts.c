static void inotify_object_assign(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_inotify[fd] || g_inotify_object[fd]) return;
    uint32_t serial = ++g_inotify_object_next;
    if (!serial) serial = ++g_inotify_object_next;
    g_inotify_object[fd] = ((uint64_t)(uint32_t)getpid() << 32) | serial;
}

// timerfd remaining-time tracking (lsys-timerfd-gettime): absolute CLOCK_MONOTONIC deadline (ns) of the
// next expiry + the interval (ns). timerfd_settime records them so timerfd_gettime reports it_value/interval.
static int64_t g_tfd_deadline[HL_NFD];
static int64_t g_tfd_interval[HL_NFD];
// timerfd aliases share pending expirations and checkpoint identity through one canonical slot.
static int g_tfd_cslot[HL_NFD];
static uint64_t g_tfd_pending[HL_NFD];
static int g_tfd_refs[HL_NFD];
static uint64_t g_tfd_object[HL_NFD];
static uint8_t g_tfd_nb[HL_NFD];

struct timerfd_shared_state {
    volatile int lock;
    int64_t deadline;
    int64_t interval;
    uint64_t pending;
};
static struct timerfd_shared_state *g_tfd_shared[HL_NFD];

static void timerfd_shared_lock(struct timerfd_shared_state *state) {
    while (__sync_lock_test_and_set(&state->lock, 1))
        sched_yield();
}

static void timerfd_shared_unlock(struct timerfd_shared_state *state) {
    __sync_lock_release(&state->lock);
}

static uint32_t g_tfd_object_next;

static int timerfd_slot(int fd) {
    if (fd >= 0 && fd < HL_NFD && g_tfd_cslot[fd] > 0) return g_tfd_cslot[fd] - 1;
    return fd;
}

static void timerfd_object_assign(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_timerfd[fd] || g_tfd_object[fd]) return;
    uint32_t serial = ++g_tfd_object_next;
    if (!serial) serial = ++g_tfd_object_next;
    g_tfd_object[fd] = ((uint64_t)(uint32_t)getpid() << 32) | serial;
    g_tfd_cslot[fd] = fd + 1;
    g_tfd_refs[fd] = 1;
}

// A periodic timerfd whose FIRST expiry (it_value) differs from its interval (it_interval) can't be
// expressed in a single kqueue EVFILT_TIMER (which fires first only after its period). So we arm a
// ONE-SHOT at the first delay and set this flag; on the first read() drain the timer is re-armed as a
// recurring periodic at g_tfd_interval. 1 = currently armed one-shot for the distinct first deadline.
static uint8_t g_tfd_first_oneshot[HL_NFD];
// The clockid the timerfd was created with (Linux CLOCK_REALTIME=0/MONOTONIC=1/BOOTTIME=7/REALTIME_ALARM=8/
// ...). A TFD_TIMER_ABSTIME deadline is expressed in THIS clock, so timerfd_settime must convert against it.
static int g_tfd_clock[HL_NFD];
// memfd sealing (lsys-memfd-seal): g_memfd_is[fd]=1 marks an anonymous memfd; g_memfd_seal[fd] carries the
// F_SEAL_* bitmask (F_SEAL_SEAL=1,SHRINK=2,GROW=4,WRITE=8,FUTURE_WRITE=16). A non-ALLOW_SEALING memfd starts
// already F_SEAL_SEAL'd, so further F_ADD_SEALS fail EPERM exactly as on Linux.
static uint8_t g_memfd_is[HL_NFD];
static int g_memfd_seal[HL_NFD];

#define MEMFD_REG_MAX 4096

struct memfd_reg_ent {
    uint64_t dev, ino;
    int seals;
};

struct memfd_reg {
    volatile int lock;
    int n;
    struct memfd_reg_ent e[MEMFD_REG_MAX];
};
static struct memfd_reg *g_memfd_reg;

static struct memfd_reg *memfd_reg(void) {
    void *arena = NULL;
    if (g_memfd_reg) return g_memfd_reg;
    if (hl_linux_shared_create(effective_host_services(), sizeof(struct memfd_reg), &arena) != HL_STATUS_OK)
        return NULL;
    g_memfd_reg = (struct memfd_reg *)arena;
    return g_memfd_reg;
}

static void memfd_reg_lock(struct memfd_reg *r) {
    while (__sync_lock_test_and_set(&r->lock, 1)) {}
}

static void memfd_reg_unlock(struct memfd_reg *r) {
    __sync_lock_release(&r->lock);
}

static int memfd_fd_id(int fd, uint64_t *dev, uint64_t *ino) {
    struct stat st;
    if (fd < 0 || fstat(fd, &st) != 0) return 0;
    *dev = (uint64_t)st.st_dev;
    *ino = (uint64_t)st.st_ino;
    return *ino != 0;
}

static void memfd_reg_set_id(uint64_t dev, uint64_t ino, int seals) {
    struct memfd_reg *r = memfd_reg();
    if (!r || !ino) return;
    memfd_reg_lock(r);
    for (int i = 0; i < r->n; i++) {
        if (r->e[i].dev == dev && r->e[i].ino == ino) {
            r->e[i].seals = seals;
            memfd_reg_unlock(r);
            return;
        }
    }
    if (r->n < MEMFD_REG_MAX) {
        int i = r->n++;
        r->e[i].dev = dev;
        r->e[i].ino = ino;
        r->e[i].seals = seals;
    }
    memfd_reg_unlock(r);
}

static void memfd_reg_set_fd(int fd, int seals) {
    uint64_t dev = 0, ino = 0;
    if (memfd_fd_id(fd, &dev, &ino)) memfd_reg_set_id(dev, ino, seals);
}

static int memfd_reg_get_fd(int fd, int *seals) {
    uint64_t dev = 0, ino = 0;
    if (!memfd_fd_id(fd, &dev, &ino)) return 0;
    struct memfd_reg *r = memfd_reg();
    if (!r) return 0;
    int found = 0, val = 0;
    memfd_reg_lock(r);
    for (int i = 0; i < r->n; i++) {
        if (r->e[i].dev == dev && r->e[i].ino == ino) {
            found = 1;
            val = r->e[i].seals;
            break;
        }
    }
    memfd_reg_unlock(r);
    if (!found) return 0;
    if (seals) *seals = val;
    return 1;
}

static int memfd_ensure_fd(int fd) {
    if (fd < 0 || fd >= HL_NFD) return 0;
    if (g_memfd_is[fd]) return 1;
    // Not flagged as a memfd on this fd yet. The only way an unflagged fd can still BE a memfd is if it was
    // received (SCM_RIGHTS) or inherited (fork/exec) after being created+sealed elsewhere -- and every such
    // memfd is recorded (dev/ino) in the fork-shared registry by memfd_create / F_ADD_SEALS. So if the
    // registry is empty (or not yet mapped in this process), no fd anywhere can match the dev/ino lookup
    // below: skip the per-write fstat(2) probe entirely. This is a pure fast path -- memfd_reg_get_fd would
    // return "not found" (0) in exactly these states -- and it makes every write(2)/pwrite/writev/ftruncate
    // to a NON-memfd fd (pipe/socket/file) one host fstat cheaper. A sealed memfd that is SCM-passed must
    // have been created+sealed before the fork that shares it, so the receiver sees a NON-empty registry and
    // still runs the full lookup below -- F_SEAL_WRITE stays enforced (see ipc_scm_memfd_seal).
    if (g_memfd_reg == NULL || g_memfd_reg->n == 0) return 0;
    int seals = 0;
    if (!memfd_reg_get_fd(fd, &seals)) return 0;
    g_memfd_is[fd] = 1;
    g_memfd_seal[fd] = seals;
    return 1;
}

static int memfd_seals_fd(int fd) {
    if (!memfd_ensure_fd(fd)) return 0;
    return (fd >= 0 && fd < HL_NFD) ? g_memfd_seal[fd] : 0;
}

// pipe read-pushback (tee(2)): tee() consumes bytes from the source pipe to copy them, then re-queues them
// here so the next read()/readv() on that fd re-serves them -> tee leaves the source pipe intact.
static uint8_t *g_fd_pushback[HL_NFD];
static size_t g_fd_pb_len[HL_NFD];
// pinned O_DIRECTORY fd to the rootfs (set at startup)
static int g_root_fd = -1;
/* Opaque twin used by the host-service resolver; legacy VFS paths retain g_root_fd until converted. */
static hl_host_handle g_root_handle = HL_HOST_HANDLE_INVALID;

/*
 * Pin the namespace root through the host-service ABI.  In container mode this
 * is the configured rootfs; a bare guest still needs an opaque handle for "/"
 * so absolute HOST_PATH opens do not fall back to the legacy native-fd lane.
 * Reinitialization (checkpoint/exec) replaces rather than leaks the old pin.
 */
static int root_handle_bind(const char *path) {
    const hl_host_file_services *file;
    const hl_host_posix_attachment_services *attachment;
    hl_host_result root;
    hl_host_result canonical;
    hl_host_result borrowed = {0};
    char canonical_path[sizeof g_rootfs_canon];

    if (g_host_services == NULL || g_host_services->file == NULL || path == NULL || path[0] == '\0') return -1;
    file = g_host_services->file;
    // The native twin is an OPTIONAL POSIX adapter (HL_HOST_CAP_POSIX_ATTACHMENT): a host with no native
    // descriptor to hand out leaves g_root_fd unbound rather than failing the pin, because the pin is a GATE
    // and not a functional dependency for a bare guest -- jail_routed_at() is identically 0 without a rootfs
    // and without a volume, so the confined walk (the only consumer of the native root) is never entered, and
    // the guest ELF already loads through the typed lane (image.c). Every remaining g_root_fd reader is
    // -1-safe: jail_pick()/jail_pick_idx() hand it back for equality tests (dispatch.c, overlay.c) or into
    // resolve_at's explicit `root_fd < 0 && !g_rootfs` fallback, engine_fd_reloc() ignores a slot that does
    // not equal the target fd, and engine_fd_vacate_range()/exec_fd_is_engine() filter on >= 0. A rootfs DOES
    // need the walk; root_native_require() below refuses it there.
    attachment = g_host_services->posix_attachment;
    root = file->open_relative(g_host_services->context, HL_HOST_HANDLE_CWD, path, strlen(path),
                               HL_HOST_FILE_READ | HL_HOST_FILE_DIRECTORY | HL_HOST_FILE_PATH_ONLY, 0, 0);
    if (root.status != HL_STATUS_OK) return -1;
    canonical = file->path(g_host_services->context, root.value,
                           (hl_host_bytes){(unsigned char *)canonical_path, sizeof(canonical_path) - 1});
    if (canonical.status != HL_STATUS_OK || canonical.value >= sizeof(canonical_path)) goto fail_root;
    canonical_path[canonical.value] = '\0';
    if (attachment != NULL) {
        borrowed = attachment->borrow_file_at_least(g_host_services->context, root.value, 1u << 20);
        if (borrowed.status != HL_STATUS_OK)
            borrowed = attachment->borrow_file_at_least(g_host_services->context, root.value, 64);
        if (borrowed.status != HL_STATUS_OK || borrowed.value > INT_MAX) goto fail_root;
        if (g_root_fd >= 0 &&
            attachment->release(g_host_services->context, (uint64_t)(unsigned)g_root_fd).status != HL_STATUS_OK)
            goto fail_borrowed;
    }
    if (g_root_handle != HL_HOST_HANDLE_INVALID &&
        file->close(g_host_services->context, g_root_handle).status != HL_STATUS_OK)
        goto fail_borrowed;
    g_root_handle = root.value;
    if (attachment != NULL) g_root_fd = (int)borrowed.value;
    if (g_rootfs != NULL) {
        memcpy(g_rootfs_canon, canonical_path, (size_t)canonical.value + 1);
        g_rootfs_canon_len = (size_t)canonical.value;
    }
    return 0;

fail_borrowed:
    if (attachment != NULL) (void)attachment->release(g_host_services->context, borrowed.value);
fail_root:
    (void)file->close(g_host_services->context, root.value);
    return -1;
}

static int root_native_require(void) {
    return g_root_fd >= 0 ? 0 : -1;
}

// Engine-private host fds (the rootfs dir-fd + each bind-mount volume dir-fd) share the guest's descriptor
// table in hl's in-process model. Opened at startup, right after stdio, they otherwise squat the LOW numbers
// Linux would leave free for the guest: g_root_fd lands on fd 3, shifting every guest fd allocation up by one
// AND becoming visible to the guest at a number a native run has free. s6-linux-init reads its
// notification pipe on the by-convention-lowest fd 3, which under hl was g_root_fd -- a DIRECTORY -> the
// read returns EISDIR ("unable to read from fd 3: Is a directory") and stage 1 aborts. Hoist each startup
// engine fd above a high floor so the guest's low fd space is exactly as on Linux (only 0/1/2 taken). Mirrors
// engine_fd_reloc's F_DUPFD floor (io.c) but relocates unconditionally, not just off a collision. Lazily
// created engine fds (the timer kqueue, the signalfd self-pipe) are made after the guest is running and take
// whatever is free then, so they never squat a fd the just-started guest relies on.
static int engine_fd_hoist(int fd) {
    if (fd < 3) return fd;                   // stdio (or a failed open) -> nothing to move
    int hi = fcntl(fd, F_DUPFD, 1 << 20);    // high floor; F_DUPFD returns the lowest free fd >= floor
    if (hi < 0) hi = fcntl(fd, F_DUPFD, 64); // floor beyond the guest's active low fds under a small RLIMIT
    if (hi < 0) return fd;                   // relocation failed -> keep the original (still functional)
    close(fd);
    return hi;
}

// Bind-mount volumes: a guest path prefix -> a host directory, each its own confined jail root.
struct vol {
    char guest[256];
    size_t glen;
    char hcanon[1024];
    size_t hlen;
    int fd;
    hl_host_handle handle; /* opaque twin of fd; rooted at the directory jail (or file parent) */
    int ro;                // 1 = read-only bind (`-v …:ro`): write-intent syscalls under `guest` fail EROFS
    int isfile;            // 1 = single-file bind (`-v host/f:/ctr/f`): `fd` is the host file's PARENT dir, `hcanon`
                           // is the file itself, and `guest` matches ONLY its exact path (a file has no children).
    int issymlink;         // projected link: resolve its target in the guest namespace
    int dead;              // 1 = detached by a runtime umount2(2): skipped by jail_match/jail_is_vol so the mount
                           // point reverts to the underlying rootfs/overlay content (the slot is never compacted --
                           // append-only keeps concurrent path resolves race-free).
};

#define HL_VOLUME_MAX 256

static struct vol g_vols[HL_VOLUME_MAX];
static int g_nvols;

#define HL_NAME_BIND_MAX 32
#define HL_NAME_BIND_ALIAS_MAX 8

struct name_bind {
    char names[HL_NAME_BIND_ALIAS_MAX][256];
    int names_count;
    char hcanon[1024];
    int fd;
};

static struct name_bind g_name_binds[HL_NAME_BIND_MAX];
static int g_name_binds_count;
static _Thread_local int g_name_bind_probe;

static int name_bind_pick(const char *guest);

static const char *name_bind_host_leaf(int index) {
    const char *slash = strrchr(g_name_binds[index].hcanon, '/');
    return slash ? slash + 1 : g_name_binds[index].hcanon;
}

static int add_name_bind(char *record) {
    if (!record || g_name_binds_count >= HL_NAME_BIND_MAX) return -1;
    char *field = strchr(record, '\t');
    if (!field) return -1;
    *field++ = 0;
    struct stat status;
    struct name_bind *bind = &g_name_binds[g_name_binds_count];
    memset(bind, 0, sizeof *bind);
    if (canonicalize_path(record, bind->hcanon, sizeof bind->hcanon) != 0 || stat(bind->hcanon, &status) != 0 ||
        !S_ISREG(status.st_mode))
        return -1;
    while (field && *field) {
        if (bind->names_count >= HL_NAME_BIND_ALIAS_MAX) return -1;
        char *next = strchr(field, '\t');
        if (next) *next++ = 0;
        if (!field[0] || !strcmp(field, ".") || !strcmp(field, "..") || strchr(field, '/') ||
            strlen(field) >= sizeof bind->names[0])
            return -1;
        for (int index = 0; index < bind->names_count; index++)
            if (!strcmp(field, bind->names[index])) return -1;
        snprintf(bind->names[bind->names_count++], sizeof bind->names[0], "%s", field);
        field = next;
    }
    if (bind->names_count == 0) return -1;
    char parent[1024];
    snprintf(parent, sizeof parent, "%s", bind->hcanon);
    char *slash = strrchr(parent, '/');
    if (!slash) return -1;
    if (slash == parent)
        parent[1] = 0;
    else
        *slash = 0;
    bind->fd = open(parent, O_RDONLY | O_DIRECTORY);
    if (bind->fd < 0) return -1;
    bind->fd = engine_fd_hoist(bind->fd);
    g_name_binds_count++;
    return 0;
}

static int name_binds_parse(const char *spec) {
    if (!spec || !spec[0]) return 0;
    char *copy = strdup(spec);
    if (!copy) return -1;
    char *save = NULL;
    for (char *record = strtok_r(copy, "\n", &save); record; record = strtok_r(NULL, "\n", &save))
        if (add_name_bind(record) != 0) {
            free(copy);
            return -1;
        }
    free(copy);
    return 0;
}

static void vol_handle_bind(struct vol *volume, const char *directory) {
    hl_host_result opened;
    if (volume == NULL) return;
    volume->handle = HL_HOST_HANDLE_INVALID;
    if (g_host_services == NULL || g_host_services->file == NULL || g_host_services->file->open_relative == NULL ||
        directory == NULL)
        return;
    opened =
        g_host_services->file->open_relative(g_host_services->context, HL_HOST_HANDLE_CWD, directory, strlen(directory),
                                             HL_HOST_FILE_READ | HL_HOST_FILE_DIRECTORY | HL_HOST_FILE_PATH_ONLY, 0, 0);
    if (opened.status == HL_STATUS_OK) volume->handle = opened.value;
}

// Materialize a volume's mount point (and every ancestor) as empty dirs in the writable rootfs/upper, the
// way Docker mkdir -p's each mount target inside the container rootfs. Without it a NESTED mount leaves its
// parent absent: `-v H:/x/y` makes `/x/y` resolve to the host dir, but `ls /x` ENOENTs because `/x` exists
// in no layer. Creating /x (and /x/y) in the upper lets the merged readdir list `/x` -> `y`; the mount
// itself still wins in jail_pick(), so `/x/y` shows the host files, not the empty placeholder. The rootfs
// is the per-container overlay upper (daemon) or the plain rootfs (manual) -- both writable & private.
// No-op until the rootfs is known (the bridge supplies HL_VOLUMES after container_init resolves g_rootfs_canon).
// A file mount's leaf is created as an empty placeholder FILE (not a dir) so a parent `ls` shows it as a
// file, exactly as Docker materializes a single-file bind target inside the rootfs.
static void vol_mkmountpoint(const char *guest, int isfile) {
    if (!g_rootfs_canon[0] || !guest || guest[0] != '/') return;
    char mp[4300];
    if ((size_t)snprintf(mp, sizeof mp, "%s%s", g_rootfs_canon, guest) >= sizeof mp) return;
    for (char *s = mp + g_rootfs_canon_len + 1; *s; s++)
        if (*s == '/') {
            *s = 0;
            hl_compat_mkdir(mp, 0755);
            *s = '/';
        }
    if (isfile) {
        int fd = open(mp, O_CREAT | O_RDONLY, 0644);
        if (fd >= 0) close(fd);
    } else
        hl_compat_mkdir(mp, 0755);
}

static int volume_hex(unsigned char value) {
    if (value >= '0' && value <= '9') return value - '0';
    if (value >= 'a' && value <= 'f') return value - 'a' + 10;
    if (value >= 'A' && value <= 'F') return value - 'A' + 10;
    return -1;
}

static int volume_unescape(char *value) {
    char *read = value;
    char *write = value;
    while (*read) {
        if (*read == '%') {
            int high = volume_hex((unsigned char)read[1]);
            int low = volume_hex((unsigned char)read[2]);
            if (high < 0 || low < 0 || (high == 0 && low == 0)) return -1;
            *write++ = (char)((high << 4) | low);
            read += 3;
        } else {
            *write++ = *read++;
        }
    }
    *write = 0;
    return 0;
}

static void add_vol(const char *spec) { // "[v2:][ro:]guestpath:hostdir" -> a confined bind-mount volume
    if (g_nvols >= HL_VOLUME_MAX) return;
    int escaped = 0;
    if (!strncmp(spec, "v2:", 3)) {
        escaped = 1;
        spec += 3;
    }
    // Optional read-only marker. A guest path always begins with '/', so a leading "ro:"/"rw:" token is
    // unambiguous; absent (the legacy `guest:host` form) it defaults to read-write -> byte-identical.
    int ro = 0;
    int preserve_link = 0;
    if (!strncmp(spec, "link:", 5)) {
        ro = 1;
        preserve_link = 1;
        spec += 5;
    } else if (!strncmp(spec, "ro:", 3)) {
        ro = 1;
        spec += 3;
    } else if (!strncmp(spec, "rw:", 3)) {
        spec += 3;
    }
    char tmp[4096];
    if (path_copy(tmp, sizeof tmp, spec) != 0) return;
    char *col = strchr(tmp, ':');
    if (!col || tmp[0] != '/') return;
    *col = 0;
    if (escaped && (volume_unescape(tmp) != 0 || volume_unescape(col + 1) != 0)) return;
    struct vol *v = &g_vols[g_nvols];
    v->ro = ro;
    if (path_copy(v->guest, sizeof v->guest, tmp) != 0) return;
    v->glen = strlen(v->guest);
    while (v->glen > 1 && v->guest[v->glen - 1] == '/')
        v->guest[--v->glen] = 0;
    if ((preserve_link ? canonicalize_link_path(col + 1, v->hcanon, sizeof v->hcanon)
                       : canonicalize_path(col + 1, v->hcanon, sizeof v->hcanon)) != 0)
        return;
    v->hlen = strlen(v->hcanon);
    struct stat hst;
    if ((preserve_link ? lstat(v->hcanon, &hst) : stat(v->hcanon, &hst)) == 0 && !S_ISDIR(hst.st_mode)) {
        // Single-file bind (regular file, but ALSO a socket / fifo / device): openat's jail base must be a
        // directory, so pin the source's PARENT dir as `fd` and route the exact mount point straight to
        // `hcanon`. Dropping the O_DIRECTORY requirement here is what lets a non-dir source register at all
        // (it ENOTDIRs otherwise -> the mount was silently lost). Matching on !S_ISDIR (not just S_ISREG)
        // is what makes a bind-mounted Unix socket — e.g. the docker daemon socket — resolve so the guest's
        // connect() dials the real host socket instead of ENOENT.
        v->isfile = 1;
        v->issymlink = preserve_link;
        char par[1024];
        snprintf(par, sizeof par, "%s", v->hcanon);
        char *sl = strrchr(par, '/');
        if (!sl) return;
        if (sl == par)
            par[1] = 0; // file directly under "/" -> parent is "/"
        else
            *sl = 0;
        if ((v->fd = open(par, O_RDONLY | O_DIRECTORY)) < 0) return;
        vol_handle_bind(v, par);
    } else if ((v->fd = open(v->hcanon, O_RDONLY | O_DIRECTORY)) < 0)
        return;
    else
        vol_handle_bind(v, v->hcanon);
    v->fd = engine_fd_hoist(v->fd); // keep this engine dir-fd out of the guest's low fd range
    g_nvols++;
    vol_mkmountpoint(v->guest, v->isfile);
}

// Runtime bind/tmpfs volume registration for mount(2): like add_vol but takes an already-resolved host
// backing (a real dir or a single file) + a guest target directly -- no "spec" string, so a guest path
// containing ':' can never be misparsed. g_nvols is published LAST (release), so a concurrent path
// resolver sees either the old count or a fully-populated entry (never a half-written one). The mount
// point (+ ancestors) is materialized in the writable upper so a parent `ls` shows it. 0 or -errno.
static int rt_add_vol(const char *guest, const char *hostsrc, int ro) {
    if (!guest || guest[0] != '/' || !hostsrc) return -EINVAL;
    if (g_nvols >= HL_VOLUME_MAX) return -ENOMEM;
    struct vol *v = &g_vols[g_nvols];
    memset(v, 0, sizeof *v);
    v->ro = ro ? 1 : 0;
    snprintf(v->guest, sizeof v->guest, "%s", guest);
    v->glen = strlen(v->guest);
    while (v->glen > 1 && v->guest[v->glen - 1] == '/')
        v->guest[--v->glen] = 0;
    if (canonicalize_path(hostsrc, v->hcanon, sizeof v->hcanon) != 0) {
        int e = errno;
        return e ? -e : -ENOENT;
    }
    v->hlen = strlen(v->hcanon);
    struct stat hst;
    if (stat(v->hcanon, &hst) == 0 && !S_ISDIR(hst.st_mode)) {
        // Single-file (or socket/fifo/device) bind: pin the source's PARENT dir as the jail base and route
        // the exact mount point straight to `hcanon` (see add_vol for the full rationale).
        v->isfile = 1;
        char par[1024];
        snprintf(par, sizeof par, "%s", v->hcanon);
        char *sl = strrchr(par, '/');
        if (!sl) return -EINVAL;
        if (sl == par)
            par[1] = 0;
        else
            *sl = 0;
        if ((v->fd = open(par, O_RDONLY | O_DIRECTORY)) < 0) return -errno;
        vol_handle_bind(v, par);
    } else if ((v->fd = open(v->hcanon, O_RDONLY | O_DIRECTORY)) < 0)
        return -errno;
    else
        vol_handle_bind(v, v->hcanon);
    v->fd = engine_fd_hoist(v->fd);
    vol_mkmountpoint(v->guest, v->isfile);
    __atomic_store_n(&g_nvols, g_nvols + 1, __ATOMIC_RELEASE); // publish the complete entry LAST
    return 0;
}

// Detach the bind/tmpfs volume mounted at EXACTLY `guest` (runtime umount2). Marks the slot dead (never
// compacted -> race-free) so the path reverts to the underlying rootfs/overlay. 0 if one was detached,
// -EINVAL if no volume is mounted there (Linux umount of a non-mount-point).
static int rt_del_vol(const char *guest) {
    int nv = __atomic_load_n(&g_nvols, __ATOMIC_ACQUIRE), hit = -EINVAL;
    for (int i = 0; i < nv; i++)
        if (!g_vols[i].dead && !strcmp(g_vols[i].guest, guest)) {
            g_vols[i].dead = 1;
            if (g_vols[i].handle != HL_HOST_HANDLE_INVALID && g_host_services && g_host_services->file &&
                g_host_services->file->close) {
                (void)g_host_services->file->close(g_host_services->context, g_vols[i].handle);
                g_vols[i].handle = HL_HOST_HANDLE_INVALID;
            }
            hit = 0;
        }
    return hit;
}

// Longest matching bind-mount volume for an absolute guest path (the DEEPEST mount wins, exactly as the
// kernel routes a path to the innermost mount), or -1 for the rootfs/overlay jail. Longest-prefix so a
// nested volume (`-v H1:/x/y -v H2:/x/y/z`) routes /x/y/z to the inner mount regardless of registration
// order; for non-nested volumes (no guest is a prefix of another) it is identical to a first-match scan.
static int jail_match(const char *abs) {
    int best = -1;
    size_t blen = 0;
    int nv = __atomic_load_n(&g_nvols, __ATOMIC_ACQUIRE);
    for (int i = 0; i < nv; i++) {
        if (g_vols[i].dead) continue; // runtime-umounted: no longer routes here
        char b = abs[g_vols[i].glen];
        // A projected symlink owns suffixes through its guest target; ordinary
        // single-file binds still match only their exact mount point.
        int hit = g_vols[i].isfile && !g_vols[i].issymlink ? (b == 0) : (b == '/' || b == 0);
        if (g_vols[i].glen > blen && hit && !strncmp(abs, g_vols[i].guest, g_vols[i].glen)) {
            best = i;
            blen = g_vols[i].glen;
        }
    }
    return best;
}

// Whether `abs` is an exact single-socket bind mount supplied by the host. Connections to these endpoints
// leave the engine process (Wayland, Docker, provider services, ...), so their SCM_RIGHTS records must contain
// only the descriptors requested by the public protocol. Engine-private descriptor metadata trailers are
// meaningful only when another engine endpoint receives and removes them.
static int jail_is_projected_socket(const char *abs) {
    int index = jail_match(abs);
    if (index < 0 || !g_vols[index].isfile || strcmp(abs, g_vols[index].guest) != 0) return 0;
    struct stat status;
    return stat(g_vols[index].hcanon, &status) == 0 && S_ISSOCK(status.st_mode);
}

// Basename of a file bind-mount's host source: the leaf to openat under the pinned parent-dir `fd`.
static const char *vol_fbase(int vi) {
    const char *sl = strrchr(g_vols[vi].hcanon, '/');
    return sl ? sl + 1 : g_vols[vi].hcanon;
}

// Pick the jail (rootfs or a volume) for an absolute guest path; *rel = the path within that jail.
static int jail_pick(const char *abs, const char **canon, size_t *clen, const char **rel) {
    int i = jail_match(abs);
    if (i >= 0) {
        if (canon) {
            *canon = g_vols[i].hcanon;
            *clen = g_vols[i].hlen;
        }
        *rel = abs[g_vols[i].glen] ? abs + g_vols[i].glen : "/";
        return g_vols[i].fd;
    }
    if (canon) {
        *canon = g_rootfs_canon;
        *clen = g_rootfs_canon_len;
    }
    *rel = abs;
    return g_root_fd;
}

// SECURE path resolution. confine() handles '..' lexically, but symlinks resolve in the kernel
// BELOW that layer (a mid-path symlink to '/' walks straight out), so lexical clamping is NOT a
// boundary. This realpath()s the deepest existing prefix (following ALL symlinks) and verifies
// the canonical result is inside g_rootfs_canon; anything that escapes is redirected to a
// guaranteed-nonexistent in-jail path (-> ENOENT). `nofollow` keeps the final component
// unresolved (for readlink/lstat). Returns 1 if inside the jail, 0 if an escape was blocked.
// ---- positive dentry/climb cache (dc_*; impl in fscache.c next to the rc_/oc_/updirneg caches) ----
// Memoizes confine_in_m's realpath climb per DIRECTORY: key = the exact pre-realpath host string
// (jail canon + normalized rel, final component peeled in nofollow mode); value = (canonical deepest
// EXISTING prefix, #trailing components missing). Epoch-gated on the container-shared g_res_epoch,
// hard-reset on fork/chroot (hl_fdcache_reset), and volumes are never cached. See the full
// correctness model at the impl. DC_KEYMAX bounds the fixed-size slots (longer paths bypass, safely).
#define DC_KEYMAX 320

#include "case_names.h"

#ifdef __APPLE__
/* APFS commonly folds case. Keep the guest namespace case-sensitive by selecting an exact
 * guest-visible sibling and reversibly escaping a missing colliding component. */
static int hl_case_path(const char *root, const char *guest, char *physical, size_t capacity) {
    char directory[8400];
    if (snprintf(directory, sizeof directory, "%s", root) >= (int)sizeof directory) return -ENAMETOOLONG;
    size_t used = 0;
    physical[0] = 0;
    const char *cursor = guest;
    while (*cursor) {
        while (*cursor == '/') cursor++;
        if (!*cursor) break;
        const char *end = strchr(cursor, '/');
        size_t size = end ? (size_t)(end - cursor) : strlen(cursor);
        if (size == 0 || size >= 256) return -ENAMETOOLONG;
        char requested[256], selected[768] = "";
        memcpy(requested, cursor, size);
        requested[size] = 0;
        int collision = 0;
        DIR *entries = opendir(directory);
        if (entries != NULL) {
            struct dirent *entry;
            while ((entry = readdir(entries)) != NULL) {
                char decoded[256];
                const char *visible =
                    hl_case_name_decode(entry->d_name, decoded, sizeof decoded) ? decoded : entry->d_name;
                if (strcmp(visible, requested) == 0) {
                    snprintf(selected, sizeof selected, "%s", entry->d_name);
                    break;
                }
                if (strcasecmp(visible, requested) == 0) collision = 1;
            }
            closedir(entries);
        }
        if (!selected[0]) {
            if (collision || hl_case_name_requires_encoding(requested)) {
                int error = hl_case_name_encode(requested, selected, sizeof selected);
                if (error != 0) return error;
            } else {
                snprintf(selected, sizeof selected, "%s", requested);
            }
        }
        size_t selected_size = strlen(selected);
        if (used + selected_size + 2 > capacity) return -ENAMETOOLONG;
        physical[used++] = '/';
        memcpy(physical + used, selected, selected_size + 1);
        used += selected_size;
        if (strlen(directory) + selected_size + 2 > sizeof directory) return -ENAMETOOLONG;
        strcat(directory, "/");
        strcat(directory, selected);
        cursor = end ? end + 1 : cursor + size;
    }
    if (used == 0) {
        if (capacity < 2) return -ENAMETOOLONG;
        physical[0] = '/';
        physical[1] = 0;
    }
    return 0;
}
#endif

// Core: confine `rel` within an explicit jail root (jcanon). Generalized from secure_resolve so the
// overlay can resolve the SAME guest path inside each layer's root, reusing the realpath boundary.
// `missing` (optional): the number of trailing DIRECTORY components that did NOT exist under the jail
// root (the climb-loop pops below). 0 => the parent chain fully exists. The overlay uses this to prove
// "this entry cannot exist in the upper" (and no whiteout/opaque marker can either) without extra probes.
static int confine_in_m(const char *jcanon, size_t jclen, const char *rel, char *out, size_t n, int nofollow,
                        int *missing) {
    if (missing) *missing = 0;
    char norm[4200];
    confine(rel, norm, sizeof norm);
#ifdef __APPLE__
    char physical[4200];
    if (hl_case_path(jcanon, norm, physical, sizeof physical) == 0) snprintf(norm, sizeof norm, "%s", physical);
#endif
    char h[8400];
    snprintf(h, sizeof h, "%s%s", jcanon, norm);
    char rem[4400] = "";
    // peel the final component, resolve its dir
    if (nofollow) {
        char *sl = strrchr(h, '/');
        if (sl && (size_t)(sl - h) >= jclen) {
            snprintf(rem, sizeof rem, "/%s", sl + 1);
            *sl = 0;
        }
        if (!h[0]) snprintf(h, sizeof h, "/");
    }
    // Dentry-cache fast path: `h` is exactly the string the climb below would hand to realpath() first,
    // so an epoch-valid entry replays the recorded outcome verbatim -- out = canon + the nmiss trailing
    // components the climb popped (a plain suffix of the key) + rem -- with ZERO realpath calls. In
    // nofollow mode the final component was already peeled into `rem` above, so all files in one
    // directory share the key (the per-DIRECTORY sharing a stat/open storm needs). Only rootfs/lower
    // jails are cached; a miss or an over-length path falls through to the untouched climb.
    int dcok = hl_fdcache_dentry_cacheable(jcanon);
    char hkey[DC_KEYMAX];
    if (dcok) {
        size_t hl = strlen(h);
        if (hl < sizeof hkey)
            memcpy(hkey, h, hl + 1);
        else
            dcok = 0;
    }
    if (dcok) {
        char dcanon[DC_KEYMAX];
        int k;
        if (hl_fdcache_dentry_lookup(hkey, dcanon, sizeof dcanon, &k)) {
            const char *p = hkey + strlen(hkey); // start of the k popped components ("" when k == 0)
            for (int i = 0; i < k; i++) {
                p--;
                while (p > hkey && *p != '/')
                    p--;
            }
            snprintf(out, n, "%s%s%s", dcanon, p, rem);
            if (missing) *missing = k;
            return 1;
        }
    }
    int pops = 0;
    for (;;) {
        char canon[4200];
        if (realpath(h, canon)) {
            int inside = strncmp(canon, jcanon, jclen) == 0 && (canon[jclen] == '/' || canon[jclen] == 0);
            if (!inside) {
                snprintf(out, n, "%s/.jail-escape-denied", jcanon);
                return 0;
            }
            snprintf(out, n, "%s%s", canon, rem);
            // Memoize the successful in-jail climb (canon was verified inside the jail just above);
            // escapes and exhausted climbs (the return-0 paths) are never cached.
            if (dcok) hl_fdcache_dentry_store(hkey, canon, pops);
            return 1;
        }
        // final missing? climb to the deepest existing dir
        char *sl = strrchr(h, '/');
        if (!sl || strlen(h) <= jclen) {
            snprintf(out, n, "%s/.jail-escape-denied", jcanon);
            return 0;
        }
        char tmp[4400];
        snprintf(tmp, sizeof tmp, "/%s%s", sl + 1, rem);
        snprintf(rem, sizeof rem, "%s", tmp);
        *sl = 0;
        pops++;
        if (missing) (*missing)++;
    }
}

static int confine_in(const char *jcanon, size_t jclen, const char *rel, char *out, size_t n, int nofollow) {
    return confine_in_m(jcanon, jclen, rel, out, n, nofollow, NULL);
}

// secure_resolve + two probe outputs the overlay's fast path uses (both optional):
//   `missing` -- trailing dir components of the path that do NOT exist under the chosen jail root
//                (see confine_in_m); lets overlay_lookup prove an upper entry/whiteout/opaque marker
//                cannot exist without paying the extra lstat probes.
//   `isvol`   -- the path routed to a bind-mount VOLUME jail, not the rootfs/overlay upper. Volume
//                backings are host-mutable (the user can create files from macOS at any time), so the
//                overlay's negative memo must never cache them (mirrors hl_fdcache_metadata_store's volume exclusion).
static int secure_resolve_probe(const char *guest, char *out, size_t n, int nofollow, int *missing, int *isvol) {
    if (isvol) *isvol = 0;
    if (missing) *missing = 0;
    // Normalize '.'/'//'/'..' and clamp at the ROOTFS root FIRST, then route. Jail selection must see the
    // post-`..` path: a `..` that pops above a volume's own root crosses the bind-mount boundary back to
    // the dir holding the mount point ("/x/y/.." -> "/x"), which lives in the rootfs/overlay jail, not the
    // volume. Routing the raw path would prefix-match "/x/y/.." to the volume and clamp `..` at the volume
    // root. confine() already collapses `..` lexically below (so this only changes WHICH jail is chosen,
    // not the symlink-via-realpath confinement) and never ascends past '/', so the result stays in rootfs.
    char cr[4200];
    if (g_chroot[0]) { // re-root under the guest's chroot first (no-op cost when no chroot is in effect)
        chroot_apply(guest, cr, sizeof cr);
        guest = cr;
    }
    char norm[4200];
    confine(guest, norm, sizeof norm);
    // Single-file bind-mount: the exact mount point maps straight to the bound host file (`hcanon` is the
    // realpath'd file, not a dir to walk). jail_match only matches a file vol on its exact path, so a hit
    // here IS that file -- emit it directly; confine_in would append rel ("/") and ENOTDIR on the file.
    int fvi = jail_match(norm);
    int exact_volume = fvi >= 0 && strcmp(norm, g_vols[fvi].guest) == 0;
    if (fvi >= 0 && g_vols[fvi].isfile && (!g_vols[fvi].issymlink || (nofollow && exact_volume))) {
        if (isvol) *isvol = 1;
        snprintf(out, n, "%s", g_vols[fvi].hcanon);
        return 1;
    }
    const char *jcanon;
    size_t jclen;
    const char *rel;
    // rootfs or a volume root (jcanon is absolute)
    jail_pick(norm, &jcanon, &jclen, &rel);
    if (isvol && jcanon != g_rootfs_canon) *isvol = 1; // jail_pick hands back the global array for rootfs
    return confine_in_m(jcanon, jclen, rel, out, n, nofollow, missing);
}

static int resolve_at(const char *guest, char *final, size_t fn, int nofollow);

static int secure_resolve(const char *guest, char *out, size_t n, int nofollow) {
    char final[512], parent[4200];
    int descriptor = resolve_at(guest, final, sizeof final, nofollow);
    if (descriptor >= 0) {
        int ok = hl_native_fd_path(descriptor, parent, sizeof parent) == 0 && path_join(out, n, parent, final) == 0;
        close(descriptor);
        if (ok) return 1;
    }
    return secure_resolve_probe(guest, out, n, nofollow, NULL, NULL);
}

#include "overlay.c"

static const struct hl_linux_vfs_namespace g_vfs_namespace = {
    g_rootfs_canon,
    &g_rootfs_canon_len,
    g_lower,
    &g_nlower,
};

// final NOT followed (readlink/lstat)
static const char *xlate(const char *p, char *buf, size_t n) {
    if (p && p[0] == '/' && (g_rootfs || jail_match(p) >= 0)) {
        secure_resolve(p, buf, n, 1);
        return buf;
    }
    return p;
}

// follow symlinks (open/stat/exec)
static const char *xresolve(const char *p, char *buf, size_t n) {
    if (p && p[0] == '/' && (g_rootfs || jail_match(p) >= 0)) {
        secure_resolve(p, buf, n, 0);
        return buf;
    }
    return p;
}

// Follow a guest-visible symlink chain while preserving its guest spelling.  Host-path resolution
// deliberately cannot represent synthetic targets such as /proc/self/fd/N: those names have no host
// inode, but open(2) still has to recognize them after traversing an image symlink such as
// /var/log/nginx/error.log -> /dev/stderr -> /proc/self/fd/2.  Return the last guest name even when it
// names a synthetic endpoint; callers classify that endpoint before attempting a host-backed open.
static const char *guest_symlink_target(const char *path, char *out, size_t capacity) {
    if (path == NULL || path[0] != '/' || capacity == 0) return path;
    char current[4200];
    if (path_copy(current, sizeof current, path) != 0) return path;
    for (int hop = 0; hop < 40; ++hop) {
        char host[4200];
        int present;
        if (g_nlower)
            present = overlay_resolve(current, host, sizeof host, 1);
        else {
            secure_resolve(current, host, sizeof host, 1);
            struct stat probe;
            present = lstat(host, &probe) == 0;
        }
        if (!present) break;
        struct stat metadata;
        if (lstat(host, &metadata) != 0 || !S_ISLNK(metadata.st_mode)) break;
        char target[4200];
        ssize_t length = readlink(host, target, sizeof target - 1);
        if (length <= 0) break;
        target[length] = 0;
        if (target[0] == '/') {
            if (path_copy(current, sizeof current, target) != 0) return path;
        } else {
            char directory[4200], joined[8400];
            if (path_copy(directory, sizeof directory, current) != 0) return path;
            char *separator = strrchr(directory, '/');
            if (separator != NULL)
                *separator = 0;
            else
                directory[0] = 0;
            if (snprintf(joined, sizeof joined, "%s/%s", directory, target) >= (int)sizeof joined ||
                path_copy(current, sizeof current, joined) != 0)
                return path;
        }
    }
    if (path_copy(out, capacity, current) != 0) return path;
    return out;
}

static int jail_at(int dirfd, const char *raw, char *final, size_t fn, int nofollow);

// Resolve an EXEC entrypoint (or PT_INTERP) to a host path, following symlinks the way the kernel
// would INSIDE the rootfs: an absolute symlink target (`/bin/sh -> /bin/busybox`) is rootfs-relative,
// not host-relative -- realpath() can't do this (it follows the target against the host root). Each
// hop is re-confined via secure_resolve, so an escaping link lands on .jail-escape-denied and fails.
static const char *xresolve_exec(const char *p, char *buf, size_t n) {
    if (!(p && p[0] == '/')) return p;
    // Bare launches can still have typed bind volumes.  They do not need the rootfs-specific absolute
    // symlink rewriting below, but must resolve inside the volume jail like every other followed lookup.
    if (!g_rootfs) {
        if (jail_match(p) < 0) return p;
        char final[512];
        int dfd = jail_at(-100, p, final, sizeof final, 0);
        if (dfd >= 0) {
            int fd = openat(dfd, final, O_RDONLY);
            close(dfd);
            if (fd >= 0) {
                if (hl_native_fd_path(fd, buf, n) == 0) {
                    close(fd);
                    return buf;
                }
                close(fd);
            }
        }
        return xresolve(p, buf, n);
    }
    char cur[4200];
    snprintf(cur, sizeof cur, "%s", p);
    // bounded symlink chain
    int hop;
    for (hop = 0; hop < 40; hop++) {
        char hb[4200];
        // host path, final component NOT followed
        secure_resolve(cur, hb, sizeof hb, 1);
        struct stat st;
        // missing -> let the loader report it
        if (lstat(hb, &st) != 0) break;
        if (!S_ISLNK(st.st_mode)) {
            snprintf(buf, n, "%s", hb);
            return buf;
            // real file -> done
        }
        char tgt[4200];
        ssize_t k = readlink(hb, tgt, sizeof tgt - 1);
        if (k <= 0) break;
        tgt[k] = 0;
        if (tgt[0] == '/')
            // absolute target: rootfs-relative
            snprintf(cur, sizeof cur, "%s", tgt);
        else {
            char d[4200];
            snprintf(d, sizeof d, "%s", cur);
            char *sl = strrchr(d, '/');
            if (sl) *sl = 0;
            char j[8400];
            snprintf(j, sizeof j, "%s/%s", d, tgt);
            if (path_copy(cur, sizeof cur, j) != 0) {
                if (n) buf[0] = 0;
                return buf;
            }
            // relative to its dir
        }
    }
    if (hop == 40)
        resolve_loop_mark(); // >40 symlink hops -> ELOOP (the guest-absolute self-loop the fallback can't follow)
    secure_resolve(cur, buf, n, 0);
    // fallback: realpath-confine the last hop
    return buf;
}

// Copy the container's PATH value (from the forwarded HL_GUEST_ENV, "K=V\nK=V") into `out`, or
// leave "" if PATH is unset/empty. This is the image-config PATH (e.g. golang's /usr/local/go/bin:...)
// merged with any `docker run/exec -e PATH=` override -- the authoritative search path for bare commands.
