static void gna_writer_lock(void) {
    while (atomic_flag_test_and_set_explicit(&g_gna_writer, memory_order_acquire))
        sched_yield();
}

static void gna_writer_unlock(void) {
    atomic_flag_clear_explicit(&g_gna_writer, memory_order_release);
}

// Guest read-only ranges physically protected on the host. The x86 lazy-map fault handler must not
// reinterpret a legitimate write-protection fault as demand-zero growth and silently make it writable.
static struct {
    uint64_t lo, hi;
} g_gro[GNA_MAX];

static int g_ngro;
static _Atomic uint64_t g_gro_generation = 2;
static atomic_flag g_gro_writer = ATOMIC_FLAG_INIT;
static void gro_clear(uint64_t lo, uint64_t hi);
static void gro_clear_raw(uint64_t lo, uint64_t hi);

// Guest ranges without PROT_EXEC.  DBT instruction bytes are host data and
// therefore remain readable even when Linux would reject an instruction fetch;
// keep execute permission separately from the host mapping protection.
static struct {
    uint64_t lo, hi;
} g_gnx[GNA_MAX];

static int g_ngnx;
static _Atomic uint64_t g_gnx_generation = 2;
static atomic_flag g_gnx_writer = ATOMIC_FLAG_INIT;
static void gnx_clear(uint64_t lo, uint64_t hi);
static void gnx_clear_raw(uint64_t lo, uint64_t hi);
static void gnx_reset(void);

static void gro_writer_lock(void) {
    while (atomic_flag_test_and_set_explicit(&g_gro_writer, memory_order_acquire))
        sched_yield();
}

static void gro_writer_unlock(void) {
    atomic_flag_clear_explicit(&g_gro_writer, memory_order_release);
}

struct guest_bus_range {
    uint64_t lo, hi;
};
static struct guest_bus_range g_gbus[GNA_MAX];
static _Atomic int g_ngbus;
/* Past-EOF ranges whose guest pages are currently PROT_NONE.  Linux answers a
   touch there with a permission fault and never with SIGBUS, so such a range
   must not arm the translated BUS guard -- but its justification returns the
   moment the guest restores an accessible protection, so it is parked here
   rather than discarded.  ld.so PROT_NONEs the inter-segment hole of every
   shared library it maps, and that hole is the tail of the whole-span
   file-backed reservation, so without this every dynamically linked guest
   armed the ledger during startup and never disarmed it. */
static struct guest_bus_range g_gbus_parked[GNA_MAX];
static int g_ngbus_parked;
static _Atomic uint64_t g_bus_generation = 1;
static atomic_flag g_bus_lock = ATOMIC_FLAG_INIT;
static int g_bus_fail_closed;
static uint32_t g_bus_prepares;
/* Conservative lock-free rejection envelope for translated memory guards.
   A miss is definitive; an envelope hit takes the precise ledger lock. */
static _Atomic uint64_t g_bus_filter_lo = UINT64_MAX;
static _Atomic uint64_t g_bus_filter_hi;
/* Runtime guard state: 0 inactive, 1 active/filterable, 3 transition/precise.
   Bit encoding lets emitted guards test it without disturbing guest flags. */
static _Atomic int g_bus_filter_force;
#define BUS_FILTER_WORDS 1024u
#define BUS_FILTER_BITS (BUS_FILTER_WORDS * 64u)
static _Atomic uint64_t g_bus_page_filter[BUS_FILTER_WORDS];
static hl_linux_bus_change_fn g_bus_callback;
static void *g_bus_callback_opaque;
static hl_linux_bus_transition_fn g_bus_transition_begin;
static hl_linux_bus_transition_fn g_bus_transition_end;
static void *g_bus_transition_opaque;
static pthread_once_t g_bus_atfork_once = PTHREAD_ONCE_INIT;
static pthread_mutex_t g_bus_transition = PTHREAD_MUTEX_INITIALIZER;
static void gbus_clear(uint64_t lo, uint64_t hi);
static int gbus_add(uint64_t lo, uint64_t hi);

