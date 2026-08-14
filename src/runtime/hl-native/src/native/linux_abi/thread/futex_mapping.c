// hl/linux_abi -- threads & futex (clone -> pthread; per-thread cpu; futex via condvars).

#include "../../host/range.h"
#include "../page.h" // hl_linux_host_map_granularity
#include "../../host/system.h"
#include "../bus.h"
#include "../logical_vma.h"
#include "../memory_arena.h"

// ---------------- syscalls ----------------
// ---------------- threads & futex ----------------
// fwd: thread trampoline runs the dispatcher
static void run_guest(struct cpu *c);
// The sentry owns descriptor-table identity outside `struct cpu` (which is also
// checkpoint state). Register inheritance before pthread_create so a child
// cannot issue its first syscall before its CLONE_FILES relationship exists.
static int sentry_thread_prepare(struct cpu *child);
static void sentry_thread_cancel(struct cpu *child);
static void sentry_thread_enter(struct cpu *child);
static void sentry_thread_leave(void);

static pthread_mutex_t g_process_owner_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_process_owner_cond = PTHREAD_COND_INITIALIZER;
static struct cpu *g_process_owner_cpu;
static struct cpu *g_process_exec_cpu;
static int g_process_exec_pending;
static int g_process_exec_complete;
static int g_process_exec_status;

static void thread_process_owner_register(struct cpu *owner) {
    pthread_mutex_lock(&g_process_owner_lock);
    g_process_owner_cpu = owner;
    g_process_exec_cpu = NULL;
    g_process_exec_pending = 0;
    g_process_exec_complete = 0;
    g_process_exec_status = 0;
    pthread_mutex_unlock(&g_process_owner_lock);
}

static int thread_exec_owner_handoff(struct cpu *self) {
    pthread_mutex_lock(&g_process_owner_lock);
    int old_tid = self->tid ? self->tid : container_pid();
    if (!hl_target_task_event(self, HL_TASK_EVENT_EXEC_THREAD, 0, (uint64_t)old_tid, 0)) {
        pthread_mutex_unlock(&g_process_owner_lock);
        return 0;
    }
    if (g_process_owner_cpu && self != g_process_owner_cpu) {
        g_process_exec_cpu = self;
        g_process_exec_pending = 1;
        g_process_exec_complete = 0;
        self->tid = 0; // the execing thread becomes the process leader
    }
    pthread_mutex_unlock(&g_process_owner_lock);
    return 1;
}

static void thread_exec_owner_complete(struct cpu *self, int status) {
    pthread_mutex_lock(&g_process_owner_lock);
    if (g_process_exec_pending && g_process_exec_cpu == self) {
        g_process_exec_status = status;
        g_process_exec_complete = 1;
        pthread_cond_broadcast(&g_process_owner_cond);
    }
    pthread_mutex_unlock(&g_process_owner_lock);
}

static int thread_process_owner_wait(struct cpu *owner, int status) {
    pthread_mutex_lock(&g_process_owner_lock);
    while (g_process_owner_cpu == owner && g_process_exec_pending && !g_process_exec_complete)
        pthread_cond_wait(&g_process_owner_cond, &g_process_owner_lock);
    if (g_process_owner_cpu == owner && g_process_exec_complete) status = g_process_exec_status;
    pthread_mutex_unlock(&g_process_owner_lock);
    return status;
}

