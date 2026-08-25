/* Cross-process view of typed logical descriptors. The host fd table contains reservation shadows at
 * guest-visible numbers, so peer /proc/<pid>/fd resolves the logical fd's stable OFD identity to a persistent
 * peer descriptor before asking the host process service for vnode/socket information. Entries live in
 * pre-fork shared memory and are generation stamped so readers never combine old identity with fd reuse. */
#ifndef FDVIS_N
#define FDVIS_N 131072
#endif
#define FDPATH_N 8192

struct fdvis_slot {
    uint64_t key; /* host pid in high 32 bits; guest fd + 1 in low 32 bits; 0 = free */
    uint64_t generation;
    uint64_t owner_start_ns;
    uint32_t kind;
    /* Who holds this slot's RESERVATION, or 0. Set only while key == UINT64_MAX, and it is what makes an
       abandoned reservation reclaimable: the marker overwrites the key, so the pid the sweeps decode from
       the high 32 bits is -1 and every one of them skipped the slot forever. This field was declared as
       `reserved` padding and never read or written anywhere in the tree, so carrying it costs nothing --
       the struct is the same 48 bytes and the shared table is still 6 MiB. */
    int32_t reserver_pid;
    uint64_t device;
    uint64_t object;
};

/* This table is shared across fork and its arena is sliced by sizeof(struct fdvis_slot). The reservation
   owner deliberately occupies the former 32-bit reserved cell; pin every boundary that makes that an
   in-place semantic reuse rather than a silent shared-layout change. */
_Static_assert(sizeof(struct fdvis_slot) == 48, "fdvis slot shared layout size changed");
_Static_assert(_Alignof(struct fdvis_slot) == 8, "fdvis slot shared layout alignment changed");
_Static_assert(offsetof(struct fdvis_slot, key) == 0, "fdvis key offset changed");
_Static_assert(offsetof(struct fdvis_slot, generation) == 8, "fdvis generation offset changed");
_Static_assert(offsetof(struct fdvis_slot, owner_start_ns) == 16, "fdvis owner token offset changed");
_Static_assert(offsetof(struct fdvis_slot, kind) == 24, "fdvis kind offset changed");
_Static_assert(offsetof(struct fdvis_slot, reserver_pid) == 28, "fdvis reservation owner left reserved cell");
_Static_assert(offsetof(struct fdvis_slot, device) == 32, "fdvis device offset changed");
_Static_assert(offsetof(struct fdvis_slot, object) == 40, "fdvis object offset changed");
static struct fdvis_slot *g_fdvis;

struct fdpath_slot {
    uint64_t key;
    uint64_t owner_start_ns;
    uint8_t path_is_guest;
    char path[sizeof g_fdpath[0]];
};
static struct fdpath_slot *g_fdpaths;

struct fdvis_control {
    _Atomic uint64_t owner;
    uint64_t generation;
};
static struct fdvis_control *g_fdvis_control;
static int g_fdvis_fork_parent;
static uint64_t fdvis_key(int pid, int fd);
static uint64_t fdvis_process_token(int pid);
static void fdpath_sweep_stale_locked(void);
static void fdvis_sweep_stale_locked(void);
static int fdvis_self(int *pid, uint64_t *token);
#if defined(HL_NATIVE_TEST_HOOKS)
static int fdvis_after_fork_rollback_test(void);
static int fdvis_stalled_parent_test(void);
static int fdvis_corpse_holder_test(void);
static int fdvis_recursive_identity_test(void);
#endif

static struct fdpath_slot *fdpath_find(uint64_t key, uint64_t owner_start_ns, int claim) {
    unsigned start = (unsigned)((key ^ (key >> 32)) * UINT64_C(2654435761)) & (FDPATH_N - 1);
    struct fdpath_slot *tombstone = NULL;
    for (unsigned probe = 0; probe < FDPATH_N; ++probe) {
        struct fdpath_slot *slot = &g_fdpaths[(start + probe) & (FDPATH_N - 1)];
        if (slot->key == key && slot->owner_start_ns == owner_start_ns) return slot;
        if (slot->key == UINT64_MAX) {
            if (claim && !tombstone) tombstone = slot;
            continue;
        }
        if (slot->key == 0) {
            if (!claim) return NULL;
            slot = tombstone ? tombstone : slot;
            memset(slot, 0, sizeof *slot);
            slot->key = key;
            slot->owner_start_ns = owner_start_ns;
            return slot;
        }
    }
    if (claim && tombstone) {
        memset(tombstone, 0, sizeof *tombstone);
        tombstone->key = key;
        tombstone->owner_start_ns = owner_start_ns;
        return tombstone;
    }
    return NULL;
}

static void fdpath_delete_locked(struct fdpath_slot *slot) {
    memset(slot, 0, sizeof *slot);
    slot->key = UINT64_MAX;
}

static void fdpath_cleanup_owner_locked(int owner, uint64_t owner_start_ns) {
    for (unsigned index = 0; index < FDPATH_N; ++index)
        if (g_fdpaths[index].key != UINT64_MAX && (int)(uint32_t)(g_fdpaths[index].key >> 32) == owner &&
            g_fdpaths[index].owner_start_ns == owner_start_ns)
            fdpath_delete_locked(&g_fdpaths[index]);
}

static int proc_fdvis_publish_path_locked(int pid, uint64_t owner_start_ns, int guest_fd) {
    uint64_t key = fdvis_key(pid, guest_fd);
    size_t length = strnlen(g_fdpath[guest_fd], sizeof g_fdpath[guest_fd]);
    if (length == sizeof g_fdpath[guest_fd]) return -ENAMETOOLONG;
    struct fdpath_slot *slot = fdpath_find(key, owner_start_ns, g_fdpath[guest_fd][0] != '\0');
    if (!slot && g_fdpath[guest_fd][0] != '\0') {
        fdpath_sweep_stale_locked();
        slot = fdpath_find(key, owner_start_ns, 1);
    }
    if (g_fdpath[guest_fd][0] == '\0') {
        if (slot) fdpath_delete_locked(slot);
        return 0;
    }
    if (!slot) return -ENOSPC;
    slot->path_is_guest = g_fdpath_guest[guest_fd];
    memcpy(slot->path, g_fdpath[guest_fd], length + 1);
    return 0;
}

static int fdpath_snapshot_locked(uint64_t key, uint64_t owner_start_ns, char *path, uint8_t *path_is_guest) {
    struct fdpath_slot *slot = fdpath_find(key, owner_start_ns, 0);
    if (!slot) {
        path[0] = 0;
        *path_is_guest = 0;
        return 0;
    }
    memcpy(path, slot->path, sizeof slot->path);
    *path_is_guest = slot->path_is_guest;
    return 1;
}