/* Remove [lo,hi) from the parked set, splitting a straddling entry so a
   partial re-protect restores exactly the covered part.  A split that cannot
   be recorded falls closed, matching gbus_clear_locked: a parked range that
   silently vanished would lose a real SIGBUS after the guest restores an
   accessible protection. */
static int gbus_parked_clear_locked(uint64_t lo, uint64_t hi) {
    int changed = 0;
    for (int index = 0; index < g_ngbus_parked;) {
        uint64_t base = g_gbus_parked[index].lo, end = g_gbus_parked[index].hi;
        if (lo >= end || hi <= base) {
            index++;
            continue;
        }
        changed = 1;
        int head = base < lo, tail = hi < end;
        if (!head && !tail) {
            g_gbus_parked[index] = g_gbus_parked[--g_ngbus_parked];
            continue;
        }
        if (head)
            g_gbus_parked[index].hi = lo;
        else
            g_gbus_parked[index].lo = hi;
        if (head && tail) {
            if (g_ngbus_parked < GNA_MAX)
                g_gbus_parked[g_ngbus_parked++] = (struct guest_bus_range){hi, end};
            else
                g_bus_fail_closed = 1;
        }
        index++;
    }
    return changed;
}

static void gbus_parked_append_locked(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    (void)gbus_parked_clear_locked(lo, hi);
    if (g_ngbus_parked < GNA_MAX)
        g_gbus_parked[g_ngbus_parked++] = (struct guest_bus_range){lo, hi};
    else
        g_bus_fail_closed = 1;
}

static int gbus_clear_locked(uint64_t lo, uint64_t hi) {
    int changed = 0;
    for (int index = 0; index < g_ngbus;) {
        uint64_t base = g_gbus[index].lo, end = g_gbus[index].hi;
        if (lo >= end || hi <= base) {
            index++;
            continue;
        }
        changed = 1;
        int head = base < lo, tail = hi < end;
        if (!head && !tail) {
            g_gbus[index] = g_gbus[--g_ngbus];
            continue;
        }
        if (head)
            g_gbus[index].hi = lo;
        else
            g_gbus[index].lo = hi;
        if (head && tail) {
            if (g_ngbus < GNA_MAX)
                g_gbus[g_ngbus++] = (struct guest_bus_range){hi, end};
            else
                g_bus_fail_closed = 1;
        }
        index++;
    }
    return changed;
}

/* File identity for legacy native-descriptor VMAs.  Typed mappings carry the
   same information in binding.c; this registry keeps the production legacy
   mmap path coherent when ftruncate is issued through a dup or reopened fd. */
struct guest_file_mapping {
    uint64_t lo, hi, offset, device, inode;
    uint64_t follow_lo, follow_hi;
    int fd;
    uint32_t shared;
    uint32_t emulated;
};
static struct guest_file_mapping g_filemap[GNA_MAX];
static int g_nfilemap;
// Conservative address envelope for mappings that can source a MAP_SHARED
// emulated refresh. It only expands, so lock-free readers can reject the common
// executable-code page without racing unmap/split updates; stale bounds merely
// fall through to the existing locked scan.
static _Atomic uint64_t g_filemap_shared_lo = UINT64_MAX;
static _Atomic uint64_t g_filemap_shared_hi;
static _Atomic uint64_t g_filemap_shared_epoch;
static _Atomic int g_filemap_emulated_shared;
#define FILEMAP_SHARED_FILTER_WORDS 1024u
#define FILEMAP_SHARED_FILTER_BITS (FILEMAP_SHARED_FILTER_WORDS * 64u)
// Monotonic page bloom for the executable-alias store observer. A stale bit only causes a locked registry
// scan; clearing on unmap could race a replacement mapping and miss a real shared write. The exact registry
// remains authoritative.
static _Atomic uint64_t g_filemap_shared_filter[FILEMAP_SHARED_FILTER_WORDS];
static pthread_mutex_t g_filemap_lock = PTHREAD_MUTEX_INITIALIZER;