// ---------------- futex: per-address hashed wait queues ----------------
// W5C: a fixed table of per-address buckets {mutex, condvar, waiter-count}, keyed by
// hash(uaddr). A WAKE touches only the bucket for that address, so a wake never broadcasts waiters
// on unrelated addresses (no cross-address thundering herd). Addresses that collide in a bucket
// share its lock (occasional extra spurious wakeups, never a missed wakeup). Correctness:
//   * Both the WAITER's value-check and the WAKER's broadcast hold the SAME bucket mutex. The
//     mutex's release/acquire is what orders the guest's pre-syscall store to *uaddr ahead of an
//     arriving waiter's load of *uaddr: either the waiter takes the lock first, reads the OLD word
//     and is asleep in cond_wait by the time the waker locks+broadcasts (so it is woken), or the
//     waker takes the lock first, and the waiter then acquires it, observes the NEW word, and bails
//     with EAGAIN instead of sleeping. A lock-free "no sleeper in bucket -> skip" fast path was
//     tried (a seq_cst-fence + seq_cst-atomic Dekker handshake on bucket.waiters) but a seq_cst
//     fence paired with a seq_cst atomic does NOT establish StoreLoad ordering on weak (ARM)
//     memory, so under contention a waiter occasionally slept on a stale word with no later waker
//     -> a lost wakeup (multi-threaded V8/Go shutdowns hung ~1/3 of runs under load). bucket.waiters
//     is now only a PROF diagnostic; correctness no longer depends on it.
//   * FUTEX_WAIT may return 0 spuriously (per spec); the guest re-checks the word and re-waits.
#define FUTEX_NBUCKET 256
// Per-bucket, per-address parked-waiter tally (under b->m): a real FUTEX_WAKE returns the NUMBER of
// waiters it actually woke, not the requested count. LTP's tst_checkpoint_wake() (and any code that
// sums the return value) loops `waked += futex(WAKE, INT_MAX)` until it equals nr_wake; if WAKE returns
// the requested INT_MAX instead of the true 1, it never matches and times out -> the fork04 TBROK
// (`tst_checkpoint_wake() ... ETIMEDOUT`). We record how many waiters are parked on EACH distinct uaddr
// in a bucket so WAKE can report min(val, parked-on-uaddr). Addresses that hash-collide into one bucket
// occupy separate slots; if a bucket ever has more distinct waited-on addresses than slots, it goes
// `imprecise` (WAKE falls back to the bucket-aggregate `waiters`) until it fully drains -- a bounded,
// wake-count-only degradation that never drops a wakeup (the broadcast still wakes everyone).
#define FUTEX_ASLOTS 16
#define FUTEX_WSLOTS 128

struct futex_bucket {
    pthread_mutex_t m;
    pthread_cond_t c;
    _Atomic int waiters;           // aggregate parked count in this bucket (PROF + imprecise fallback)
    uintptr_t saddr[FUTEX_ASLOTS]; // distinct uaddrs with >=1 parked waiter (0 == free slot)
    uint32_t scnt[FUTEX_ASLOTS];   // parked-waiter count for saddr[i]
    uint32_t sbits[FUTEX_ASLOTS];  // OR of the FUTEX_WAIT_BITSET masks parked on saddr[i] (plain WAIT = ~0)
    uintptr_t waddr[FUTEX_WSLOTS]; // individual waiters, for exact FUTEX_WAKE(n) selection
    uint32_t wbits[FUTEX_WSLOTS];  // this waiter's FUTEX_WAIT_BITSET mask
    uint8_t wgrant[FUTEX_WSLOTS];  // selected by a wake; consumed by that waiter
    uint16_t wcursor;              // next waiter slot considered by FUTEX_WAKE (queue progress)
    int imprecise;                 // slots overflowed while waiters were parked -> WAKE count approximate
};

// pthread_cond_broadcast is only the transport that gets sleepers runnable;
// Linux FUTEX_WAKE(n) selects at most n waiters.  Keep that selection in the
// shared bucket so non-selected sleepers re-park instead of spuriously
// returning success.  Fixed slots are process-shared and therefore work for
// futexes inherited across a real host fork (unlike linked host pointers).
static int fbk_wait_register(struct futex_bucket *b, uintptr_t address, uint32_t bits) {
    for (int i = 0; i < FUTEX_WSLOTS; ++i)
        if (!b->waddr[i]) {
            b->waddr[i] = address;
            b->wbits[i] = bits;
            b->wgrant[i] = 0;
            return i;
        }
    return -1;
}

static void fbk_wait_unregister(struct futex_bucket *b, int slot) {
    if (slot < 0) return;
    b->wgrant[slot] = 0;
    b->wbits[slot] = 0;
    b->waddr[slot] = 0;
}

static int fbk_wait_grant(struct futex_bucket *b, uintptr_t address, int count, uint32_t mask, int *has_registered) {
    int granted = 0;
    *has_registered = 0;
    int start = b->wcursor % FUTEX_WSLOTS;
    for (int offset = 0; offset < FUTEX_WSLOTS; ++offset) {
        int i = (start + offset) % FUTEX_WSLOTS;
        if (b->waddr[i] != address) continue;
        *has_registered = 1;
        if (b->wgrant[i] || !(b->wbits[i] & mask) || granted >= count) continue;
        b->wgrant[i] = 1;
        b->wcursor = (uint16_t)((i + 1) % FUTEX_WSLOTS);
        granted++;
    }
    return granted;
}