static int fdpath_restore_locked(uint64_t key, uint64_t owner_start_ns, const char *path, uint8_t path_is_guest) {
    if (!path[0]) return 0;
    struct fdpath_slot *slot = fdpath_find(key, owner_start_ns, 1);
    if (!slot) {
        fdpath_sweep_stale_locked();
        slot = fdpath_find(key, owner_start_ns, 1);
    }
    if (!slot) return -ENOSPC;
    slot->path_is_guest = path_is_guest;
    memcpy(slot->path, path, sizeof slot->path);
    return 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
#if !defined(_WIN32)
static int fdvis_corpse_sweep_test(struct fdvis_slot *slots) {
    const struct timespec tick = {.tv_sec = 0, .tv_nsec = 1000000};
    pid_t corpse = fork();
    if (corpse == 0) {
        for (;;) {
            struct timespec forever = {.tv_sec = 3600, .tv_nsec = 0};
            (void)nanosleep(&forever, NULL);
        }
    }
    int verdict = 0;
    if (corpse > 0) {
        uint64_t start = fdvis_process_token((int)corpse);
        (void)kill(corpse, SIGKILL);
        int is_zombie = 0;
        for (int spin = 0; spin < 5000 && !is_zombie; ++spin) {
            hl_host_process_info record;
            if (hl_host_process_read((int64_t)corpse, &record) && record.state == 'Z')
                is_zombie = 1;
            else
                (void)nanosleep(&tick, NULL);
        }
        if (is_zombie && start != 0) {
            slots[0].key = fdvis_key((int)corpse, 3);
            slots[0].owner_start_ns = start;
            slots[0].reserver_pid = 0;
            fdvis_sweep_stale_locked();
            verdict = slots[0].key == 0;
        }
        int status = 0;
        while (waitpid(corpse, &status, 0) < 0 && errno == EINTR) {}
    }
    return verdict;
}
#else
/* This scenario needs a real zombie: a process whose identity record remains readable after it can no
   longer run. The Windows host reader has no such state and the target has no fork/waitpid staging.
   Refuse with 0, as scenarios 7 and fdvis_corpse_holder_test do, rather than reporting an arm that did
   not execute as passing. */
static int fdvis_corpse_sweep_test(struct fdvis_slot *slots) {
    (void)slots;
    return 0;
}
#endif

/* Scenarios 8-10: can an abandoned RESERVATION be reclaimed?
 *
 * A reservation overwrites the key with UINT64_MAX, so the owner both sweeps decode from the high 32
 * bits is (int)0xFFFFFFFF == -1 and their `owner <= 0` guard skipped the slot on every pass. Nothing in
 * the tree could reclaim one, so a process that died between proc_fdvis_reserve() and its publish or
 * cancel leaked the slot for the lifetime of the shared table. These run on private tables and fabricate
 * the state directly -- no fork -- because the property under test is the sweep's decision, not a race.
 *
 * 10 is the one that keeps this change from breaking the fork path: reclamation is opt-in on
 * reserver_pid being set, so a reservation from before this commit, or any slot whose holder is not
 * recorded, is still left strictly alone. */
static int fdvis_reservation_sweep_test(uint32_t scenario) {
    struct fdvis_slot *slots = calloc(FDVIS_N, sizeof *slots);
    struct fdpath_slot *paths = calloc(FDPATH_N, sizeof *paths);
    if (!slots || !paths) {
        free(slots);
        free(paths);
        return 0;
    }
    struct fdvis_slot *saved_slots = g_fdvis;
    struct fdpath_slot *saved_paths = g_fdpaths;
    g_fdvis = slots;
    g_fdpaths = paths;
    slots[0].key = UINT64_MAX;
    int verdict = 0;
    if (scenario == 8) {
        /* A pid the kernel cannot be holding: hl_host_process_read() fails, so the owner is gone. */
        slots[0].reserver_pid = 2147483647;
        slots[0].owner_start_ns = 1;
        fdvis_sweep_stale_locked();
        verdict = slots[0].key == 0;
    } else if (scenario == 9) {
        int pid = 0;
        uint64_t owner_start = 0;
        (void)fdvis_self(&pid, &owner_start);
        slots[0].reserver_pid = pid;
        slots[0].owner_start_ns = owner_start;
        fdvis_sweep_stale_locked();
        verdict = slots[0].key == UINT64_MAX; /* we are alive: our own reservation must survive */
    } else if (scenario == 10) {
        slots[0].reserver_pid = 0; /* holder not recorded -- e.g. an in-flight fork plan */
        slots[0].owner_start_ns = 1;
        fdvis_sweep_stale_locked();
        verdict = slots[0].key == UINT64_MAX;
    } else {
        /* Scenario 11: a CORPSE owner's published slot must be swept.
         *
         * This is the case the token comparison could not see and the case no other scenario reaches:
         * 8 uses a pid the kernel has never issued and 9 uses this live process, so both are answered
         * identically by "does the start time still match" and by "can this owner still run". Only a
         * zombie separates them -- Linux keeps /proc/<pid>/stat, start time and all, until the parent
         * waits -- so the fixture has to make a real one. Deliberately NOT reaped until after the
         * sweep has run: reaping first is what would make the old predicate pass too. */
        verdict = fdvis_corpse_sweep_test(slots);
    }
    g_fdvis = saved_slots;
    g_fdpaths = saved_paths;
    free(slots);
    free(paths);
    return verdict;
}

HL_API int HL_TARGET_LOCAL(fdvis_path_publication_test)(uint32_t scenario) {
    if (scenario == 12) return fdvis_recursive_identity_test();
    if (scenario >= 8 && scenario <= 11) return fdvis_reservation_sweep_test(scenario);
    struct fdpath_slot *paths = calloc(FDPATH_N, sizeof *paths);
    struct fdpath_slot *saved_paths = g_fdpaths;
    const int descriptor = HL_NFD - 1;
    char saved_path[sizeof g_fdpath[descriptor]];
    uint8_t saved_is_guest = g_fdpath_guest[descriptor];
    memcpy(saved_path, g_fdpath[descriptor], sizeof saved_path);
    if (scenario == 2)
        memset(g_fdpath[descriptor], 'x', sizeof g_fdpath[descriptor]);
    else
        snprintf(g_fdpath[descriptor], sizeof g_fdpath[descriptor],
                 scenario == 1 ? "/fork/inherited" : "/checkpoint/restored");
    g_fdpath_guest[descriptor] = 1;
    if (!paths) return 0;
    g_fdpaths = paths;
    if (scenario == 3) {
        uint64_t first = 1, second_key = 1 + FDPATH_N;
        struct fdpath_slot *a = fdpath_find(first, 9, 1);
        struct fdpath_slot *b = fdpath_find(second_key, 9, 1);
        int collision_ok = 0;
        if (a && b) {
            snprintf(b->path, sizeof b->path, "second");
            fdpath_delete_locked(a);
            struct fdpath_slot *observed = fdpath_find(second_key, 9, 0);
            struct fdpath_slot *republished = fdpath_find(first, 9, 1);
            collision_ok = observed == b && strcmp(observed->path, "second") == 0 && republished == a &&
                           fdpath_find(first, 9, 0) == a;
        }
        g_fdpaths = saved_paths;
        free(paths);
        return collision_ok;
    }
    if (scenario == 4) {
        for (unsigned index = 0; index < FDPATH_N; ++index)
            paths[index].key = (uint64_t)index + 1;
        int full = fdpath_find(UINT64_C(0x100000001), 9, 1) == NULL;
        g_fdpaths = saved_paths;
        free(paths);
        return full;
    }
    if (scenario == 5) {
        uint64_t key = fdvis_key(77, descriptor);
        struct fdpath_slot *owned = fdpath_find(key, 99, 1);
        fdpath_cleanup_owner_locked(77, 99);
        int cleaned = owned && fdpath_find(key, 99, 0) == NULL;
        g_fdpaths = saved_paths;
        free(paths);
        return cleaned;
    }
    if (scenario == 6) {
        for (unsigned index = 0; index < FDPATH_N; ++index)
            paths[index].key = (uint64_t)index + 1;
        int propagated = fdpath_restore_locked(fdvis_key(88, descriptor), 100, "/fork/full", 1);
        g_fdpaths = saved_paths;
        free(paths);
        return propagated == -ENOSPC;
    }
    if (scenario == 8) {
        g_fdpaths = saved_paths;
        free(paths);
        memcpy(g_fdpath[descriptor], saved_path, sizeof saved_path);
        g_fdpath_guest[descriptor] = saved_is_guest;
        return fdvis_corpse_holder_test();
    }
    if (scenario == 7) {
        const int dead = 2147483647;
        for (unsigned index = 0; index < FDPATH_N; ++index) {
            paths[index].key = fdvis_key(dead, (int)index);
            paths[index].owner_start_ns = 1;
        }
        int reclaimed = proc_fdvis_publish_path_locked(7, 9, descriptor) == 0 &&
                        fdpath_find(fdvis_key(7, descriptor), 9, 0) != NULL;
        g_fdpaths = saved_paths;
        free(paths);
        return reclaimed && fdvis_after_fork_rollback_test() && fdvis_stalled_parent_test();
    }
    int first = proc_fdvis_publish_path_locked(7, 9, descriptor);
    struct fdpath_slot *slot = fdpath_find(fdvis_key(7, descriptor), 9, 0);
    const char *expected = scenario == 1 ? "/fork/inherited" : "/checkpoint/restored";
    int populated = scenario == 2 ? first == -ENAMETOOLONG && slot == NULL
                                  : first == 0 && slot && slot->path_is_guest == 1 && strcmp(slot->path, expected) == 0;
    if (scenario == 1 && populated) {
        char inherited[sizeof g_fdpath[0]];
        uint8_t inherited_is_guest;
        uint64_t parent_key = fdvis_key(7, descriptor), child_key = fdvis_key(8, descriptor);
        populated = fdpath_snapshot_locked(parent_key, 9, inherited, &inherited_is_guest) == 1 &&
                    fdpath_restore_locked(child_key, 10, inherited, inherited_is_guest) == 0;
        struct fdpath_slot *child = fdpath_find(child_key, 10, 0);
        populated = populated && child && child->path_is_guest == 1 && strcmp(child->path, expected) == 0;
    }
    g_fdpath[descriptor][0] = 0;
    g_fdpath_guest[descriptor] = 0;
    int second = proc_fdvis_publish_path_locked(7, 9, descriptor);
    int cleared = second == 0 && fdpath_find(fdvis_key(7, descriptor), 9, 0) == NULL;
    memcpy(g_fdpath[descriptor], saved_path, sizeof saved_path);
    g_fdpath_guest[descriptor] = saved_is_guest;
    g_fdpaths = saved_paths;
    free(paths);
    return populated && (scenario == 2 || cleared);
}
#endif
static uint64_t g_fdvis_fork_parent_start;
static uint64_t g_pipe_identity[HL_NFD];

static void pipe_identity_bind(int fd, uint64_t identity) {
    if (fd < 0 || fd >= HL_NFD) return;
    g_pipe_identity[fd] = identity;
    if (identity != 0) virtual_fd_bound_include(fd);
}

// Guest-visible F_SETPIPE_SZ/F_GETPIPE_SZ state is also needed by early SCM_RIGHTS marshalling.
static int g_pipesz[HL_NFD];
static uint8_t g_fdvis_private[HL_NFD];
static _Atomic uint64_t g_pipe_identity_next = 1;
static void proc_fdvis_cleanup(void);
static void proc_fdvis_close(int guest_fd);
static int proc_fdvis_publish_native_fd(int guest_fd);

struct fdvis_fork_entry {
    unsigned slot;
    int guest_fd;
    uint32_t kind;
    uint64_t device;
    uint64_t object;
    uint8_t path_is_guest;
    char path[sizeof g_fdpath[0]];
};

struct fdvis_fork_plan {
    struct fdvis_fork_entry *entries;
    size_t count;
};

static uint64_t fdvis_key(int pid, int fd) {
    return pid > 0 && fd >= 0 ? ((uint64_t)(uint32_t)pid << 32) | ((uint32_t)fd + 1u) : 0;
}

static uint64_t fdvis_process_token(int pid) {
    uint64_t start_time_ns = 0;
    return hl_host_process_start_time_ns(pid, &start_time_ns) ? start_time_ns : 0;
}

/* This process's own (pid, start-time token) pair. Every fdvis operation on a descriptor this process
 * owns needs both, and resolving them as getpid() + fdvis_process_token(getpid()) cost two host calls
 * per lock acquisition and per publish/close -- 13 of the 68 getpid() the engine issued per guest
 * open(). hl_host_process_self_identity() serves both from a memo retired by a fork epoch, so the pair
 * is still this process's own after any fork. PEER pids keep going through fdvis_process_token(), which
 * never memoizes: a remembered start time on a recycled peer pid is precisely the stale ownership the
 * owner_start_ns stamp exists to reject. */
static int fdvis_self(int *pid, uint64_t *token) {
    int64_t self = 0;
    uint64_t start = 0;
    if (!hl_host_process_self_identity(&self, &start)) {
        *pid = (int)getpid();
        *token = fdvis_process_token(*pid);
        return *token != 0;
    }
    *pid = (int)self;
    *token = start;
    return 1;
}

static uint64_t fdvis_identity(int pid, uint64_t start_ns) {
    uint32_t fingerprint = (uint32_t)start_ns ^ (uint32_t)(start_ns >> 32);
    return ((uint64_t)(uint32_t)pid << 32) | fingerprint;
}

/* Can the process named in the fdvis lock word still reach fdvis_unlock()?
 *
 * The word carries (pid, start-time), so a pid the kernel has reissued to somebody else is already
 * rejected by the stamp. What the stamp cannot see is a corpse. A terminated task keeps its /proc entry
 * -- start time included -- until its parent collects it, so an owner killed inside the critical section
 * goes on answering with exactly the token it published, and the reclaim below refuses to steal from it.
 * Nothing releases the word either, because the owner has already run its last instruction.
 *
 * That is not a race window that closes on its own. In the ordinary double-fork shape -- a middle process
 * that kills its child and exits without waiting -- the only task that could collect the corpse is the
 * middle process, and the middle process is the one now spinning here on its way out through
 * proc_fdvis_cleanup(). The wait is unbounded and burns a core for as long as it lasts.
 *
 * A task in state 'Z' is dead: Linux reports it from /proc/<pid>/stat and Darwin from SZOMB, and the
 * Windows arm has no process reader at all and answers "gone" for every pid. Treat all three as gone. */
static int fdvis_owner_is_gone(int pid, uint64_t owner) {
    hl_host_process_info record;
    if (pid <= 0 || !hl_host_process_read((int64_t)pid, &record)) return 1;
    if (fdvis_identity(pid, record.start_time_ns) != owner) return 1;
    return record.state == 'Z';
}

static void fdvis_init(const hl_host_services *host) {
    void *arena = NULL;
    if (g_fdvis != NULL) return;
    size_t bytes =
        sizeof(struct fdvis_slot) * FDVIS_N + sizeof(struct fdpath_slot) * FDPATH_N + sizeof(*g_fdvis_control);
    if (hl_linux_shared_create(host, bytes, &arena) != HL_STATUS_OK) return;
    g_fdvis = arena;
    g_fdpaths = (void *)((unsigned char *)arena + sizeof(struct fdvis_slot) * FDVIS_N);
    g_fdvis_control = (void *)((unsigned char *)g_fdpaths + sizeof(struct fdpath_slot) * FDPATH_N);
    (void)atexit(proc_fdvis_cleanup);
    // Enumerate this process's open descriptors ONCE and publish the non-engine-private ones. Each
    // hl_host_process_fds() call is a full /proc/self/fd getdents scan whose kernel cost is O(highest open
    // fd); the engine keeps its internal descriptors at the high private floor (65536+), so a scan is ~1.2ms.
    // The prior count-then-fill idiom paid that scan TWICE. A generous on-stack buffer captures every real
    // descriptor in a single scan (an engine launch has a handful of inherited fds, far below the floor);
    // only a pathological overflow falls back to the exact two-pass path, so behavior is unchanged.
    hl_host_process_fd inline_entries[128];
    size_t count = 0;
    if (hl_host_process_fds(getpid(), inline_entries, sizeof inline_entries / sizeof *inline_entries, &count)) {
        hl_host_process_fd *entries = inline_entries;
        hl_host_process_fd *heap = NULL;
        if (count > sizeof inline_entries / sizeof *inline_entries) {
            // Rare: more open descriptors than the inline buffer. Re-scan into an exact heap buffer.
            heap = calloc(count, sizeof(*heap));
            if (heap && hl_host_process_fds(getpid(), heap, count, &count))
                entries = heap;
            else
                count = sizeof inline_entries / sizeof *inline_entries; // publish what the inline scan captured
        }
        for (size_t index = 0; index < count; ++index)
            if ((entries[index].flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) == 0)
                (void)proc_fdvis_publish_native_fd(entries[index].descriptor);
        free(heap);
    }
}

static struct fdvis_slot *fdvis_find(uint64_t key, uint64_t owner_start_ns, int claim) {
    if (!g_fdvis || key == 0) return NULL;
    unsigned start = (unsigned)((key ^ (key >> 32)) * UINT64_C(2654435761)) & (FDVIS_N - 1);
    for (unsigned probe = 0; probe < FDVIS_N; ++probe) {
        struct fdvis_slot *slot = &g_fdvis[(start + probe) & (FDVIS_N - 1)];
        uint64_t present = slot->key;
        if (present == key) {
            if (slot->owner_start_ns == owner_start_ns) return slot;
            if (!claim) return NULL;
            memset(slot, 0, sizeof *slot);
            slot->key = key;
            slot->owner_start_ns = owner_start_ns;
            return slot;
        }
        if (claim && present == 0) {
            slot->key = key;
            slot->owner_start_ns = owner_start_ns;
            return slot;
        }
    }
    return NULL;
}

static void fdvis_lock(void) {
    int me = 0;
    uint64_t me_token = 0;
    (void)fdvis_self(&me, &me_token);
    uint64_t mine = fdvis_identity(me, me_token);
    for (unsigned spin = 0;; ++spin) {
        uint64_t expected = 0;
        if (atomic_compare_exchange_weak_explicit(&g_fdvis_control->owner, &expected, mine, memory_order_acquire,
                                                  memory_order_relaxed))
            return;
        if ((spin & 1023u) == 1023u) {
            uint64_t owner = atomic_load_explicit(&g_fdvis_control->owner, memory_order_relaxed);
            if (owner != 0 && fdvis_owner_is_gone((int)(uint32_t)(owner >> 32), owner) &&
                atomic_compare_exchange_strong_explicit(&g_fdvis_control->owner, &owner, mine, memory_order_acquire,
                                                        memory_order_relaxed))
                return;
            sched_yield();
        }
    }
}

static void fdvis_unlock(void) {
    atomic_store_explicit(&g_fdvis_control->owner, 0, memory_order_release);
}

/* A slot's owner is gone if it cannot be read, if the pid has been reissued to somebody else, or if the
   task is a corpse. The third case is the one this used to miss: comparing start times alone answers
   "does this owner still exist?", and a zombie exists -- Linux keeps /proc/<pid>/stat, start time
   included, until its parent waits -- so a killed owner's rows survived every sweep until something
   reaped it, which in the double-fork shape is nobody. fdvis_owner_is_gone() asks "can this owner still
   run?" instead, which is the predicate the lock word was just moved onto for the same reason.

   The per-owner memo matters more now, not less: the answer costs a /proc/<pid>/stat read rather than a
   start-time lookup, and these tables are swept whole. Slots for one owner are not adjacent, so the memo
   is keyed on the last owner asked about rather than a run. */
static void fdpath_sweep_stale_locked(void) {
    int memo_owner = 0;
    uint64_t memo_start = 0;
    int memo_gone = 0;
    for (unsigned index = 0; index < FDPATH_N; ++index) {
        struct fdpath_slot *slot = &g_fdpaths[index];
        /* UINT64_MAX is a TOMBSTONE in this table, not a reservation -- fdpath_delete_locked() writes it
           so fdpath_find()'s early exit can keep walking past a deleted entry. The fdvis table spells a
           reservation with the same value; they are different meanings and the two must not be merged. */
        int owner = slot->key == UINT64_MAX ? 0 : (int)(uint32_t)(slot->key >> 32);
        if (owner <= 0) continue;
        int gone;
        if (owner == memo_owner && slot->owner_start_ns == memo_start) {
            gone = memo_gone;
        } else {
            gone = fdvis_owner_is_gone(owner, fdvis_identity(owner, slot->owner_start_ns));
            memo_owner = owner;
            memo_start = slot->owner_start_ns;
            memo_gone = gone;
        }
        if (gone) fdpath_delete_locked(slot);
    }
}

static void fdvis_sweep_stale_locked(void) {
    int memo_owner = 0;
    uint64_t memo_start = 0;
    int memo_gone = 0;
    for (unsigned index = 0; index < FDVIS_N; ++index) {
        struct fdvis_slot *slot = &g_fdvis[index];
        int owner;
        if (slot->key == UINT64_MAX) {
            /* A reservation. Until reserver_pid existed nothing could reclaim one: the marker replaces
               the key, so the owner decoded from the high 32 bits is (int)0xFFFFFFFF == -1 and the
               `owner <= 0` guard below skipped it on every pass. A process that died between
               proc_fdvis_reserve() and its publish or cancel therefore leaked the slot permanently.
               Reclaim it here, on exactly the same "can the owner still run?" test as a published slot.
               reserver_pid is 0 for a reservation taken by proc_fdvis_fork_prepare() before this commit
               and for any slot that has never been reserved, and a 0 pid is left alone. */
            owner = slot->reserver_pid;
            if (owner <= 0) continue;
        } else {
            owner = (int)(uint32_t)(slot->key >> 32);
            if (owner <= 0) continue;
        }
        int gone;
        if (owner == memo_owner && slot->owner_start_ns == memo_start) {
            gone = memo_gone;
        } else {
            gone = fdvis_owner_is_gone(owner, fdvis_identity(owner, slot->owner_start_ns));
            memo_owner = owner;
            memo_start = slot->owner_start_ns;
            memo_gone = gone;
        }
        if (gone) memset(slot, 0, sizeof *slot);
    }
    fdpath_sweep_stale_locked();
}

struct fdvis_reservation {
    unsigned slot;
    int active;
    int new_slot;
};

static int proc_fdvis_reserve(struct fdvis_reservation *reservation) {
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
    if (!reservation) return -EINVAL;
    memset(reservation, 0, sizeof *reservation);
    if (!g_fdvis || !g_fdvis_control) return -ENOSPC;
    fdvis_lock();
    for (unsigned pass = 0; pass < 2; ++pass) {
        for (unsigned index = 0; index < FDVIS_N; ++index) {
            if (g_fdvis[index].key != 0) continue;
            g_fdvis[index].key = UINT64_MAX;
            /* Name the holder so an abandoned reservation is reclaimable; see fdvis_sweep_stale_locked. */
            g_fdvis[index].reserver_pid = pid;
            g_fdvis[index].owner_start_ns = owner_start;
            reservation->slot = index;
            reservation->active = 1;
            reservation->new_slot = 1;
            fdvis_unlock();
            return 0;
        }
        fdvis_sweep_stale_locked();
    }
    fdvis_unlock();
    return -ENOSPC;
}

static int proc_fdvis_reserve_at(int guest_fd, struct fdvis_reservation *reservation) {
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
    if (!reservation) return -EINVAL;
    memset(reservation, 0, sizeof *reservation);
    if (!g_fdvis || !g_fdvis_control) return -ENOSPC;
    fdvis_lock();
    struct fdvis_slot *present = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (present) {
        reservation->slot = (unsigned)(present - g_fdvis);
        reservation->active = 1;
        fdvis_unlock();
        return 0;
    }
    for (unsigned pass = 0; pass < 2; ++pass) {
        for (unsigned index = 0; index < FDVIS_N; ++index) {
            if (g_fdvis[index].key != 0) continue;
            g_fdvis[index].key = UINT64_MAX;
            /* Name the holder so an abandoned reservation is reclaimable; see fdvis_sweep_stale_locked. */
            g_fdvis[index].reserver_pid = pid;
            g_fdvis[index].owner_start_ns = owner_start;
            reservation->slot = index;
            reservation->active = 1;
            reservation->new_slot = 1;
            fdvis_unlock();
            return 0;
        }
        fdvis_sweep_stale_locked();
    }
    fdvis_unlock();
    return -ENOSPC;
}

static void proc_fdvis_reservation_cancel(struct fdvis_reservation *reservation) {
    if (!reservation || !reservation->active) return;
    fdvis_lock();
    struct fdvis_slot *slot = &g_fdvis[reservation->slot];
    if (reservation->new_slot && slot->key == UINT64_MAX) memset(slot, 0, sizeof *slot);
    fdvis_unlock();
    reservation->active = 0;
}

static void proc_fdvis_reservation_publish(struct fdvis_reservation *reservation, int guest_fd, uint32_t kind,
                                           uint64_t device, uint64_t object) {
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
    fdvis_lock();
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    struct fdvis_slot *reserved = &g_fdvis[reservation->slot];
    if (slot) {
        if (reserved != slot && reserved->key == UINT64_MAX) memset(reserved, 0, sizeof *reserved);
    } else {
        slot = reserved;
    }
    slot->device = device;
    slot->object = object;
    slot->kind = kind;
    if (guest_fd >= 0 && guest_fd < HL_NFD) { (void)proc_fdvis_publish_path_locked(pid, owner_start, guest_fd); }
    slot->owner_start_ns = owner_start;
    slot->generation = ++g_fdvis_control->generation;
    slot->key = fdvis_key(pid, guest_fd);
    fdvis_unlock();
    reservation->active = 0;
}

static int proc_fdvis_publish(int guest_fd, uint32_t kind, uint64_t device, uint64_t object) {
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
    if (guest_fd < 0 || guest_fd >= HL_NFD) return -EBADF;
    if (!g_fdvis_control) return -ENOSPC;
    fdvis_lock();
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 1);
    if (!slot) {
        fdvis_sweep_stale_locked();
        slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 1);
    }
    if (!slot) {
        fdvis_unlock();
        return -ENOSPC;
    }
    uint64_t generation = ++g_fdvis_control->generation;
    slot->device = device;
    slot->object = object;
    slot->kind = kind;
    int path_status = proc_fdvis_publish_path_locked(pid, owner_start, guest_fd);
    slot->generation = generation;
    fdvis_unlock();
    return path_status;
}