static void filemap_shared_filter_add(uint64_t address, uint64_t size) {
    uint64_t first = address >> 12;
    uint64_t last = (address + size - 1) >> 12;
    if (last - first >= FILEMAP_SHARED_FILTER_BITS) {
        for (size_t index = 0; index < FILEMAP_SHARED_FILTER_WORDS; ++index)
            atomic_store_explicit(&g_filemap_shared_filter[index], UINT64_MAX, memory_order_release);
        return;
    }
    for (uint64_t page = first;; ++page) {
        uint64_t bit = page & (FILEMAP_SHARED_FILTER_BITS - 1);
        atomic_fetch_or_explicit(&g_filemap_shared_filter[bit >> 6], UINT64_C(1) << (bit & 63), memory_order_release);
        if (page == last) break;
    }
}

static int filemap_shared_filter_maybe(uint64_t address, uint64_t size) {
    uint64_t first = address >> 12;
    uint64_t last = (address + size - 1) >> 12;
    if (last - first >= FILEMAP_SHARED_FILTER_BITS) return 1;
    for (uint64_t page = first;; ++page) {
        uint64_t bit = page & (FILEMAP_SHARED_FILTER_BITS - 1);
        if (atomic_load_explicit(&g_filemap_shared_filter[bit >> 6], memory_order_acquire) &
            (UINT64_C(1) << (bit & 63)))
            return 1;
        if (page == last) break;
    }
    return 0;
}

/* A file mapping survives fork in every guest process, while the bookkeeping
   above becomes process-private COW memory.  File size/data mutations do not:
   macOS can leave a clean MAP_PRIVATE subpage stale after another process
   shrinks and regrows the vnode.  Journal those mutations in an inherited
   shared arena.  Each process replays them before returning from a syscall,
   which is the same visibility boundary that ordered the mutating process's
   pipe/socket/file notification. */
#define FILEMAP_EVENT_COUNT 65536u

struct filemap_event {
    _Atomic uint64_t sequence;
    uint64_t device, inode, first, second;
    uint32_t kind;
};

struct filemap_events {
    _Atomic uint64_t next;
    struct filemap_event event[FILEMAP_EVENT_COUNT];
};
static struct filemap_events *g_filemap_events;
static uint64_t g_filemap_cursor;
static pthread_mutex_t g_filemap_replay_lock = PTHREAD_MUTEX_INITIALIZER;
static hl_linux_file_event_fn g_file_event_callback;
static void *g_file_event_opaque;

static void filemap_events_init_locked(void) {
    void *arena = NULL;
    if (g_filemap_events != NULL) return;
    if (hl_linux_shared_create(effective_host_services(), sizeof(struct filemap_events), &arena) != HL_STATUS_OK)
        return;
    g_filemap_events = arena;
    g_filemap_cursor = 0;
}

static int hl_linux_file_events_enable(void) {
    pthread_mutex_lock(&g_filemap_lock);
    filemap_events_init_locked();
    int enabled = g_filemap_events != NULL;
    pthread_mutex_unlock(&g_filemap_lock);
    return enabled ? 0 : -1;
}

static void hl_linux_file_events_set_callback(hl_linux_file_event_fn callback, void *opaque) {
    pthread_mutex_lock(&g_filemap_replay_lock);
    g_file_event_callback = callback;
    g_file_event_opaque = opaque;
    pthread_mutex_unlock(&g_filemap_replay_lock);
}

static void filemap_publish(uint32_t kind, uint64_t device, uint64_t inode, uint64_t first, uint64_t second) {
    struct filemap_events *events = g_filemap_events;
    if (events == NULL) return;
    uint64_t ticket = atomic_fetch_add_explicit(&events->next, 1, memory_order_relaxed);
    struct filemap_event *event = &events->event[ticket % FILEMAP_EVENT_COUNT];
    event->device = device;
    event->inode = inode;
    event->first = first;
    event->second = second;
    event->kind = kind;
    atomic_store_explicit(&event->sequence, ticket + 1, memory_order_release);
}

static void hl_linux_file_event_publish(uint32_t kind, uint64_t device, uint64_t object, uint64_t first,
                                        uint64_t second) {
    if (kind == HL_LINUX_FILE_EVENT_RESIZE || kind == HL_LINUX_FILE_EVENT_WRITE)
        filemap_publish(kind, device, object, first, second);
}