// Called under b->m. Register/unregister one parked waiter on `a`, and report the parked count for `a`.
// `bits` is the waiter's FUTEX_WAIT_BITSET mask (~0u for a plain FUTEX_WAIT); it is OR'd into the address's
// aggregate so a FUTEX_WAKE_BITSET can tell whether any waiter here can match its wake mask. The aggregate
// over-approximates (bits are only cleared when the address fully drains), which can only cause an extra --
// always-legal -- spurious wakeup, never a missed one.
static void fbk_park(struct futex_bucket *b, uintptr_t a, uint32_t bits) {
    int freeslot = -1;
    for (int i = 0; i < FUTEX_ASLOTS; i++) {
        if (b->scnt[i] && b->saddr[i] == a) {
            b->scnt[i]++;
            b->sbits[i] |= bits;
            return;
        }
        if (freeslot < 0 && !b->scnt[i]) freeslot = i;
    }
    if (freeslot >= 0) {
        b->saddr[freeslot] = a;
        b->scnt[freeslot] = 1;
        b->sbits[freeslot] = bits;
        return;
    }
    b->imprecise = 1; // no free slot: this bucket's WAKE counts are approximate until it drains
}

static void fbk_unpark(struct futex_bucket *b, uintptr_t a) {
    for (int i = 0; i < FUTEX_ASLOTS; i++)
        if (b->scnt[i] && b->saddr[i] == a) {
            if (--b->scnt[i] == 0) {
                b->saddr[i] = 0;
                b->sbits[i] = 0; // address drained -> reset the aggregate wait-mask
            }
            return;
        }
}

// Under b->m: does any waiter parked on `a` have a bitset overlapping `mask`? A plain FUTEX_WAKE passes ~0u
// (always matches). When the bucket overflowed (imprecise), we cannot trust the per-address aggregate, so
// conservatively report a match (broadcast + let waiters re-check) rather than risk missing a wakeup.
static int fbk_match(struct futex_bucket *b, uintptr_t a, uint32_t mask) {
    if (b->imprecise) return 1;
    for (int i = 0; i < FUTEX_ASLOTS; i++)
        if (b->scnt[i] && b->saddr[i] == a) return (b->sbits[i] & mask) != 0;
    return 0;
}

static int fbk_parked(struct futex_bucket *b, uintptr_t a) {
    if (b->imprecise) return atomic_load_explicit(&b->waiters, memory_order_relaxed);
    for (int i = 0; i < FUTEX_ASLOTS; i++)
        if (b->scnt[i] && b->saddr[i] == a) return (int)b->scnt[i];
    return 0;
}
// _xproc-futex-fork_: the bucket table lives in a MAP_SHARED anonymous region whose mutex/condvar are
// PTHREAD_PROCESS_SHARED, so a FUTEX_WAKE in one process matches a FUTEX_WAIT in another across hl's
// fork() -- e.g. a glibc process-shared (named/unnamed-on-shm) semaphore where the child sem_post()s
// and the parent sem_wait()s. hl's fork() is a real host fork(): the child inherits the identical guest
// address space, so a shared-memory futex word resolves to the SAME host address in parent and child
// and both hash to the same bucket, while the underlying MAP_SHARED guest page is one physical page.
// The table is created ONCE at engine startup (constructor, before any guest fork) so every forked
// worker inherits the same physical buckets. The lock-free no-sleeper WAKE fast path is unchanged --
// only the slow path (a real sleeper exists) touches the cross-process mutex/condvar. FUTEX_PRIVATE_FLAG
// operations use a separate process-private table below; non-private operations retain this shared table.
static struct futex_bucket *g_fbk;
static struct futex_bucket *g_fbk_private;
static __thread struct futex_bucket *g_fbk_active;