static void proc_fdvis_publish_path(int guest_fd) {
    int pid = 0;
    uint64_t owner_start = 0;
    struct fdvis_slot *slot;
    if (guest_fd < 0 || guest_fd >= HL_NFD || !g_fdvis_control) return;
    (void)fdvis_self(&pid, &owner_start);
    fdvis_lock();
    slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (slot) {
        (void)proc_fdvis_publish_path_locked(pid, owner_start, guest_fd);
        slot->generation = ++g_fdvis_control->generation;
    }
    fdvis_unlock();
}

static int fdvis_native_detail(int guest_fd, hl_host_process_fd *detail, int force_fallback) {
    size_t ignored = 0;
    if (guest_fd < 0) return -EBADF;
    if (!force_fallback && hl_host_process_fd_read(getpid(), guest_fd, detail, NULL, 0, &ignored)) return 0;
    struct stat status;
    if (fstat(guest_fd, &status) != 0) return -EBADF;
    detail->kind = S_ISREG(status.st_mode) || S_ISDIR(status.st_mode) || S_ISLNK(status.st_mode) ||
                           S_ISCHR(status.st_mode) || S_ISBLK(status.st_mode)
                       ? HL_HOST_FD_FILE
                   : S_ISFIFO(status.st_mode) ? HL_HOST_FD_PIPE
                   : S_ISSOCK(status.st_mode) ? HL_HOST_FD_SOCKET
                                              : HL_HOST_FD_OTHER;
    detail->stable_device = (uint64_t)status.st_dev;
    detail->stable_object = (uint64_t)status.st_ino;
    return 0;
}