static uint64_t filemap_accessible(const struct guest_file_mapping *mapping, uint64_t size) {
    if (size <= mapping->offset) return 0;
    uint64_t available = size - mapping->offset;
    if (available > UINT64_MAX - UINT64_C(4095)) return mapping->hi - mapping->lo;
    uint64_t rounded = (available + UINT64_C(4095)) & ~UINT64_C(4095);
    uint64_t length = mapping->hi - mapping->lo;
    return rounded < length ? rounded : length;
}

static void filemap_register(uint64_t address, uint64_t size, int fd, uint64_t offset, int shared, int emulated) {
    struct stat st;
    if (size == 0 || address > UINT64_MAX - size || fstat(fd, &st) != 0) return;
    pthread_mutex_lock(&g_filemap_lock);
    filemap_events_init_locked();
    int retained = -1;
    for (int index = 0; index < g_nfilemap; ++index)
        if (g_filemap[index].device == (uint64_t)st.st_dev && g_filemap[index].inode == (uint64_t)st.st_ino) {
            retained = g_filemap[index].fd;
            break;
        }
    if (retained < 0) {
        // The retained backing descriptor must stay OUT of the guest's fd interval. The fixed 1<<20 / HL_NFD
        // targets fail with EINVAL when RLIMIT_NOFILE sits below them (the GitHub aarch64 runner boots with a
        // soft limit under 65536), which previously left retained < 0 and DROPPED the mapping entirely -- so
        // filemap_has_shared_mapping() then missed a live MAP_SHARED memfd mapping and the F_SEAL_WRITE EBUSY
        // guard regressed (memfd-seal-busy: wr_ebusy/ro_ebusy=0 vs golden 1) while the VM's huge limit hid it.
        // Anchor at the engine's RLIMIT-aware private floor first (the boundary hl_host_process_fd_private_*
        // already use to keep native handles disjoint from guest fds), then the legacy fixed targets, then any
        // free descriptor -- so a file-backed mapping is always registered regardless of the host fd limit.
        int floor = hl_host_process_fd_private_floor();
        if (floor > 0) retained = fcntl(fd, F_DUPFD_CLOEXEC, floor);
        if (retained < 0) retained = fcntl(fd, F_DUPFD_CLOEXEC, 1 << 20);
        if (retained < 0) retained = fcntl(fd, F_DUPFD_CLOEXEC, HL_NFD);
        if (retained < 0) retained = fcntl(fd, F_DUPFD_CLOEXEC, 0);
    }
    if (g_nfilemap < GNA_MAX && retained >= 0) {
        if (shared) atomic_fetch_add_explicit(&g_filemap_shared_epoch, 1, memory_order_seq_cst);
        if (shared) {
            filemap_shared_filter_add(address, size);
            uint64_t old = atomic_load_explicit(&g_filemap_shared_lo, memory_order_relaxed);
            while (address < old &&
                   !atomic_compare_exchange_weak_explicit(&g_filemap_shared_lo, &old, address, memory_order_release,
                                                          memory_order_relaxed)) {}
            uint64_t end = address + size;
            old = atomic_load_explicit(&g_filemap_shared_hi, memory_order_relaxed);
            while (end > old && !atomic_compare_exchange_weak_explicit(&g_filemap_shared_hi, &old, end,
                                                                       memory_order_release, memory_order_relaxed)) {}
        }
        // Publish the conservative envelope before the entry itself. A lock-free
        // refresher that observes the wider bounds then takes g_filemap_lock and
        // cannot scan until this fully initialized entry is visible.
        g_filemap[g_nfilemap++] = (struct guest_file_mapping){
            address, address + size, offset,           (uint64_t)st.st_dev, (uint64_t)st.st_ino, 0,
            0,       retained,       (uint32_t)shared, (uint32_t)emulated};
        if (shared) {
            /* The registry entry is not yet visible outside g_filemap_lock. Disable byte-authorized
               decode hits before releasing it, so a writable alias cannot race stale decoded IR. */
            atomic_store_explicit(&g_exec_bytes_unstable, 1, memory_order_release);
            if (emulated) atomic_store_explicit(&g_filemap_emulated_shared, 1, memory_order_release);
        }
        if (shared) atomic_fetch_add_explicit(&g_filemap_shared_epoch, 1, memory_order_seq_cst);
    } else if (retained >= 0) {
        int shared_source = 0;
        for (int index = 0; index < g_nfilemap; ++index)
            if (g_filemap[index].fd == retained) shared_source = 1;
        if (!shared_source) close(retained);
    }
    pthread_mutex_unlock(&g_filemap_lock);
}