static struct futex_bucket *futex_table_alloc(const hl_host_services *host, int shared) {
    size_t sz = sizeof(struct futex_bucket) * FUTEX_NBUCKET;
    void *mem = NULL;
    if (hl_linux_memory_create(host, sz, shared ? HL_HOST_MEMORY_SHARED : HL_HOST_MEMORY_PRIVATE, &mem) != HL_STATUS_OK)
        abort();
    struct futex_bucket *t = (struct futex_bucket *)mem;
    pthread_mutexattr_t ma;
    pthread_condattr_t ca;
    pthread_mutexattr_init(&ma);
    pthread_condattr_init(&ca);
    pthread_mutexattr_setpshared(&ma, shared ? PTHREAD_PROCESS_SHARED : PTHREAD_PROCESS_PRIVATE);
    pthread_condattr_setpshared(&ca, shared ? PTHREAD_PROCESS_SHARED : PTHREAD_PROCESS_PRIVATE);
    for (int i = 0; i < FUTEX_NBUCKET; i++) {
        pthread_mutex_init(&t[i].m, &ma);
        pthread_cond_init(&t[i].c, &ca);
        atomic_store_explicit(&t[i].waiters, 0, memory_order_relaxed);
        t[i].wcursor = 0;
    }
    pthread_mutexattr_destroy(&ma);
    pthread_condattr_destroy(&ca);
    return t;
}

static void futex_table_init(const hl_host_services *host) {
    if (g_fbk) return;
    g_fbk = futex_table_alloc(host, 1);
    g_fbk_private = futex_table_alloc(host, 0);
}

// A fork child inherits the private table's bytes, including locks that may have been held by a vanished
// peer thread. Rebuild only that table in place; the shared table must retain its cross-process waiters.
static void futex_private_table_after_fork(void) {
    for (int i = 0; i < FUTEX_NBUCKET; i++) {
        struct futex_bucket *b = &g_fbk_private[i];
        pthread_mutex_init(&b->m, NULL);
        pthread_cond_init(&b->c, NULL);
        atomic_store_explicit(&b->waiters, 0, memory_order_relaxed);
        memset(b->saddr, 0, sizeof b->saddr);
        memset(b->scnt, 0, sizeof b->scnt);
        memset(b->sbits, 0, sizeof b->sbits);
        memset(b->waddr, 0, sizeof b->waddr);
        memset(b->wbits, 0, sizeof b->wbits);
        memset(b->wgrant, 0, sizeof b->wgrant);
        b->wcursor = 0;
        b->imprecise = 0;
    }
    g_fbk_active = g_fbk_private;
}

// ===================== shared-memory futex key (Linux "shared" futex semantics) =================
// hl hashes a futex bucket by the WORD's host virtual address. That is exactly Linux's PRIVATE futex key
// (mm + address) and is correct for anon/private words -- including a fork-inherited MAP_SHARED page, which
// lands at the SAME VA in parent and child. But a file-backed MAP_SHARED object (memfd, shm) is mapped
// INDEPENDENTLY by each peer: cooperating processes may map command-buffer shared memory at DIFFERENT
// addresses, so the SAME physical futex word has a different VA in each. Linux keys such a word by the
// SHARED object identity (inode + page offset), so a FUTEX_WAKE through one mapping reaches a FUTEX_WAIT
// parked through another. hl's VA-only key put the two in different buckets and LOST the wake -- the
// observed failure: one process's command-buffer flush never woke its peer, so page content was
// never rasterized. (A fork-inherited anon MAP_SHARED page keeps the VA key: same VA in both processes.)
//
// Fix: record every file-backed MAP_SHARED region {host VA range -> (st_dev, st_ino, file offset)} at mmap
// time (mem.c), and canonicalise a futex word in such a region to a stable token derived from that
// identity. futex_key() returns this token for a shared word and the plain VA otherwise, and is used BOTH
// for the bucket hash AND for the per-address parked-waiter slot (fbk_park/unpark/match/parked), so a
// waiter and a cross-mapping waker agree on the bucket AND the slot. A token that happens to collide with a
// real VA (or another shared word) only causes a spurious wake -- the guest re-checks its word and re-waits
// -- never a missed one. The registry is process-private (each process maps its own VAs to the same global
// (dev,ino,off), so keys still match across processes); a zero-entry fast path keeps non-shared futexes
// (every private/anon word, the overwhelming majority) byte-identical and lock-free.
#define FSHKEY_MAX 4096