static int proc_fdvis_publish_native_fd(int guest_fd) {
    hl_host_process_fd detail;
    /* Under recursive execution an ambient descriptor can be a guest descriptor of the outer engine,
     * not an entry in this process's native fd table.  fstat follows that descriptor through the normal
     * syscall route and therefore remains authoritative at every nesting depth. */
    if (fdvis_native_detail(guest_fd, &detail, 0) != 0) return -EBADF;
    return proc_fdvis_publish(guest_fd, detail.kind, detail.stable_device, detail.stable_object);
}

static int fdvis_recursive_identity_test(void) {
    FILE *first = tmpfile();
    FILE *second = tmpfile();
    if (first == NULL || second == NULL) {
        if (first) fclose(first);
        if (second) fclose(second);
        return 0;
    }
    int descriptor = fileno(first);
    struct stat before, expected;
    int verdict = fstat(descriptor, &before) == 0 && fstat(fileno(second), &expected) == 0 &&
                  dup2(fileno(second), descriptor) == descriptor;
    /* Force the same fallback operation recursive execution uses, after the number changed owners. */
    hl_host_process_fd observed = {0};
    verdict = verdict && fdvis_native_detail(descriptor, &observed, 1) == 0 &&
              observed.stable_device == (uint64_t)expected.st_dev &&
              observed.stable_object == (uint64_t)expected.st_ino &&
              (observed.stable_device != (uint64_t)before.st_dev || observed.stable_object != (uint64_t)before.st_ino);
    fclose(first);
    fclose(second);
    return verdict;
}