static int filemap_emulated_shared_active(void) {
    return atomic_load_explicit(&g_filemap_emulated_shared, memory_order_acquire);
}

#if defined(HL_NATIVE_TEST_HOOKS)
static void filemap_test_set_emulated_shared(int active) {
    atomic_store_explicit(&g_filemap_emulated_shared, active, memory_order_release);
}
#endif

static ssize_t filemap_pread(int fd, void *buffer, size_t length, off_t offset) {
    ssize_t result;
    do
        result = pread(fd, buffer, length, offset);
    while (result < 0 && errno == EINTR);
    return result;
}

static ssize_t filemap_pwrite(int fd, const void *buffer, size_t length, off_t offset) {
    const unsigned char *cursor = buffer;
    size_t written = 0;
    while (written < length) {
        ssize_t result = pwrite(fd, cursor + written, length - written, offset + (off_t)written);
        if (result > 0) {
            written += (size_t)result;
            continue;
        }
        if (result < 0 && errno == EINTR) continue;
        return written != 0 ? (ssize_t)written : result;
    }
    return (ssize_t)written;
}

static int filemap_source_fd(struct guest_file_mapping *mapping);

/*
 * A Linux 4K-offset file mapping cannot always be represented by a native
 * 16K-offset mmap on Apple silicon.  mmap therefore materializes those VMAs
 * as private snapshots (emulated=1).  MAP_SHARED stores must still update the
 * backing object before a following syscall can notify another process.
 *
 * ONLY the snapshot form.  mmap has a second emulated=1 representation: the
 * logical-VMA ledger, which keeps a real MAP_SHARED window over the same vnode
 * at an engine-private host address and relocates guest accesses into it
 * (host = guest + host_delta).  Those stores are already in the vnode -- and
 * the guest address they name is a reservation holding no data at all, so
 * writing it back would publish unrelated bytes over the guest's own.
 */
static int filemap_bytes_live_at_guest_address(uint64_t first, uint64_t last) {
    return !hl_logical_vma_global_overlap(first, last - first);
}

static void filemap_flush_emulated(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    pthread_mutex_lock(&g_filemap_lock);
    for (int index = 0; index < g_nfilemap; ++index) {
        struct guest_file_mapping *mapping = &g_filemap[index];
        if (!mapping->shared || !mapping->emulated || hi <= mapping->lo || lo >= mapping->hi) continue;
        uint64_t first = lo > mapping->lo ? lo : mapping->lo;
        uint64_t last = hi < mapping->hi ? hi : mapping->hi;
        uint64_t offset = mapping->offset + first - mapping->lo;
        if (!filemap_bytes_live_at_guest_address(first, last)) continue;
        int fd = filemap_source_fd(mapping);
        if (fd < 0) continue;
        if (filemap_pwrite(fd, (const void *)(uintptr_t)first, (size_t)(last - first), (off_t)offset) ==
            (ssize_t)(last - first))
            filemap_publish(HL_LINUX_FILE_EVENT_WRITE, mapping->device, mapping->inode, offset, last - first);
    }
    pthread_mutex_unlock(&g_filemap_lock);
}