static struct {
    uint64_t lo, hi;   // host VA range [lo, hi) of this mapping
    uint64_t dev, ino; // backing object identity (fstat)
    uint64_t foff;     // file offset mapped at `lo`
} g_shkey[FSHKEY_MAX];

static int g_shkey_n;
static _Atomic int g_nshkey; // 0 => futex_key is identity (lock-free fast path, no registry scan)
static pthread_mutex_t g_shkey_m = PTHREAD_MUTEX_INITIALIZER;

// Canonical futex key for uaddr: a shared-object token for a file-backed MAP_SHARED word, else the VA.
static uintptr_t futex_key(const void *uaddr) {
    if (atomic_load_explicit(&g_nshkey, memory_order_acquire) == 0) return (uintptr_t)uaddr;
    uint64_t v = (uint64_t)(uintptr_t)uaddr;
    uintptr_t key = (uintptr_t)uaddr;
    pthread_mutex_lock(&g_shkey_m);
    for (int i = 0; i < g_shkey_n; i++) {
        if (v >= g_shkey[i].lo && v < g_shkey[i].hi) {
            uint64_t off = g_shkey[i].foff + (v - g_shkey[i].lo);
            uint64_t h = (g_shkey[i].ino + 0x9E3779B97F4A7C15ull) * 1099511628211ull;
            h ^= (g_shkey[i].dev + 0x100000001B3ull) * 2654435761ull;
            h ^= off * 0xD6E8FEB86659FD93ull;
            h ^= h >> 29;
            key = (uintptr_t)(h | 1); // never 0 (0 is the free-slot sentinel in fbk_park)
            break;
        }
    }
    pthread_mutex_unlock(&g_shkey_m);
    return key;
}

// Record a file-backed MAP_SHARED mapping so its futex words canonicalise to the shared object identity.
// Called from mem.c after a successful mmap; a no-op (fast-path gate stays 0) until the first such map.
static void futex_shared_register(uint64_t base, uint64_t len, int fd, uint64_t foff) {
    struct stat st;
    if (len == 0 || fstat(fd, &st) != 0) return;
    pthread_mutex_lock(&g_shkey_m);
    for (int i = 0; i < g_shkey_n;) { // drop any stale entry fully covered by this range (a remap)
        if (g_shkey[i].lo >= base && g_shkey[i].hi <= base + len)
            g_shkey[i] = g_shkey[--g_shkey_n];
        else
            i++;
    }
    if (g_shkey_n < FSHKEY_MAX) {
        g_shkey[g_shkey_n].lo = base;
        g_shkey[g_shkey_n].hi = base + len;
        g_shkey[g_shkey_n].dev = (uint64_t)st.st_dev;
        g_shkey[g_shkey_n].ino = (uint64_t)st.st_ino;
        g_shkey[g_shkey_n].foff = foff;
        g_shkey_n++;
    }
    atomic_store_explicit(&g_nshkey, g_shkey_n, memory_order_release);
    pthread_mutex_unlock(&g_shkey_m);
}

// Trim/drop shared-key entries against a host range [ustart,uend) that munmap actually released (mirrors
// anon_split_unmap). A surviving head keeps its identity; a surviving tail advances foff for the raised base.
static void futex_shared_unmap(uint64_t ustart, uint64_t uend) {
    if (atomic_load_explicit(&g_nshkey, memory_order_acquire) == 0) return;
    pthread_mutex_lock(&g_shkey_m);
    for (int i = 0; i < g_shkey_n;) {
        uint64_t base = g_shkey[i].lo, end = g_shkey[i].hi;
        if (ustart >= end || uend <= base) {
            i++;
            continue;
        }
        int keep_head = base < ustart, keep_tail = uend < end;
        uint64_t dev = g_shkey[i].dev, ino = g_shkey[i].ino, foff = g_shkey[i].foff;
        if (!keep_head && !keep_tail) {
            g_shkey[i] = g_shkey[--g_shkey_n];
            continue;
        }
        if (keep_head) {
            g_shkey[i].hi = ustart; // lo/foff unchanged; trim to the surviving head
        } else {                    // keep_tail only: base rises -> advance foff
            g_shkey[i].foff = foff + (uend - base);
            g_shkey[i].lo = uend;
        }
        if (keep_head && keep_tail && g_shkey_n < FSHKEY_MAX) { // middle unmap -> tail becomes a 2nd entry
            g_shkey[g_shkey_n].lo = uend;
            g_shkey[g_shkey_n].hi = end;
            g_shkey[g_shkey_n].dev = dev;
            g_shkey[g_shkey_n].ino = ino;
            g_shkey[g_shkey_n].foff = foff + (uend - base);
            g_shkey_n++;
        }
        i++;
    }
    atomic_store_explicit(&g_nshkey, g_shkey_n, memory_order_release);
    pthread_mutex_unlock(&g_shkey_m);
}