static int proc_fdvis_publish_pipe_pair(int first, int second) {
    uint64_t sequence = atomic_fetch_add_explicit(&g_pipe_identity_next, 1, memory_order_relaxed);
    int self_pid = 0;
    uint64_t self_token = 0;
    (void)fdvis_self(&self_pid, &self_token);
    uint64_t identity = fdvis_identity(self_pid, self_token) ^ sequence;
    if (identity == 0) identity = sequence ? sequence : 1;
    if (first < 0 || first >= HL_NFD || second < 0 || second >= HL_NFD) return -EINVAL;
    if (proc_fdvis_publish(first, HL_HOST_FD_PIPE, 1, identity) != 0) return -ENOSPC;
    if (proc_fdvis_publish(second, HL_HOST_FD_PIPE, 1, identity) != 0) {
        proc_fdvis_close(first);
        return -ENOSPC;
    }
    pipe_identity_bind(first, identity);
    pipe_identity_bind(second, identity);
    return 0;
}

static void proc_fdvis_close(int guest_fd) {
    if (!g_fdvis_control) return;
    fdvis_lock();
    int pid = 0;
    uint64_t owner_start = 0;
    (void)fdvis_self(&pid, &owner_start);
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (slot) memset(slot, 0, sizeof *slot);
    struct fdpath_slot *path = fdpath_find(fdvis_key(pid, guest_fd), owner_start, 0);
    if (path) fdpath_delete_locked(path);
    fdvis_unlock();
}

static int proc_fdvis_lookup(int pid, int guest_fd, uint32_t *kind, uint64_t *device, uint64_t *object) {
    if (!g_fdvis_control) return 0;
    fdvis_lock();
    struct fdvis_slot *slot = fdvis_find(fdvis_key(pid, guest_fd), fdvis_process_token(pid), 0);
    if (slot) {
        if (kind) *kind = slot->kind;
        if (device) *device = slot->device;
        if (object) *object = slot->object;
    }
    fdvis_unlock();
    return slot != NULL;
}

static int proc_fdvis_lookup_path(int pid, int guest_fd, char *path, size_t capacity, int *path_is_guest) {
    struct fdpath_slot *slot;
    int found = 0;
    if (!g_fdvis_control || path == NULL || capacity == 0) return 0;
    fdvis_lock();
    slot = fdpath_find(fdvis_key(pid, guest_fd), fdvis_process_token(pid), 0);
    if (slot && slot->path[0] != '\0') {
        size_t length = strnlen(slot->path, sizeof slot->path);
        if (length < sizeof slot->path && length < capacity) {
            memcpy(path, slot->path, length + 1);
            if (path_is_guest) *path_is_guest = slot->path_is_guest != 0;
            found = 1;
        }
    }
    fdvis_unlock();
    return found;
}

struct fdvis_view {
    int guest_fd;
    uint32_t kind;
    uint64_t device;
    uint64_t object;
};

static size_t proc_fdvis_list(int pid, struct fdvis_view *views, size_t capacity) {
    uint64_t owner_start = fdvis_process_token(pid);
    size_t count = 0;
    if (!g_fdvis || !g_fdvis_control || owner_start == 0) return 0;
    fdvis_lock();
    for (unsigned index = 0; index < FDVIS_N; ++index) {
        struct fdvis_slot *slot = &g_fdvis[index];
        if ((int)(uint32_t)(slot->key >> 32) != pid || slot->owner_start_ns != owner_start) continue;
        int guest_fd = (int)(uint32_t)slot->key - 1;
        if (guest_fd < 0 || guest_fd >= HL_NFD) continue;
        if (count < capacity) {
            views[count].guest_fd = guest_fd;
            views[count].kind = slot->kind;
            views[count].device = slot->device;
            views[count].object = slot->object;
        }
        ++count;
    }
    fdvis_unlock();
    return count;
}

static int proc_fdvis_fork_prepare(struct fdvis_fork_plan *plan) {
    size_t count = 0;
    size_t capacity = 0;
    size_t reserved = 0;
    struct fdvis_fork_entry *entries = NULL;
    g_fdvis_fork_parent = (int)getpid();
    g_fdvis_fork_parent_start = fdvis_process_token(g_fdvis_fork_parent);
    memset(plan, 0, sizeof *plan);
    if (!g_fdvis || !g_fdvis_control) return -ENOSPC;

    fdvis_lock();
    /* One pass fuses the stale sweep with the parent-descriptor collect. A parent-owned live slot is
     * never stale (its owner is us and owner_start_ns matches), so the "collect" and "sweep" categories
     * are disjoint: the result is byte-identical to sweeping the whole table first and then counting +
     * copying the parent's slots. Collecting the fd identity here also folds the old separate fill pass
     * away, since the reserve pass below never touches occupied parent slots. */
    for (unsigned index = 0; index < FDVIS_N; ++index) {
        struct fdvis_slot *slot = &g_fdvis[index];
        int owner = (int)(uint32_t)(slot->key >> 32);
        if (owner <= 0) continue;
        if (owner == g_fdvis_fork_parent && slot->owner_start_ns == g_fdvis_fork_parent_start) {
            if (count == capacity) {
                size_t next = capacity ? capacity * 2 : 16;
                struct fdvis_fork_entry *grown = realloc(entries, next * sizeof *grown);
                if (grown == NULL) {
                    fdvis_unlock();
                    free(entries);
                    return -ENOMEM;
                }
                entries = grown;
                capacity = next;
            }
            entries[count].guest_fd = (int)(uint32_t)slot->key - 1;
            entries[count].kind = slot->kind;
            entries[count].device = slot->device;
            entries[count].object = slot->object;
            (void)fdpath_snapshot_locked(slot->key, slot->owner_start_ns, entries[count].path,
                                         &entries[count].path_is_guest);
            ++count;
            continue;
        }
        /* Corpse-aware, for the reason on fdpath_sweep_stale_locked: a zombie owner still answers with
           the start time it published, so comparing tokens retained its slots until somebody reaped it. */
        if (fdvis_owner_is_gone(owner, fdvis_identity(owner, slot->owner_start_ns))) memset(slot, 0, sizeof *slot);
    }
    fdpath_sweep_stale_locked();
    for (unsigned index = 0; index < FDVIS_N && reserved < count; ++index) {
        if (g_fdvis[index].key != 0) continue;
        g_fdvis[index].key = UINT64_MAX;
        /* These reservations leak exactly as the single-slot ones did if this process dies before
           proc_fdvis_fork_cancel() or the child's publish. Name the holder so the sweep can reclaim them. */
        g_fdvis[index].reserver_pid = g_fdvis_fork_parent;
        g_fdvis[index].owner_start_ns = g_fdvis_fork_parent_start;
        entries[reserved++].slot = index;
    }
    if (reserved != count) {
        for (size_t index = 0; index < reserved; ++index)
            memset(&g_fdvis[entries[index].slot], 0, sizeof *g_fdvis);
        fdvis_unlock();
        free(entries);
        return -ENOSPC;
    }
    fdvis_unlock();
    plan->entries = entries;
    plan->count = count;
    return 0;
}