static void filemap_refresh_emulated(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    uint64_t epoch = atomic_load_explicit(&g_filemap_shared_epoch, memory_order_seq_cst);
    if (!(epoch & 1)) {
        uint64_t shared_lo = atomic_load_explicit(&g_filemap_shared_lo, memory_order_relaxed);
        uint64_t shared_hi = atomic_load_explicit(&g_filemap_shared_hi, memory_order_relaxed);
        if (epoch == atomic_load_explicit(&g_filemap_shared_epoch, memory_order_seq_cst) &&
            (hi <= shared_lo || lo >= shared_hi))
            return;
    }
    pthread_mutex_lock(&g_filemap_lock);
    /* Host-page emulation creates a private snapshot.  Refresh every
       registered snapshot of the same shared file extent, not merely the
       virtual range named by the caller: MAP_SHARED coherence is defined by
       backing identity and offset.  Provider memory is not registered here
       and remains owned by its explicit provider coherence contract. */
    for (int source_index = 0; source_index < g_nfilemap; ++source_index) {
        struct guest_file_mapping *source = &g_filemap[source_index];
        if (!source->shared || hi <= source->lo || lo >= source->hi) continue;
        uint64_t source_first = lo > source->lo ? lo : source->lo;
        uint64_t source_last = hi < source->hi ? hi : source->hi;
        uint64_t file_first = source->offset + source_first - source->lo;
        uint64_t file_last = source->offset + source_last - source->lo;
        int fd = filemap_source_fd(source);
        if (fd < 0) continue;

        for (int target_index = 0; target_index < g_nfilemap; ++target_index) {
            struct guest_file_mapping *target = &g_filemap[target_index];
            uint64_t target_size = target->hi - target->lo;
            if (!target->shared || !target->emulated || target->device != source->device ||
                target->inode != source->inode || target->offset > UINT64_MAX - target_size)
                continue;
            uint64_t target_last = target->offset + target_size;
            uint64_t overlap_first = file_first > target->offset ? file_first : target->offset;
            uint64_t overlap_last = file_last < target_last ? file_last : target_last;
            if (overlap_last <= overlap_first) continue;
            (void)filemap_pread(fd, (void *)(uintptr_t)(target->lo + overlap_first - target->offset),
                                (size_t)(overlap_last - overlap_first), (off_t)overlap_first);
        }
    }
    pthread_mutex_unlock(&g_filemap_lock);
}

static void filemap_unmap(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    pthread_mutex_lock(&g_filemap_lock);
    for (int i = 0; i < g_nfilemap;) {
        struct guest_file_mapping *mapping = &g_filemap[i];
        if (hi <= mapping->lo || lo >= mapping->hi) {
            i++;
            continue;
        }
        uint64_t old_lo = mapping->lo, old_hi = mapping->hi;
        if (lo <= old_lo && hi >= old_hi) {
            int retained = mapping->fd;
            g_filemap[i] = g_filemap[--g_nfilemap];
            int used = 0;
            for (int index = 0; index < g_nfilemap; ++index)
                if (g_filemap[index].fd == retained) used = 1;
            if (!used && retained >= 0) close(retained);
            continue;
        }
        if (lo > old_lo && hi < old_hi && g_nfilemap < GNA_MAX) {
            struct guest_file_mapping tail = *mapping;
            tail.lo = hi;
            tail.offset += hi - old_lo;
            mapping->hi = lo;
            g_filemap[g_nfilemap++] = tail;
            i++;
            continue;
        }
        if (lo <= old_lo) {
            uint64_t cut = hi - old_lo;
            mapping->lo = hi;
            mapping->offset += cut;
        } else {
            mapping->hi = lo;
        }
        i++;
    }
    pthread_mutex_unlock(&g_filemap_lock);
}

// memfd F_SEAL_WRITE (io.c fcntl) must fail EBUSY while an outstanding MAP_SHARED mapping of the same object
// is live (Linux mm/shmem.c gates the seal on the address_space's writable-mapping count). A memfd is always
// opened read-write, so every shared mapping of it carries VM_MAYWRITE and counts regardless of the
// mapping's current PROT (a PROT_READ shared map, or a shared map later mprotect'd read-only, still blocks);
// only MAP_PRIVATE (COW) mappings are exempt. Scan the file-mapping registry for a live shared mapping of
// this fd's (device, inode).
static int filemap_has_shared_mapping(int fd) {
    struct stat st;
    if (fd < 0 || fstat(fd, &st) != 0) return 0;
    int found = 0;
    pthread_mutex_lock(&g_filemap_lock);
    for (int i = 0; i < g_nfilemap; ++i)
        if (g_filemap[i].shared && g_filemap[i].device == (uint64_t)st.st_dev &&
            g_filemap[i].inode == (uint64_t)st.st_ino) {
            found = 1;
            break;
        }
    pthread_mutex_unlock(&g_filemap_lock);
    return found;
}