static inline struct futex_bucket *fbk_of(const void *uaddr) {
    uint32_t h = (uint32_t)((futex_key(uaddr) >> 2) * 2654435761u) & (FUTEX_NBUCKET - 1);
    return &(g_fbk_active ? g_fbk_active : g_fbk)[h];
}

// PROF: fast (no-lock) wakes, slow (locked) wakes, eagain pre-checks
static uint64_t g_futex_wake_fast, g_futex_wake_slow, g_futex_wait_n;

// ===================== non-PIE image placement, per host ========================================
// Where a non-PIE ET_EXEC is mapped is a HOST property. It is the entire reason the next block exists,
// and the reason the next block is dormant here.
//   Linux: nothing owns the low 4 GB (vm.mmap_min_addr is 64 KiB, the link vaddr is 0x400000 for both
//     guest ISAs, and the engine itself is PIE at ET_DYN_BASE), so the loader maps the image AT its link
//     address. base == basepage, so the bias is 0, g_nonpie_lo stays 0, and one image byte has ONE name.
//     Every fold below and every workaround built on it is then inert BY CONSTRUCTION, not by care.
//   macOS: __PAGEZERO reserves the low 4 GB and no MAP_FIXED can get in, so the image must go HIGH and
//     the two coordinate systems of the next block are unavoidable.
// The machinery stays compiled in on both hosts: a checkpoint image records g_nonpie_lo/hi/bias and a
// restore replays the placement its capture used, so a Linux engine must still be able to run folded.
#if defined(__linux__)
#define HL_NONPIE_LINK_PLACEMENT 1 // loaders (linux_abi/x86.c, linux_abi/elf.c) place ET_EXEC at p_vaddr
#else
#define HL_NONPIE_LINK_PLACEMENT 0
#endif

// Reserve [basepage, basepage+span) EXACTLY, or report failure by returning 0 with *mapping untouched.
// MAP_FIXED_NOREPLACE, never MAP_FIXED: if anything already owns the link range -- a host below
// vm.mmap_min_addr, an execve whose predecessor image has not been released, a kernel too old to honour
// NOREPLACE -- the caller falls back to the biased placement rather than clobbering a live mapping.
static uint64_t nonpie_place_at_link_address(uint64_t basepage, uint64_t span, hl_host_memory_mapping *mapping) {
    if (!HL_NONPIE_LINK_PLACEMENT || basepage == 0 || span == 0) return 0;
    const hl_host_services *host = effective_host_services();
    hl_host_memory_mapping placed = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(placed), 0, 0, 0, 0};
    hl_host_result result =
        host->memory->map_anonymous(host->context, basepage, span, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE,
                                    HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED_NOREPLACE, &placed);
    if (result.status != HL_STATUS_OK) return 0;
    if (placed.address != basepage) { // pre-4.17 kernel: NOREPLACE degraded to a hint
        (void)host->memory->release(host->context, placed.handle);
        return 0;
    }
    *mapping = placed;
    return basepage;
}

static void nonpie_report_forced_displacement(void) {
    static const char message[] = "hl-test-displaced-et-exec: displaced\n";
    const hl_host_services *host = effective_host_services();
    if (host == NULL || (host->capabilities & HL_HOST_CAP_LOG) == 0 || host->log == NULL || host->log->emit == NULL)
        return;
    host->log->emit(host->context, 0, message, sizeof(message) - 1);
}