static void proc_fdvis_fork_cancel(struct fdvis_fork_plan *plan) {
    if (!plan->entries) return;
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        struct fdvis_slot *slot = &g_fdvis[plan->entries[index].slot];
        if (slot->key == UINT64_MAX) memset(slot, 0, sizeof *slot);
    }
    fdvis_unlock();
}

static void proc_fdvis_fork_child_abort(struct fdvis_fork_plan *plan, int child) {
    uint64_t child_start = fdvis_process_token(child);
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *slot = &g_fdvis[entry->slot];
        uint64_t key = fdvis_key(child, entry->guest_fd);
        if (slot->key == UINT64_MAX) {
            memset(slot, 0, sizeof *slot);
            continue;
        }
        if (slot->key != key || (slot->owner_start_ns != 0 && slot->owner_start_ns != child_start)) continue;
        struct fdpath_slot *path = fdpath_find(key, slot->owner_start_ns, 0);
        if (path) fdpath_delete_locked(path);
        memset(slot, 0, sizeof *slot);
    }
    fdvis_unlock();
}

static int proc_fdvis_fork_child_timeout(struct fdvis_fork_plan *plan, int child) {
    fdvis_lock();
    int published = 1;
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        if (g_fdvis[entry->slot].key != fdvis_key(child, entry->guest_fd)) {
            published = 0;
            break;
        }
    }
    if (published) {
        fdvis_unlock();
        return 1;
    }
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *slot = &g_fdvis[entry->slot];
        uint64_t key = fdvis_key(child, entry->guest_fd);
        if (slot->key != UINT64_MAX) continue;
        memset(slot, 0, sizeof *slot);
        slot->key = key;
        slot->owner_start_ns = UINT64_MAX;
        slot->generation = ++g_fdvis_control->generation;
    }
    fdvis_unlock();
    return 0;
}

static void proc_fdvis_fork_parent_clear_timeout(struct fdvis_fork_plan *plan, int child) {
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        const struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *slot = &g_fdvis[entry->slot];
        if (slot->key == fdvis_key(child, entry->guest_fd) && slot->owner_start_ns == UINT64_MAX)
            memset(slot, 0, sizeof *slot);
    }
    fdvis_unlock();
}

#if defined(HL_NATIVE_TEST_HOOKS)
static uint64_t g_fdvis_fork_wait_milliseconds = UINT64_C(5000);
#endif

static uint64_t fdvis_fork_wait_milliseconds(void) {
#if defined(HL_NATIVE_TEST_HOOKS)
    return g_fdvis_fork_wait_milliseconds;
#else
    return UINT64_C(5000);
#endif
}

static uint64_t fdvis_monotonic_milliseconds(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return 0;
    return (uint64_t)now.tv_sec * UINT64_C(1000) + (uint64_t)now.tv_nsec / UINT64_C(1000000);
}

struct fdvis_fork_journal {
    struct fdvis_slot *identity;
    struct fdvis_slot previous_identity;
    uint64_t key;
    uint64_t owner_start_ns;
    uint64_t generation;
    struct fdpath_slot *path;
    struct fdpath_slot previous_path;
    struct fdpath_slot written_path;
    struct fdpath_slot provisional_path;
    uint8_t reservation_owned;
    uint8_t identity_written;
    uint8_t path_written;
    uint8_t path_existed;
    uint8_t identity_replaced;
    uint8_t provisional_path_existed;
};

static int fdvis_fork_entry_matches_locked(const struct fdvis_slot *identity, const struct fdvis_fork_entry *entry,
                                           uint64_t key, uint64_t owner_start_ns) {
    if (identity->key != key || identity->owner_start_ns != owner_start_ns || identity->kind != entry->kind ||
        identity->device != entry->device || identity->object != entry->object)
        return 0;
    struct fdpath_slot *path = fdpath_find(key, owner_start_ns, 0);
    if (entry->path[0] == '\0') return path == NULL;
    return path && path->path_is_guest == entry->path_is_guest && strcmp(path->path, entry->path) == 0;
}

static void fdvis_fork_rollback_locked(struct fdvis_fork_journal *journal, size_t count) {
    while (count > 0) {
        struct fdvis_fork_journal *change = &journal[--count];
        if (change->path_written && change->path && change->path->key == change->key &&
            change->path->owner_start_ns == change->owner_start_ns &&
            change->path->path_is_guest == change->written_path.path_is_guest &&
            strcmp(change->path->path, change->written_path.path) == 0) {
            if (change->path_existed)
                *change->path = change->previous_path;
            else
                fdpath_delete_locked(change->path);
        }
        if (change->identity_written && change->identity->key == change->key &&
            change->identity->owner_start_ns == change->owner_start_ns &&
            change->identity->generation == change->generation) {
            if (change->identity_replaced)
                *change->identity = change->previous_identity;
            else
                memset(change->identity, 0, sizeof *change->identity);
        } else if (change->reservation_owned && change->identity->key == UINT64_MAX)
            memset(change->identity, 0, sizeof *change->identity);
        if (change->identity_replaced && change->provisional_path_existed) {
            struct fdpath_slot *provisional =
                fdpath_find(change->provisional_path.key, change->provisional_path.owner_start_ns, 1);
            if (provisional) *provisional = change->provisional_path;
        }
    }
}

static void fdvis_fork_commit_locked(struct fdvis_fork_journal *journal, size_t count) {
    for (size_t index = 0; index < count; ++index) {
        struct fdvis_fork_journal *change = &journal[index];
        if (!change->identity_replaced || !change->provisional_path_existed) continue;
        struct fdpath_slot *provisional =
            fdpath_find(change->provisional_path.key, change->provisional_path.owner_start_ns, 0);
        if (provisional && provisional->path_is_guest == change->provisional_path.path_is_guest &&
            strcmp(provisional->path, change->provisional_path.path) == 0)
            fdpath_delete_locked(provisional);
    }
}

static int proc_fdvis_after_fork(struct fdvis_fork_plan *plan, int child, int in_child) {
    uint64_t child_start = fdvis_process_token(child);
    if (!g_fdvis || !g_fdvis_control || child <= 0) return -EINVAL;
    /* The parent owns publication of the pre-fork reservations.  Letting both
     * branches publish races the child's immediate exit cleanup against the
     * parent's commit: cleanup can clear a slot between the two commits and
     * turn the parent's still-valid reservation into EAGAIN.  The child may
     * not expose its inherited descriptors until the parent's atomic batch is
     * visible, so wait here and then use the transaction below only for the
     * possible start-token upgrade. */
    if (in_child) {
        uint64_t deadline = fdvis_monotonic_milliseconds() + fdvis_fork_wait_milliseconds();
        for (;;) {
            int published = 1;
            fdvis_lock();
            for (size_t index = 0; index < plan->count; ++index) {
                const struct fdvis_fork_entry *entry = &plan->entries[index];
                const struct fdvis_slot *slot = &g_fdvis[entry->slot];
                if (slot->key != fdvis_key(child, entry->guest_fd)) {
                    published = 0;
                    break;
                }
            }
            fdvis_unlock();
            if (published) break;
            if ((int)getppid() != g_fdvis_fork_parent) {
                proc_fdvis_fork_child_abort(plan, child);
                return -ECHILD;
            }
            uint64_t now = fdvis_monotonic_milliseconds();
            if (now == 0 || now >= deadline) {
                if (proc_fdvis_fork_child_timeout(plan, child)) break;
                return -ETIMEDOUT;
            }
            struct timespec pause = {.tv_sec = 0, .tv_nsec = 1000000};
            (void)nanosleep(&pause, NULL);
        }
        child_start = fdvis_process_token(child);
    }
    struct fdvis_fork_journal *journal = calloc(plan->count, sizeof *journal);
    if (plan->count != 0 && !journal) {
        proc_fdvis_fork_cancel(plan);
        return -ENOMEM;
    }
    int status = 0;
    fdvis_lock();
    for (size_t index = 0; index < plan->count; ++index) {
        journal[index].identity = &g_fdvis[plan->entries[index].slot];
        journal[index].key = fdvis_key(child, plan->entries[index].guest_fd);
        journal[index].owner_start_ns = child_start;
        journal[index].reservation_owned = journal[index].identity->key == UINT64_MAX;
    }
    for (size_t index = 0; index < plan->count; ++index) {
        struct fdvis_fork_entry *entry = &plan->entries[index];
        struct fdvis_slot *copy = &g_fdvis[entry->slot];
        uint64_t key = fdvis_key(child, entry->guest_fd);
        if (fdvis_fork_entry_matches_locked(copy, entry, key, child_start)) continue;
        if (child_start == 0 && copy->key == key && copy->owner_start_ns != 0 &&
            fdvis_fork_entry_matches_locked(copy, entry, key, copy->owner_start_ns))
            continue;
        int token_upgrade = copy->key == key && copy->owner_start_ns == 0 && child_start != 0 &&
                            fdvis_fork_entry_matches_locked(copy, entry, key, 0);
        if (!journal[index].reservation_owned && !token_upgrade) {
            status = -EAGAIN;
            break;
        }
        if (token_upgrade) {
            journal[index].previous_identity = *copy;
            journal[index].identity_replaced = 1;
            struct fdpath_slot *provisional = fdpath_find(key, 0, 0);
            if (provisional) {
                journal[index].provisional_path = *provisional;
                journal[index].provisional_path_existed = 1;
            }
        }
        copy->device = entry->device;
        copy->object = entry->object;
        copy->kind = entry->kind;
        copy->owner_start_ns = child_start;
        copy->generation = ++g_fdvis_control->generation;
        copy->key = key;
        journal[index].generation = copy->generation;
        journal[index].identity_written = 1;
        struct fdpath_slot *previous_path = fdpath_find(key, child_start, 0);
        if (previous_path) {
            journal[index].previous_path = *previous_path;
            journal[index].path_existed = 1;
        }
        int restored = fdpath_restore_locked(key, child_start, entry->path, entry->path_is_guest);
        if (restored != 0) {
            status = restored;
            break;
        }
        if (entry->path[0] != '\0') {
            journal[index].path = fdpath_find(key, child_start, 0);
            journal[index].path_written = journal[index].path != NULL;
            if (journal[index].path_written) journal[index].written_path = *journal[index].path;
        }
    }
    if (status != 0) {
        fdvis_fork_rollback_locked(journal, plan->count);
    } else {
        fdvis_fork_commit_locked(journal, plan->count);
        if (in_child) {
            g_fdvis_fork_parent = child;
            g_fdvis_fork_parent_start = child_start;
        }
    }
    fdvis_unlock();
    if (status != 0 && !in_child) proc_fdvis_fork_parent_clear_timeout(plan, child);
    free(journal);
    return status;
}