static void filemap_resize_identity(uint64_t device, uint64_t inode, uint64_t old_size, uint64_t new_size) {
    pthread_mutex_lock(&g_filemap_lock);
    for (int i = 0; i < g_nfilemap; ++i) {
        struct guest_file_mapping *mapping = &g_filemap[i];
        if (mapping->device != device || mapping->inode != inode) continue;
        uint64_t old_accessible = filemap_accessible(mapping, old_size);
        uint64_t new_accessible = filemap_accessible(mapping, new_size);
        if (new_size < old_size && new_size > mapping->offset &&
            new_size < mapping->offset + (mapping->hi - mapping->lo)) {
            uint64_t tail = new_size - mapping->offset;
            uint64_t partial_end = (tail + UINT64_C(4095)) & ~UINT64_C(4095);
            if (partial_end > mapping->hi - mapping->lo) partial_end = mapping->hi - mapping->lo;
            if (partial_end > tail) memset((void *)(uintptr_t)(mapping->lo + tail), 0, (size_t)(partial_end - tail));
        }
        if (new_accessible < old_accessible)
            (void)gbus_add(mapping->lo + new_accessible, mapping->hi);
        else if (new_accessible > old_accessible) {
            /* macOS may retain stale private-page cache (or an anonymous quiet
               EOF tail) across shrink+extend.  Recreate each newly valid host
               page as a private snapshot while preserving the previously
               valid prefix, exactly matching Linux MAP_PRIVATE regrowth. */
            if (!mapping->shared) {
                mapping->follow_lo = old_accessible;
                mapping->follow_hi = new_accessible;
                long hp = (long)hl_linux_host_map_granularity();
                uint64_t cursor = old_accessible;
                while (hp > 0 && cursor < new_accessible) {
                    uint64_t absolute = mapping->lo + cursor;
                    uint64_t page_lo = absolute & ~((uint64_t)hp - 1u);
                    uint64_t page_off = page_lo - mapping->lo;
                    /* Invalidate clean private file pages instead of copying
                       bytes into them: copying would COW the whole 16K host
                       page and later file writes would stop being visible to
                       Linux's independently clean 4K subpages. Dirty private
                       pages are retained by MS_INVALIDATE. */
                    (void)msync((void *)(uintptr_t)page_lo, (size_t)hp, MS_INVALIDATE);
                    cursor = page_off + (uint64_t)hp;
                }
            }
            gbus_clear(mapping->lo + old_accessible, mapping->lo + new_accessible);
        }
    }
    pthread_mutex_unlock(&g_filemap_lock);
}

static void filemap_resize(int fd, uint64_t old_size, uint64_t new_size) {
    struct stat st;
    if (fstat(fd, &st) != 0) return;
    uint64_t device = (uint64_t)st.st_dev, inode = (uint64_t)st.st_ino;
    filemap_resize_identity(device, inode, old_size, new_size);
    filemap_publish(HL_LINUX_FILE_EVENT_RESIZE, device, inode, old_size, new_size);
}

static int filemap_source_fd(struct guest_file_mapping *mapping) {
    struct stat st;
    if (mapping->fd >= 0 && fstat(mapping->fd, &st) == 0 && (uint64_t)st.st_dev == mapping->device &&
        (uint64_t)st.st_ino == mapping->inode)
        return mapping->fd;
    return -1;
}