// ===================== non-PIE coordinates: the one rule ========================================
#include "../../bridge/address_projection.h"
// When the image IS folded (macOS, or a restored image captured folded) it is mapped HIGH at
// +g_nonpie_bias but carries no dynamic relocations, so every address BAKED INTO IT stays at the LOW
// link vaddr. One image byte therefore has two names, and which one is correct is not a judgement call:
//
//   GUEST (low) is canonical. Anything the guest can name, is handed, or asks about -- a baked
//   pointer, a syscall argument, AT_PHDR/AT_ENTRY, a sigaltstack ss_sp, a /proc/self/maps row, a
//   protection-registry key -- is the LOW value, because the fold is the loader's private business
//   and nothing inside the guest knows it happened.
//
//   HOST (high) is STORAGE, and the only thing it is ever right for is one host dereference. Fold
//   at the instant of the dereference and throw the result away; never store it, never return it,
//   never key a registry on it.
//
// Eight defects on this branch were one half of that rule missed: rip-relative LEA and mov r64,imm32
// materialising the wrong half; vfs.c and maps_phdr_segs dereferencing an unfolded AT_PHDR; a queued
// signal delivered to an unfolded handler; the protection registries keyed HOST by the loader and GUEST
// by mprotect; sigaltstack's ss_sp; a fault's si_addr handed back as storage.
// nonpie_fold is total. nonpie_unfold is only for an address that is storage by construction -- a
// hardware fault address, or a caller that already folded -- and is unambiguous only because the image
// occupies [lo+bias,hi+bias), where no other guest mapping can be. Both inert for PIE (g_nonpie_lo == 0).
// Tentatively defined here: thread.c is the earliest linux_abi member of the unity TU. dispatch.c's
// nonpie_p is nonpie_fold under its historical name.
static uint64_t g_nonpie_lo, g_nonpie_hi, g_nonpie_bias;

static inline uint64_t nonpie_fold(uint64_t guest) {
    if (!g_nonpie_lo) return guest;
    const hl_native_address_projection p = {HL_NATIVE_ADDRESS_PROJECTION_ABI,
                                            (uint32_t)sizeof(p),
                                            HL_NATIVE_ADDRESS_PROJECTION_DISPLACED,
                                            0,
                                            g_nonpie_lo,
                                            g_nonpie_hi,
                                            g_nonpie_bias};
    return hl_native_address_projection_storage_unchecked(&p, guest);
}

static inline uint64_t nonpie_unfold(uint64_t storage) {
    if (!g_nonpie_lo) return storage;
    const hl_native_address_projection p = {HL_NATIVE_ADDRESS_PROJECTION_ABI,
                                            (uint32_t)sizeof(p),
                                            HL_NATIVE_ADDRESS_PROJECTION_DISPLACED,
                                            0,
                                            g_nonpie_lo,
                                            g_nonpie_hi,
                                            g_nonpie_bias};
    return hl_native_address_projection_guest_unchecked(&p, storage);
}

// ===================== guest PROT_NONE region registry ==========================================
// hl maps every guest anon page R+W on the host (case 222 ORs in PROT_READ|WRITE) so that a later
// mprotect-to-writable -- which hl no-ops, since the JIT never enforces guest page protection -- is
// already in effect. A consequence: a guest mapping the guest genuinely made INACCESSIBLE (mmap
// PROT_NONE, e.g. LTP's tst_get_bad_addr, a malloc-arena guard page, a Go/V8 reservation) is still
// PHYSICALLY readable, so host_range_mapped's page probe wrongly reports it mapped -- and a syscall
// whose user buffer lands there returns success instead of -EFAULT (LTP sched_getaffinity01's EFAULT
// case). Track the guest-requested PROT_NONE ranges so host_range_mapped can fault them exactly as the
// kernel's copy_to_user would. Readers use the generation as a seqlock while mmap/mprotect/munmap update it.
#define GNA_MAX 512

static struct {
    uint64_t lo, hi;
} g_gna[GNA_MAX];

static int g_ngna;
static _Atomic uint64_t g_gna_filter_first = UINT64_MAX;
static _Atomic uint64_t g_gna_filter_last;
static void gna_clear(uint64_t lo, uint64_t hi);
static void gna_clear_raw(uint64_t lo, uint64_t hi);
#define GNA_NEGATIVE_N 1024u
static _Atomic uint64_t g_gna_generation = 2;
static atomic_flag g_gna_writer = ATOMIC_FLAG_INIT;
static _Thread_local uint64_t g_gna_negative_page[GNA_NEGATIVE_N];
static _Thread_local uint64_t g_gna_negative_generation[GNA_NEGATIVE_N];