#if defined(HL_NATIVE_TEST_HOOKS)
static int fdvis_after_fork_rollback_test(void) {
    struct fdvis_slot *identities = calloc(FDVIS_N, sizeof *identities);
    struct fdpath_slot *paths = calloc(FDPATH_N, sizeof *paths);
    struct fdvis_control *control = calloc(1, sizeof *control);
    if (!identities || !paths || !control) {
        free(identities);
        free(paths);
        free(control);
        return 0;
    }
    struct fdvis_slot *saved_identities = g_fdvis;
    struct fdpath_slot *saved_paths = g_fdpaths;
    struct fdvis_control *saved_control = g_fdvis_control;
    int child = (int)getpid();
    uint64_t child_start = fdvis_process_token(child);
    for (unsigned index = 0; index + 1 < FDPATH_N; ++index) {
        paths[index].key = fdvis_key(child, (int)index);
        paths[index].owner_start_ns = child_start;
    }
    identities[0].key = UINT64_MAX;
    identities[1].key = UINT64_MAX;
    struct fdvis_fork_entry entries[2] = {
        {.slot = 0,
         .guest_fd = HL_NFD - 2,
         .kind = 1,
         .device = 2,
         .object = 3,
         .path_is_guest = 1,
         .path = "/rollback/first"},
        {.slot = 1,
         .guest_fd = HL_NFD - 1,
         .kind = 4,
         .device = 5,
         .object = 6,
         .path_is_guest = 1,
         .path = "/rollback/second"},
    };
    struct fdvis_fork_plan plan = {.entries = entries, .count = 2};
    g_fdvis = identities;
    g_fdpaths = paths;
    g_fdvis_control = control;
    int status = proc_fdvis_after_fork(&plan, child, 0);
    uint64_t first_key = fdvis_key(child, entries[0].guest_fd);
    uint64_t second_key = fdvis_key(child, entries[1].guest_fd);
    int rolled_back = status == -ENOSPC && identities[0].key == 0 && identities[1].key == 0 &&
                      fdpath_find(first_key, child_start, 0) == NULL && fdpath_find(second_key, child_start, 0) == NULL;
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    memset(control, 0, sizeof *control);
    identities[0].key = UINT64_MAX;
    struct fdvis_fork_plan first_only = {.entries = entries, .count = 1};
    int first_status = proc_fdvis_after_fork(&first_only, child, 0);
    struct fdvis_slot successful_identity = identities[0];
    struct fdpath_slot *successful_path = fdpath_find(first_key, child_start, 0);
    struct fdpath_slot successful_path_value = {0};
    if (successful_path) successful_path_value = *successful_path;
    identities[1].key = UINT64_C(0x1234);
    int competing_status = proc_fdvis_after_fork(&plan, child, 0);
    successful_path = fdpath_find(first_key, child_start, 0);
    int preserved = first_status == 0 && competing_status == -EAGAIN &&
                    memcmp(&identities[0], &successful_identity, sizeof successful_identity) == 0 && successful_path &&
                    successful_path->key == successful_path_value.key &&
                    successful_path->owner_start_ns == successful_path_value.owner_start_ns &&
                    successful_path->path_is_guest == successful_path_value.path_is_guest &&
                    strcmp(successful_path->path, successful_path_value.path) == 0 &&
                    identities[1].key == UINT64_C(0x1234);
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    memset(control, 0, sizeof *control);
    identities[0] = (struct fdvis_slot){.key = first_key,
                                        .owner_start_ns = 0,
                                        .generation = 7,
                                        .kind = entries[0].kind,
                                        .device = entries[0].device,
                                        .object = entries[0].object};
    struct fdpath_slot *provisional = fdpath_find(first_key, 0, 1);
    if (provisional) {
        provisional->path_is_guest = entries[0].path_is_guest;
        snprintf(provisional->path, sizeof provisional->path, "%s", entries[0].path);
    }
    identities[1].key = UINT64_C(0x5678);
    int upgrade_failure = proc_fdvis_after_fork(&plan, child, 0);
    provisional = fdpath_find(first_key, 0, 0);
    int upgrade_rolled_back = upgrade_failure == -EAGAIN && identities[0].key == first_key &&
                              identities[0].owner_start_ns == 0 && identities[0].generation == 7 && provisional &&
                              strcmp(provisional->path, entries[0].path) == 0 &&
                              fdpath_find(first_key, child_start, 0) == NULL;
    identities[1].key = 0;
    int upgrade_success = proc_fdvis_after_fork(&first_only, child, 0);
    struct fdpath_slot *upgraded = fdpath_find(first_key, child_start, 0);
    int upgraded_cleanly = upgrade_success == 0 && identities[0].owner_start_ns == child_start && upgraded &&
                           strcmp(upgraded->path, entries[0].path) == 0 && fdpath_find(first_key, 0, 0) == NULL;
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    identities[0] = (struct fdvis_slot){.key = first_key, .owner_start_ns = child_start};
    identities[1].key = UINT64_MAX;
    struct fdpath_slot *partial_path = fdpath_find(first_key, child_start, 1);
    if (partial_path) snprintf(partial_path->path, sizeof partial_path->path, "%s", entries[0].path);
    proc_fdvis_fork_child_abort(&plan, child);
    int abandoned_cleanly =
        identities[0].key == 0 && identities[1].key == 0 && fdpath_find(first_key, child_start, 0) == NULL;
    struct fdvis_reservation reusable;
    int reserve_status = proc_fdvis_reserve(&reusable);
    abandoned_cleanly = abandoned_cleanly && reserve_status == 0;
    if (reserve_status == 0) proc_fdvis_reservation_cancel(&reusable);
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    identities[0] = (struct fdvis_slot){.key = first_key,
                                        .owner_start_ns = child_start,
                                        .generation = 11,
                                        .kind = entries[0].kind,
                                        .device = entries[0].device,
                                        .object = entries[0].object};
    identities[1] = (struct fdvis_slot){.key = second_key,
                                        .owner_start_ns = child_start,
                                        .generation = 12,
                                        .kind = entries[1].kind,
                                        .device = entries[1].device,
                                        .object = entries[1].object};
    struct fdpath_slot *committed_path = fdpath_find(first_key, child_start, 1);
    if (committed_path) snprintf(committed_path->path, sizeof committed_path->path, "%s", entries[0].path);
    int timeout_observed_commit = proc_fdvis_fork_child_timeout(&plan, child);
    committed_path = fdpath_find(first_key, child_start, 0);
    int commit_survived_timeout = timeout_observed_commit == 1 && identities[0].key == first_key &&
                                  identities[0].generation == 11 && identities[1].key == second_key &&
                                  identities[1].generation == 12 && committed_path &&
                                  strcmp(committed_path->path, entries[0].path) == 0;
    g_fdvis = saved_identities;
    g_fdpaths = saved_paths;
    g_fdvis_control = saved_control;
    free(identities);
    free(paths);
    free(control);
    return rolled_back && preserved && upgrade_rolled_back && upgraded_cleanly && abandoned_cleanly &&
           commit_survived_timeout;
}