static void filemap_written_identity(uint64_t device, uint64_t inode, int source_fd, uint64_t offset, uint64_t size) {
    if (size == 0 || offset > UINT64_MAX - size) return;
    uint64_t end = offset + size;
    pthread_mutex_lock(&g_filemap_lock);
    for (int i = 0; i < g_nfilemap; ++i) {
        struct guest_file_mapping *mapping = &g_filemap[i];
        if (mapping->device != device || mapping->inode != inode) continue;
        uint64_t map_lo;
        uint64_t map_hi;
        if (mapping->shared && mapping->emulated) {
            map_lo = mapping->offset;
            map_hi = mapping->offset + mapping->hi - mapping->lo;
        } else if (!mapping->shared && mapping->follow_hi > mapping->follow_lo) {
            map_lo = mapping->offset + mapping->follow_lo;
            map_hi = mapping->offset + mapping->follow_hi;
        } else {
            continue;
        }
        uint64_t lo = offset > map_lo ? offset : map_lo;
        uint64_t hi = end < map_hi ? end : map_hi;
        /* Same ledger caveat as filemap_flush_emulated: a logical-VMA mapping
           does not keep its bytes at the guest address, and its window over the
           vnode never went stale in the first place. */
        if (hi > lo && mapping->shared &&
            !filemap_bytes_live_at_guest_address(mapping->lo + lo - mapping->offset,
                                                 mapping->lo + hi - mapping->offset))
            continue;
        int fd = source_fd >= 0 ? source_fd : filemap_source_fd(mapping);
        if (hi > lo && fd >= 0) {
            ssize_t loaded;
            do {
                loaded =
                    pread(fd, (void *)(uintptr_t)(mapping->lo + lo - mapping->offset), (size_t)(hi - lo), (off_t)lo);
            } while (loaded < 0 && errno == EINTR);
            // A short read intentionally leaves the anonymous-zero tail intact; an error leaves the
            // prior MAP_PRIVATE snapshot intact, matching a failed external refresh.
            if (loaded < 0) continue;
        }
    }
    pthread_mutex_unlock(&g_filemap_lock);
}

static void filemap_written(int fd, uint64_t offset, uint64_t size) {
    struct stat st;
    if (fstat(fd, &st) != 0) return;
    uint64_t device = (uint64_t)st.st_dev, inode = (uint64_t)st.st_ino;
    filemap_written_identity(device, inode, fd, offset, size);
    filemap_publish(HL_LINUX_FILE_EVENT_WRITE, device, inode, offset, size);
}

static void filemap_replay(void) {
    struct filemap_events *events = g_filemap_events;
    if (events == NULL) return;
    pthread_mutex_lock(&g_filemap_replay_lock);
    uint64_t end = atomic_load_explicit(&events->next, memory_order_acquire);
    if (end - g_filemap_cursor > FILEMAP_EVENT_COUNT) {
        /* Never silently manufacture stale MAP_PRIVATE bytes after losing a
           shrink/regrow transition.  Reconstructing which private bytes were
           dirtied before an arbitrary missed shrink is impossible.  Treat
           exhausting this internal journal as a fatal engine resource error,
           before any guest instruction can observe corrupt data. */
        static const char message[] = "[hl-engine] fatal: file mapping mutation journal exhausted\n";
        ssize_t written = write(STDERR_FILENO, message, sizeof(message) - 1);
        if (written < 0 && errno == EINTR) written = write(STDERR_FILENO, message, sizeof(message) - 1);
        if (written < 0) errno = 0; // diagnostics cannot change the unconditional fatal outcome below
        _exit(125);
    }
    while (g_filemap_cursor < end) {
        uint64_t ticket = g_filemap_cursor;
        struct filemap_event *event = &events->event[ticket % FILEMAP_EVENT_COUNT];
        if (atomic_load_explicit(&event->sequence, memory_order_acquire) != ticket + 1) break;
        uint32_t kind = event->kind;
        uint64_t device = event->device, inode = event->inode, first = event->first, second = event->second;
        g_filemap_cursor = ticket + 1;
        if (kind == HL_LINUX_FILE_EVENT_RESIZE)
            filemap_resize_identity(device, inode, first, second);
        else if (kind == HL_LINUX_FILE_EVENT_WRITE)
            filemap_written_identity(device, inode, -1, first, second);
        if (g_file_event_callback != NULL)
            g_file_event_callback(g_file_event_opaque, kind, device, inode, first, second);
    }
    pthread_mutex_unlock(&g_filemap_replay_lock);
}