#if !defined(_WIN32)
static int fdvis_stalled_parent_test(void) {
    struct fdvis_slot *identities =
        mmap(NULL, sizeof *identities * FDVIS_N, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    struct fdpath_slot *paths =
        mmap(NULL, sizeof *paths * FDPATH_N, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    struct fdvis_control *control =
        mmap(NULL, sizeof *control, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (identities == MAP_FAILED || paths == MAP_FAILED || control == MAP_FAILED) {
        if (identities != MAP_FAILED) (void)munmap(identities, sizeof *identities * FDVIS_N);
        if (paths != MAP_FAILED) (void)munmap(paths, sizeof *paths * FDPATH_N);
        if (control != MAP_FAILED) (void)munmap(control, sizeof *control);
        return 0;
    }
    memset(identities, 0, sizeof *identities * FDVIS_N);
    memset(paths, 0, sizeof *paths * FDPATH_N);
    memset(control, 0, sizeof *control);
    struct fdvis_slot *saved_identities = g_fdvis;
    struct fdpath_slot *saved_paths = g_fdpaths;
    struct fdvis_control *saved_control = g_fdvis_control;
    g_fdvis = identities;
    g_fdpaths = paths;
    g_fdvis_control = control;
    int parent = (int)getpid();
    uint64_t parent_start = fdvis_process_token(parent);
    identities[0] = (struct fdvis_slot){.key = fdvis_key(parent, 3),
                                        .owner_start_ns = parent_start,
                                        .generation = 1,
                                        .kind = 1,
                                        .device = 2,
                                        .object = 3};
    struct fdvis_fork_plan plan;
    int prepared = proc_fdvis_fork_prepare(&plan);
    pid_t child = prepared == 0 ? fork() : -1;
    if (child == 0) {
        g_fdvis_fork_wait_milliseconds = 20;
        int status = proc_fdvis_after_fork(&plan, (int)getpid(), 1);
        _exit(status == -ETIMEDOUT ? 0 : 1);
    }
    int child_status = 1;
    int parent_status = -1;
    if (child > 0) {
        struct timespec hold = {.tv_sec = 0, .tv_nsec = 100000000};
        (void)nanosleep(&hold, NULL);
        parent_status = proc_fdvis_after_fork(&plan, (int)child, 0);
        while (waitpid(child, &child_status, 0) < 0 && errno == EINTR) {}
    }
    struct fdvis_fork_plan retry;
    int retry_status = proc_fdvis_fork_prepare(&retry);
    if (retry_status == 0) proc_fdvis_fork_cancel(&retry);
    int clean = child > 0 && WIFEXITED(child_status) && WEXITSTATUS(child_status) == 0 && parent_status != 0 &&
                identities[1].key == 0 && retry_status == 0;
    free(plan.entries);
    g_fdvis = saved_identities;
    g_fdpaths = saved_paths;
    g_fdvis_control = saved_control;
    (void)munmap(identities, sizeof *identities * FDVIS_N);
    (void)munmap(paths, sizeof *paths * FDPATH_N);
    (void)munmap(control, sizeof *control);
    return clean;
}
#else
/* This scenario forks and rendezvouses through a MAP_SHARED anonymous page; the Windows target has
 * neither spelling. It reports 0 -- a refusal scenario 7 propagates to its caller -- rather than 1,
 * which would report a half that never ran as having passed. The sibling rollback scenario above is
 * portable and still runs here. */
static int fdvis_stalled_parent_test(void) {
    return 0;
}
#endif

#if !defined(_WIN32)
/* The lock word has to be reclaimable from an owner that is dead but NOT YET COLLECTED.
 *
 * A holder killed inside the critical section never reaches fdvis_unlock(), and its /proc entry -- start
 * time included -- survives until its parent waits for it, so an owner check built only on that stamp goes
 * on reading "live" forever. Reclaiming only after collection is not a race window that closes on its own:
 * in the ordinary double-fork shape the task that would collect the corpse is the middle process, and the
 * middle process is by then spinning on this very word inside its own exit path. Measured before the fix:
 * one core at 99.9% for as long as the run was allowed to last.
 *
 * The topology is reproduced exactly -- a corpse this process deliberately leaves uncollected while a
 * SECOND child asks for the word. The acquisition is watched with a deadline rather than joined, so an
 * engine that cannot reclaim fails this scenario instead of hanging the suite that runs it. */
static int fdvis_corpse_holder_test(void) {
    struct fdvis_control *control =
        mmap(NULL, sizeof *control, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    unsigned char *held = mmap(NULL, 1, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (control == MAP_FAILED || held == MAP_FAILED) {
        if (control != MAP_FAILED) (void)munmap(control, sizeof *control);
        if (held != MAP_FAILED) (void)munmap(held, 1);
        return 0;
    }
    memset(control, 0, sizeof *control);
    *held = 0;
    struct fdvis_control *saved_control = g_fdvis_control;
    g_fdvis_control = control;
    const struct timespec tick = {.tv_sec = 0, .tv_nsec = 1000000};
    pid_t holder = fork();
    if (holder == 0) {
        fdvis_lock();
        __atomic_store_n(held, 1, __ATOMIC_RELEASE);
        for (;;) {
            struct timespec forever = {.tv_sec = 3600, .tv_nsec = 0};
            (void)nanosleep(&forever, NULL);
        }
    }
    int acquired = 0;
    if (holder > 0) {
        for (int spin = 0; spin < 5000 && __atomic_load_n(held, __ATOMIC_ACQUIRE) == 0; ++spin)
            (void)nanosleep(&tick, NULL);
        /* The word must name the holder under ITS OWN identity: a child that reported its parent's pid
         * would make everything below pass for the wrong reason. */
        uint64_t owner = atomic_load_explicit(&control->owner, memory_order_relaxed);
        int owned_by_holder =
            __atomic_load_n(held, __ATOMIC_ACQUIRE) == 1 && (int)(uint32_t)(owner >> 32) == (int)holder;
        (void)kill(holder, SIGKILL);
        int corpse = 0;
        for (int spin = 0; spin < 5000 && owned_by_holder && !corpse; ++spin) {
            hl_host_process_info record;
            if (hl_host_process_read((int64_t)holder, &record) && record.state == 'Z')
                corpse = 1;
            else
                (void)nanosleep(&tick, NULL);
        }
        if (corpse) {
            pid_t claimant = fork();
            if (claimant == 0) {
                fdvis_lock();
                fdvis_unlock();
                _exit(0);
            }
            if (claimant > 0) {
                int status = 0;
                for (int spin = 0; spin < 5000; ++spin) {
                    pid_t seen = waitpid(claimant, &status, WNOHANG);
                    if (seen == claimant) {
                        acquired = WIFEXITED(status) && WEXITSTATUS(status) == 0;
                        break;
                    }
                    if (seen < 0 && errno != EINTR) break;
                    (void)nanosleep(&tick, NULL);
                }
                if (!acquired) {
                    (void)kill(claimant, SIGKILL);
                    while (waitpid(claimant, &status, 0) < 0 && errno == EINTR) {}
                }
            }
        }
        /* Collected only now, and only so the scenario leaves no corpse of its own behind: the claimant
         * above had to succeed without it. */
        int status = 0;
        while (waitpid(holder, &status, 0) < 0 && errno == EINTR) {}
    }
    g_fdvis_control = saved_control;
    (void)munmap(control, sizeof *control);
    (void)munmap(held, 1);
    return acquired;
}
#else
/* Forking, SIGKILL and a MAP_SHARED anonymous page have no Windows spelling, and the Windows process
 * reader answers "gone" for every pid, so the reclaim this scenario is about cannot be staged there. It
 * reports 0 rather than 1, which would record a scenario that never ran as having passed. */
static int fdvis_corpse_holder_test(void) {
    return 0;
}
#endif
#endif

static void proc_fdvis_cleanup(void) {
    int owner = (int)getpid();
    uint64_t owner_start = fdvis_process_token(owner);
    if (!g_fdvis || !g_fdvis_control) return;
    fdvis_lock();
    for (unsigned index = 0; index < FDVIS_N; ++index)
        if ((int)(uint32_t)(g_fdvis[index].key >> 32) == owner && g_fdvis[index].owner_start_ns == owner_start)
            memset(&g_fdvis[index], 0, sizeof g_fdvis[index]);
    fdpath_cleanup_owner_locked(owner, owner_start);
    fdvis_unlock();
}
