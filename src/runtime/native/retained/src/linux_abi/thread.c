// hl/linux_abi -- threads & futex (clone -> pthread; per-thread cpu; futex via condvars).

#include "../host/range.h"
#include "page.h" // hl_linux_host_map_granularity
#include "../host/system.h"
#include "bus.h"
#include "logical_vma.h"
#include "shared.h"

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

// ===================== non-PIE coordinates: the one rule ========================================
#include "address_projection.h"
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
    const hl_native_address_projection p = {HL_NATIVE_ADDRESS_PROJECTION_ABI, (uint32_t)sizeof(p),
                                             HL_NATIVE_ADDRESS_PROJECTION_DISPLACED, 0, g_nonpie_lo, g_nonpie_hi,
                                             g_nonpie_bias};
    return hl_native_address_projection_storage_unchecked(&p, guest);
}

static inline uint64_t nonpie_unfold(uint64_t storage) {
    if (!g_nonpie_lo) return storage;
    const hl_native_address_projection p = {HL_NATIVE_ADDRESS_PROJECTION_ABI, (uint32_t)sizeof(p),
                                             HL_NATIVE_ADDRESS_PROJECTION_DISPLACED, 0, g_nonpie_lo, g_nonpie_hi,
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
static void gna_clear(uint64_t lo, uint64_t hi);
static void gna_clear_raw(uint64_t lo, uint64_t hi);
#define GNA_NEGATIVE_N 1024u
static _Atomic uint64_t g_gna_generation = 2;
static atomic_flag g_gna_writer = ATOMIC_FLAG_INIT;
static _Thread_local uint64_t g_gna_negative_page[GNA_NEGATIVE_N];
static _Thread_local uint64_t g_gna_negative_generation[GNA_NEGATIVE_N];

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
        if (shared && emulated) atomic_store_explicit(&g_filemap_emulated_shared, 1, memory_order_release);
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

static void gbus_lock(void) {
    while (atomic_flag_test_and_set_explicit(&g_bus_lock, memory_order_acquire))
        sched_yield();
}

static void gbus_unlock(void) {
    atomic_flag_clear_explicit(&g_bus_lock, memory_order_release);
}

static void gbus_filter_rebuild_locked(void) {
    uint64_t lo = UINT64_MAX, hi = 0;
    if (g_bus_fail_closed || g_bus_prepares != 0) {
        lo = 0;
        hi = UINT64_MAX;
    } else {
        for (int index = 0; index < g_ngbus; ++index) {
            if (g_gbus[index].lo < lo) lo = g_gbus[index].lo;
            if (g_gbus[index].hi > hi) hi = g_gbus[index].hi;
        }
    }
    atomic_store_explicit(&g_bus_filter_lo, lo, memory_order_relaxed);
    atomic_store_explicit(&g_bus_filter_hi, hi, memory_order_release);
}

static unsigned gbus_page_hash(uint64_t page) {
    return (unsigned)page & (BUS_FILTER_BITS - 1u);
}

static void gbus_page_mark_locked(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    uint64_t first = lo >> 12;
    uint64_t last = (hi - 1) >> 12;
    /* Mark the preceding page too: a single instruction may begin there and
       cross into the first BUS page.  Guards then need only hash their start. */
    if (first != 0) first--;
    if (last - first >= BUS_FILTER_BITS) {
        for (unsigned i = 0; i < BUS_FILTER_WORDS; ++i)
            atomic_store_explicit(&g_bus_page_filter[i], UINT64_MAX, memory_order_release);
        return;
    }
    for (uint64_t page = first;; ++page) {
        unsigned bit = gbus_page_hash(page);
        atomic_fetch_or_explicit(&g_bus_page_filter[bit >> 6], UINT64_C(1) << (bit & 63u), memory_order_release);
        if (page == last) break;
    }
}

static void gbus_page_reset_locked(void) {
    for (unsigned i = 0; i < BUS_FILTER_WORDS; ++i)
        atomic_store_explicit(&g_bus_page_filter[i], 0, memory_order_release);
}

static void gbus_page_rebuild_locked(void) {
    gbus_page_reset_locked();
    if (g_bus_fail_closed) {
        for (unsigned i = 0; i < BUS_FILTER_WORDS; ++i)
            atomic_store_explicit(&g_bus_page_filter[i], UINT64_MAX, memory_order_release);
        return;
    }
    for (int index = 0; index < g_ngbus; ++index)
        gbus_page_mark_locked(g_gbus[index].lo, g_gbus[index].hi);
}

static void gbus_atfork_prepare(void) {
    pthread_mutex_lock(&g_bus_transition);
    gbus_lock();
}

static void gbus_atfork_parent(void) {
    gbus_unlock();
    pthread_mutex_unlock(&g_bus_transition);
}

static void gbus_atfork_child(void) {
    gbus_unlock();
    pthread_mutex_unlock(&g_bus_transition);
}

static void gbus_atfork_install(void) {
    (void)pthread_atfork(gbus_atfork_prepare, gbus_atfork_parent, gbus_atfork_child);
}

static void gbus_notify(uint64_t generation, int active) {
    gbus_lock();
    hl_linux_bus_change_fn callback = g_bus_callback;
    void *opaque = g_bus_callback_opaque;
    gbus_unlock();
    if (callback != NULL) callback(opaque, generation, active);
}

static void gbus_prepare(void) {
    (void)pthread_once(&g_bus_atfork_once, gbus_atfork_install);
    pthread_mutex_lock(&g_bus_transition);
    gbus_lock();
    int was_active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    uint64_t generation = !was_active ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                      : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    gbus_unlock();
    /* On first activation guarded code does not exist yet.  Complete the
       synchronous STW while the old empty ledger remains queryable; forcing
       queries to wait on a prepare here deadlocks a peer inside its guard
       before that peer can acknowledge the STW.  The caller has not changed
       the host mapping yet, so the old empty answer remains correct. */
    if (!was_active) gbus_notify(generation, 1);
    if (g_bus_transition_begin != NULL) g_bus_transition_begin(g_bus_transition_opaque);
    gbus_lock();
    /* From this point through host mapping publication and ledger commit,
       already-guarded code must use the precise transition path. */
    atomic_store_explicit(&g_bus_filter_force, 3, memory_order_release);
    if (g_bus_prepares != UINT32_MAX)
        g_bus_prepares++;
    else
        g_bus_fail_closed = 1;
    gbus_unlock();
    /* Keep the transition lock through host publication and commit/release. This serializes
       concurrent mapping transactions and prevents fork from inheriting an orphan prepare token. */
}

static void gbus_prepare_release(void) {
    gbus_lock();
    if (g_bus_prepares != 0) g_bus_prepares--;
    /* force remains set until publication below, so no translated guard can
       observe the temporary zeroes.  Rebuilding on every completed mapping
       transaction prevents a long-lived range plus distinct-page churn from
       monotonically saturating the fast rejection filter. */
    gbus_page_rebuild_locked();
    gbus_filter_rebuild_locked();
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    uint64_t generation = !active ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    gbus_unlock();
    atomic_store_explicit(&g_bus_filter_force, active ? 1 : 0, memory_order_release);
    if (!active) gbus_notify(generation, 0);
    if (g_bus_transition_end != NULL) g_bus_transition_end(g_bus_transition_opaque);
    pthread_mutex_unlock(&g_bus_transition);
}

/* A host MAP_FIXED replacement must not run concurrently with a translated
   peer accessing the replaced range.  This is only a mapping transaction: it
   deliberately does not activate BUS instrumentation or change the ledger. */
static void gbus_mapping_transition_lock(void) {
    (void)pthread_once(&g_bus_atfork_once, gbus_atfork_install);
    pthread_mutex_lock(&g_bus_transition);
}

static void gbus_mapping_stw_begin(void) {
    if (g_bus_transition_begin != NULL) g_bus_transition_begin(g_bus_transition_opaque);
}

static void gbus_mapping_stw_end(void) {
    if (g_bus_transition_end != NULL) g_bus_transition_end(g_bus_transition_opaque);
}

static void gbus_mapping_transition_unlock(void) {
    pthread_mutex_unlock(&g_bus_transition);
}

static void gbus_mapping_prepare(void) {
    gbus_mapping_transition_lock();
    gbus_mapping_stw_begin();
}

static void gbus_mapping_prepare_release(void) {
    gbus_mapping_stw_end();
    gbus_mapping_transition_unlock();
}

int hl_linux_bus_transition_begin(hl_linux_bus_transition *transition) {
    if (transition == NULL || transition->held != 0) return -1;
    gbus_prepare();
    transition->generation = atomic_load_explicit(&g_bus_generation, memory_order_acquire);
    transition->held = 1;
    return 0;
}

int hl_linux_bus_transition_add(hl_linux_bus_transition *transition, uint64_t lo, uint64_t hi) {
    if (transition == NULL || transition->held == 0) return -1;
    return gbus_add(lo, hi);
}

void hl_linux_bus_transition_clear(hl_linux_bus_transition *transition, uint64_t lo, uint64_t hi) {
    if (transition != NULL && transition->held != 0) gbus_clear(lo, hi);
}

void hl_linux_bus_transition_end(hl_linux_bus_transition *transition) {
    if (transition == NULL || transition->held == 0) return;
    transition->held = 0;
    gbus_prepare_release();
    transition->generation = atomic_load_explicit(&g_bus_generation, memory_order_acquire);
}

void hl_linux_bus_set_change_callback(hl_linux_bus_change_fn callback, void *opaque) {
    gbus_lock();
    g_bus_callback_opaque = opaque;
    g_bus_callback = callback;
    uint64_t generation = atomic_load_explicit(&g_bus_generation, memory_order_acquire);
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    gbus_unlock();
    if (callback != NULL) callback(opaque, generation, active);
}

void hl_linux_bus_set_transition_callbacks(hl_linux_bus_transition_fn begin, hl_linux_bus_transition_fn end,
                                           void *opaque) {
    pthread_mutex_lock(&g_bus_transition);
    g_bus_transition_begin = begin;
    g_bus_transition_end = end;
    g_bus_transition_opaque = opaque;
    pthread_mutex_unlock(&g_bus_transition);
}

static int gbus_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return 0;
    (void)pthread_once(&g_bus_atfork_once, gbus_atfork_install);
    gbus_lock();
    (void)gbus_clear_locked(lo, hi);
    int ok = g_ngbus < GNA_MAX;
    if (ok)
        g_gbus[g_ngbus++] = (struct guest_bus_range){lo, hi};
    else
        g_bus_fail_closed = 1;
    gbus_page_mark_locked(lo, hi);
    gbus_filter_rebuild_locked();
    uint64_t generation = atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1;
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    atomic_store_explicit(&g_bus_filter_force, g_bus_prepares != 0 ? 3 : (active ? 1 : 0), memory_order_release);
    gbus_unlock();
    gbus_notify(generation, active);
    return ok ? 0 : -1;
}

static void gbus_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gbus_lock();
    int changed = gbus_clear_locked(lo, hi);
    if (changed && g_ngbus == 0 && !g_bus_fail_closed) gbus_page_reset_locked();
    if (changed) gbus_filter_rebuild_locked();
    uint64_t generation = changed ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    if (changed)
        atomic_store_explicit(&g_bus_filter_force, g_bus_prepares != 0 ? 3 : (active ? 1 : 0), memory_order_release);
    gbus_unlock();
    if (changed) gbus_notify(generation, active);
}

uint64_t hl_linux_bus_fault(uint64_t address, uint64_t length) {
    if (length == 0) return 0;
    if (address > UINT64_MAX - length) return address != 0 ? address : 1;
    uint64_t end = address + length;
    if (atomic_load_explicit(&g_bus_filter_force, memory_order_acquire) != 3) {
        uint64_t lo = atomic_load_explicit(&g_bus_filter_lo, memory_order_relaxed);
        uint64_t hi = atomic_load_explicit(&g_bus_filter_hi, memory_order_acquire);
        if (address >= hi || end <= lo) return 0;
    }
retry:
    gbus_lock();
    /* A prepare spans activation, host mapping publication, and precise-ledger
       commit.  Wait out that short transaction rather than treating every
       address as BUS: unrelated translated threads must not receive a
       synchronous SIGBUS merely because a mapper is between publication and
       ledger insertion. */
    if (g_bus_prepares != 0) {
        gbus_unlock();
        sched_yield();
        goto retry;
    }
    if (g_bus_fail_closed) {
        gbus_unlock();
        return address != 0 ? address : 1;
    }
    for (int index = 0; index < g_ngbus; ++index)
        if (address < g_gbus[index].hi && end > g_gbus[index].lo) {
            uint64_t fault = address > g_gbus[index].lo ? address : g_gbus[index].lo;
            gbus_unlock();
            return fault != 0 ? fault : 1;
        }
    gbus_unlock();
    return 0;
}

int hl_linux_bus_hit(uint64_t address, uint64_t length) {
    return hl_linux_bus_fault(address, length) != 0;
}

uint64_t hl_linux_bus_generation(void) {
    return atomic_load_explicit(&g_bus_generation, memory_order_acquire);
}

int hl_linux_bus_active(void) {
    gbus_lock();
    int active = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    gbus_unlock();
    return active;
}

static void gna_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gna_writer_lock();
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_acq_rel);
    gna_clear_raw(lo, hi); // coalesce inside the same odd-generation transaction
    int count = __atomic_load_n(&g_ngna, __ATOMIC_RELAXED);
    if (count < GNA_MAX) {
        __atomic_store_n(&g_gna[count].lo, lo, __ATOMIC_RELAXED);
        __atomic_store_n(&g_gna[count].hi, hi, __ATOMIC_RELAXED);
        __atomic_store_n(&g_ngna, count + 1, __ATOMIC_RELEASE);
    }
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_release);
    gna_writer_unlock();
}

// Remove [lo,hi) from the set (access granted, or the range unmapped/re-mapped), splitting any interval
// that straddles the boundary so a partial grant (mprotect of a sub-range of a big PROT_NONE reservation)
// keeps the still-inaccessible remainder tracked.
static void gna_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gna_writer_lock();
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_acq_rel);
    gna_clear_raw(lo, hi);
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_release);
    gna_writer_unlock();
}

static void gna_clear_raw(uint64_t lo, uint64_t hi) {
    int count = __atomic_load_n(&g_ngna, __ATOMIC_RELAXED);
    for (int i = 0; i < count;) {
        uint64_t b = __atomic_load_n(&g_gna[i].lo, __ATOMIC_RELAXED);
        uint64_t e = __atomic_load_n(&g_gna[i].hi, __ATOMIC_RELAXED);
        if (lo >= e || hi <= b) {
            i++;
            continue;
        }
        int keep_head = b < lo, keep_tail = hi < e;
        if (!keep_head && !keep_tail) {
            --count;
            __atomic_store_n(&g_gna[i].lo, __atomic_load_n(&g_gna[count].lo, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_gna[i].hi, __atomic_load_n(&g_gna[count].hi, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngna, count, __ATOMIC_RELEASE);
            continue;
        }
        if (keep_head)
            __atomic_store_n(&g_gna[i].hi, lo, __ATOMIC_RELAXED); // trim to the surviving head [b,lo)
        else
            __atomic_store_n(&g_gna[i].lo, hi, __ATOMIC_RELAXED); // keep_tail only: [hi,e)
        if (keep_head && keep_tail && count < GNA_MAX) {          // middle grant -> tail becomes a 2nd entry
            __atomic_store_n(&g_gna[count].lo, hi, __ATOMIC_RELAXED);
            __atomic_store_n(&g_gna[count].hi, e, __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngna, ++count, __ATOMIC_RELEASE);
        }
        i++;
    }
}

// True iff any byte of [a,a+len) lies in a tracked guest PROT_NONE region.
static int gna_hit(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE) == 0) return 0;
    a = nonpie_unfold(a); // registry keys are guest coordinates; callers may hold either (see the rule above)
    uint64_t end = a + len;
    uint64_t first_page = a >> 12, last_page = (end - 1) >> 12;
    uint32_t slot = (uint32_t)(first_page * 2654435761u) & (GNA_NEGATIVE_N - 1);
    for (int attempt = 0; attempt < 4096; ++attempt) {
        uint64_t generation = atomic_load_explicit(&g_gna_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        if (first_page == last_page && g_gna_negative_generation[slot] == generation &&
            g_gna_negative_page[slot] == first_page &&
            atomic_load_explicit(&g_gna_generation, memory_order_acquire) == generation)
            return 0;
        int count = __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE);
        int hit = 0;
        for (int i = 0; i < count; ++i) {
            uint64_t lo = __atomic_load_n(&g_gna[i].lo, __ATOMIC_RELAXED);
            uint64_t hi = __atomic_load_n(&g_gna[i].hi, __ATOMIC_RELAXED);
            if (a < hi && end > lo) {
                hit = 1;
                break;
            }
        }
        if (atomic_load_explicit(&g_gna_generation, memory_order_acquire) != generation) continue;
        if (!hit && first_page == last_page) {
            g_gna_negative_page[slot] = first_page;
            g_gna_negative_generation[slot] = generation;
        }
        return hit;
    }
    return 1;
}

// True iff EVERY guest page of [a,a+len) is in a tracked guest PROT_NONE region -- the whole-MAPPING
// question, which gna_hit ("any byte") must not be used for: a glibc pthread stack is one mmap whose first
// page is the guard. Walks pages: gna_add does not coalesce a piecewise-mprotect'd reservation's intervals.
static int gna_all(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE) == 0) return 0;
    uint64_t end = a + len;
    for (uint64_t page = a & ~(uint64_t)0xfff; page < end; page += 0x1000)
        if (!gna_hit(page, 1)) return 0;
    return 1;
}

// How many LEADING bytes of [a,a+len) are outside every tracked guest PROT_NONE region. Linux's
// copy_to_user is byte-granular: a read(2) whose destination straddles a PROT_NONE page copies the good
// prefix and returns that SHORT count, reporting EFAULT only when the prefix is empty. gna_hit alone
// cannot express that (it is all-or-nothing), so the read family clamps its count with this instead.
static uint64_t gna_prefix(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE) == 0) return len;
    a = nonpie_unfold(a); // guest-keyed registry; the return is a LENGTH, so the coordinate cancels
    for (int attempt = 0; attempt < 4096; ++attempt) {
        uint64_t generation = atomic_load_explicit(&g_gna_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        uint64_t end = a + len;
        int count = __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE);
        for (int i = 0; i < count; ++i) {
            uint64_t lo = __atomic_load_n(&g_gna[i].lo, __ATOMIC_RELAXED);
            uint64_t hi = __atomic_load_n(&g_gna[i].hi, __ATOMIC_RELAXED);
            if (a < hi && end > lo) {
                uint64_t first = lo > a ? lo : a;
                if (first - a < end - a) end = first;
            }
        }
        if (atomic_load_explicit(&g_gna_generation, memory_order_acquire) == generation) return end - a;
    }
    return 0;
}

static void gro_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gro_writer_lock();
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_acq_rel);
    gro_clear_raw(lo, hi);
    if (g_ngro < GNA_MAX) {
        __atomic_store_n(&g_gro[g_ngro].lo, lo, __ATOMIC_RELAXED);
        __atomic_store_n(&g_gro[g_ngro].hi, hi, __ATOMIC_RELAXED);
        __atomic_store_n(&g_ngro, g_ngro + 1, __ATOMIC_RELEASE);
    }
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_release);
    gro_writer_unlock();
}

static void gro_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gro_writer_lock();
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_acq_rel);
    gro_clear_raw(lo, hi);
    atomic_fetch_add_explicit(&g_gro_generation, 1, memory_order_release);
    gro_writer_unlock();
}

static void gro_clear_raw(uint64_t lo, uint64_t hi) {
    for (int i = 0; i < g_ngro;) {
        uint64_t b = __atomic_load_n(&g_gro[i].lo, __ATOMIC_RELAXED);
        uint64_t e = __atomic_load_n(&g_gro[i].hi, __ATOMIC_RELAXED);
        if (lo >= e || hi <= b) {
            i++;
            continue;
        }
        int keep_head = b < lo, keep_tail = hi < e;
        if (!keep_head && !keep_tail) {
            --g_ngro;
            __atomic_store_n(&g_gro[i].lo, __atomic_load_n(&g_gro[g_ngro].lo, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            __atomic_store_n(&g_gro[i].hi, __atomic_load_n(&g_gro[g_ngro].hi, __ATOMIC_RELAXED), __ATOMIC_RELAXED);
            continue;
        }
        if (keep_head)
            __atomic_store_n(&g_gro[i].hi, lo, __ATOMIC_RELAXED);
        else
            __atomic_store_n(&g_gro[i].lo, hi, __ATOMIC_RELAXED);
        if (keep_head && keep_tail && g_ngro < GNA_MAX) {
            __atomic_store_n(&g_gro[g_ngro].lo, hi, __ATOMIC_RELAXED);
            __atomic_store_n(&g_gro[g_ngro].hi, e, __ATOMIC_RELAXED);
            __atomic_store_n(&g_ngro, g_ngro + 1, __ATOMIC_RELEASE);
        }
        i++;
    }
}

static int gro_hit(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngro, __ATOMIC_ACQUIRE) == 0) return 0;
    a = nonpie_unfold(a); // guest-keyed registry; a hardware fault address arrives in storage coordinates
    uint64_t end = a + len;
    // RETRY the seqlock instead of answering "read-only" while a writer is mid-update: any concurrent
    // mprotect/mmap (a peer's thread-stack allocation) otherwise EFAULTs an unrelated writable address.
    for (int attempt = 0; attempt < 4096; attempt++) {
        uint64_t generation = atomic_load_explicit(&g_gro_generation, memory_order_acquire);
        if (generation & 1) {
            sched_yield();
            continue;
        }
        int count = __atomic_load_n(&g_ngro, __ATOMIC_ACQUIRE);
        int hit = 0;
        for (int i = 0; i < count; i++) {
            uint64_t lo = __atomic_load_n(&g_gro[i].lo, __ATOMIC_RELAXED);
            uint64_t hi = __atomic_load_n(&g_gro[i].hi, __ATOMIC_RELAXED);
            if (a < hi && end > lo) {
                hit = 1;
                break;
            }
        }
        if (atomic_load_explicit(&g_gro_generation, memory_order_acquire) == generation) return hit;
    }
    return 1; // a writer that never settles: keep the conservative answer
}

// execve replaces the whole address space -> drop all tracked PROT_NONE ranges (they're gone with the old
// image; a stale entry could otherwise wrongly EFAULT a fresh mapping the new image lays at the same address).
static void gna_reset(void) {
    gna_writer_lock();
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_acq_rel);
    __atomic_store_n(&g_ngna, 0, __ATOMIC_RELEASE);
    atomic_fetch_add_explicit(&g_gna_generation, 1, memory_order_release);
    gna_writer_unlock();
    __atomic_store_n(&g_ngro, 0, __ATOMIC_RELEASE);
    pthread_mutex_lock(&g_filemap_lock);
    for (int index = 0; index < g_nfilemap; ++index) {
        int retained = g_filemap[index].fd;
        int first = 1;
        for (int previous = 0; previous < index; ++previous)
            if (g_filemap[previous].fd == retained) first = 0;
        if (first && retained >= 0) close(retained);
    }
    g_nfilemap = 0;
    pthread_mutex_unlock(&g_filemap_lock);
    hl_logical_vma_global_reset_quiescent();
    pthread_mutex_lock(&g_bus_transition);
    gbus_lock();
    int changed = g_ngbus != 0 || g_bus_fail_closed || g_bus_prepares != 0;
    atomic_store_explicit(&g_ngbus, 0, memory_order_release);
    g_bus_fail_closed = 0;
    g_bus_prepares = 0;
    gbus_page_reset_locked();
    gbus_filter_rebuild_locked();
    atomic_store_explicit(&g_bus_filter_force, 0, memory_order_release);
    uint64_t generation = changed ? atomic_fetch_add_explicit(&g_bus_generation, 1, memory_order_release) + 1
                                  : atomic_load_explicit(&g_bus_generation, memory_order_relaxed);
    gbus_unlock();
    if (changed) gbus_notify(generation, 0);
    pthread_mutex_unlock(&g_bus_transition);
    /* Soft mode is intentionally sticky across temporary empty logical-VMA
       intervals.  exec/checkpoint image reset is the lifecycle boundary where
       old guarded translations are no longer useful; rotate once here before
       admitting direct, unguarded translations for the replacement image. */
    jit_guest_soft_deactivate();
}

// True iff host virtual address `a` is currently mapped. mincore() is useless on macOS (returns 0 for ANY
// address), so query the VM map directly: mach_vm_region returns the first region at-or-above `a`, and `a`
// is mapped iff it falls inside [start, start+size). Same technique as the x86 loader's lazy_addr_mapped.
// Used to mirror the kernel's fault-tolerant put_user() on the CLEARTID teardown path (futex_wake_addr).
static int host_addr_mapped(uintptr_t a) {
    return hl_host_address_mapped(a);
}

// per-thread ALTERNATE signal stack for the synchronous-fault guards. On the aarch64 frontend the
// host SP == the guest SP while a translated block runs, so a guest STACK OVERFLOW leaves no room for the
// kernel to push the SIGSEGV/SIGBUS guard's signal frame -- without an altstack the handler double-faults
// and the guest dies of a spurious SIGILL/SIGBUS instead of a clean, guard-delivered SIGSEGV. Installed
// once per thread (main + every guest thread) from run_guest, before any guest code executes, and torn down
// at run_guest exit. (x86 keeps host SP != guest SP, so its guards don't take SA_ONSTACK and never use it;
// the reservation is uncommitted there.)
#define HOST_ALTSTK_SZ (512u << 10)
static _Thread_local hl_host_memory_mapping g_altstk_mapping = {
    HL_HOST_MEMORY_MAPPING_ABI, sizeof(hl_host_memory_mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};

// Idempotent: (re)registers the alternate signal stack for THIS thread, allocating one on first use and
// reusing the existing region otherwise. The sigaltstack() registration is not reliably inherited across
// fork() on Apple Silicon (like the W^X/APRR state -- see fork_child_hooks), so the fork child re-arms via
// this same call with its COW-inherited region.
static void install_host_sigaltstack(void) {
    const hl_host_services *host = effective_host_services();
    hl_host_memory_mapping mapped = {
        HL_HOST_MEMORY_MAPPING_ABI, sizeof(hl_host_memory_mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};
    hl_host_memory_mapping *mapping = &g_altstk_mapping;
    int created = mapping->handle == HL_HOST_HANDLE_INVALID;
    if (created) {
        hl_host_result result;
        if (host == NULL || host->memory == NULL || host->memory->map_anonymous == NULL ||
            host->memory->release == NULL)
            return;
        result =
            host->memory->map_anonymous(host->context, 0, HOST_ALTSTK_SZ, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE,
                                        HL_HOST_MEMORY_PRIVATE, &mapped);
        if (result.status != HL_STATUS_OK || mapped.handle == HL_HOST_HANDLE_INVALID || mapped.address == 0 ||
            mapped.address > UINTPTR_MAX || mapped.mapped_size < HOST_ALTSTK_SZ) {
            if (mapped.handle != HL_HOST_HANDLE_INVALID) (void)host->memory->release(host->context, mapped.handle);
            return;
        }
        mapping = &mapped;
    }
    stack_t ss = {.ss_sp = (void *)(uintptr_t)mapping->address, .ss_size = HOST_ALTSTK_SZ, .ss_flags = 0};
    if (sigaltstack(&ss, NULL) != 0) {
        if (created) (void)host->memory->release(host->context, mapping->handle);
        return;
    }
    if (created) g_altstk_mapping = mapped;
}

static void uninstall_host_sigaltstack(void) {
    const hl_host_services *host = effective_host_services();
    hl_host_result released = {HL_STATUS_NOT_SUPPORTED, 0, 0, 0};
    if (g_altstk_mapping.handle == HL_HOST_HANDLE_INVALID) return;
    stack_t ss = {.ss_flags = SS_DISABLE};
    if (sigaltstack(&ss, NULL) != 0) return;
    if (host == NULL || host->memory == NULL || host->memory->release == NULL) return;
    /* A provider can report a transient teardown failure (including an injected first-call failure).
     * The stack is already disabled, so bounded immediate retries are safe; retain the handle unless a
     * release succeeds rather than silently forgetting provider-owned memory. */
    for (unsigned attempt = 0; attempt < 3 && released.status != HL_STATUS_OK; ++attempt)
        released = host->memory->release(host->context, g_altstk_mapping.handle);
    if (released.status == HL_STATUS_OK)
        g_altstk_mapping = (hl_host_memory_mapping){
            HL_HOST_MEMORY_MAPPING_ABI, sizeof(hl_host_memory_mapping), HL_HOST_HANDLE_INVALID, 0, 0, 0};
}

// Range form of host_addr_mapped: true iff every page spanning [a, a+len) is mapped. Used to validate a
// guest-supplied syscall buffer (a result struct to write, an argument struct to read) BEFORE dereferencing
// it, so a bad/garbage user pointer returns -EFAULT to the guest instead of faulting the engine (the
// kernel's access_ok() role). A zero length is vacuously OK; an address-space-wrapping range is rejected.
//
// PERF (sqlite/fcntl): the original implementation issued one mach_vm_region -- a full Mach
// message round-trip (~200ns+) -- PER PAGE PER CALL. `sample` showed ~97% of the engine-side overhead of the
// sqlite syscall mix (2 fcntl(F_SETLK) per query, each validating the guest flock*) inside
// host_range_mapped->mach_vm_region->mach_msg2_trap. Replace it with the kernel's own access_ok() idiom:
// a FAULT-GUARDED PROBE READ of each page under a per-thread sigsetjmp. Mapped pointer (the always case)
// = one L1 load per page, no syscall; unmapped pointer = the SIGSEGV/SIGBUS guard long-jumps back and we
// report 0 exactly as mach_vm_region did. Every fault handler on the normal run path checks
// hrm_fault_hook() FIRST (before non-PIE fixup / the x86 lazy zero-page mapper), so a probe fault can
// never be mis-served as a lazy mapping (which would flip an EFAULT into a bogus success), never burns
// lazy-map budget, and never reaches guest-signal delivery. PROT_NONE pages now probe as UNMAPPED ->
// -EFAULT, which is what a real Linux copy_from_user() returns (the old region query called them mapped
// and the later engine deref crashed) -- strictly closer to the oracle. CRASHDBG runs (whose Mach
// exception port intercepts EXC_BAD_ACCESS before the POSIX guards) and HL_NOFASTHRM=1 keep the
// byte-identical mach_vm_region path.
#include <setjmp.h>
#if defined(_WIN32)
// The host fault primitive already owns this exact operation, pad and all: it arms its own landing site,
// touches each page (with an atomic OR of zero when the caller asked about writes, so the access really is
// a write and still cannot lose a concurrent update), and reports whether every page was reachable. Its
// probe window is consulted by the vectored handler BEFORE the engine classifier runs, which is exactly the
// ordering the POSIX arm gets from calling hrm_fault_hook first -- a probe fault is the probe's answer and
// nothing else's, so it can never be mis-served as a lazy mapping or reach guest-signal delivery.
//
// Measured, and the reason that ordering is stated rather than assumed: consulting the window after a
// DECLINE instead is not equivalent, because the engine's classifier has no decline for a fault it cannot
// serve -- it terminates the guest. A probe of an unmapped page (an mprotect hole check, a syscall's EFAULT
// check) killed the process before the window was reached.
//
// So there is nothing left for a second implementation to do, and the two thread-locals and the fault hook
// below have no Windows arm at all. What the primitive additionally buys, which no POSIX arm can, is the
// kernel-write hole: a kernel touching an inaccessible user page on the caller's behalf raises no exception
// anywhere, so every host call that hands guest memory to the kernel has to make the pages good from user
// mode first, and that is this same probe.
#include "../host/windows/fault.h"
#endif
static _Thread_local sigjmp_buf g_hrm_jb;                   // probe return point (valid while g_hrm_hi != 0)
static _Thread_local volatile uintptr_t g_hrm_lo, g_hrm_hi; // page range being probed; probing iff hi != 0
static int g_hrm_slow = -1; // HL_NOFASTHRM=1 / crash diagnostics -> per-page mach_vm_region

// Called FIRST by every SIGSEGV/SIGBUS handler on the run path: when the fault is this thread's own probe
// load, long-jump back to host_range_mapped ("unmapped"). The faulting signal was auto-blocked at handler
// entry and siglongjmp(.,0) does not restore masks, so unblock it here or the NEXT probe fault would be
// force-killed instead of caught. Returns 0 (fault not ours) in every other case.
static int hrm_fault_hook(siginfo_t *si) {
#if defined(_WIN32)
    // Never claims anything: the probe window lives inside the host fault primitive, and its dispatcher
    // tests it directly. A hook that answered here would be a second, stale copy of that window.
    (void)si;
    return 0;
#else
    if (!g_hrm_hi) return 0;
    uintptr_t va = (uintptr_t)(si ? si->si_addr : NULL);
    if (va < g_hrm_lo || va >= g_hrm_hi) return 0; // not the probe access -> normal fault handling
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, SIGSEGV);
    sigaddset(&s, SIGBUS);
    pthread_sigmask(SIG_UNBLOCK, &s, NULL);
    siglongjmp(g_hrm_jb, 1); // never returns
#endif
}

static int host_range_mapped(uintptr_t a, size_t len) {
    if (!len) return 1;
    uintptr_t end = a + len;
    if (end < a) return 0; // wrap -> bogus pointer
    // A guest PROT_NONE mapping is physically R+W under hl (see the g_gna registry above), so the page
    // probe below would call it mapped; the kernel's copy_to/from_user faults it. Reject up front.
    if (gna_hit((uint64_t)a, (uint64_t)len) || hl_linux_bus_hit((uint64_t)a, (uint64_t)len)) return 0;
#if defined(_WIN32)
    // The registry rejections above are still ours -- they encode guest-visible protection this host's
    // page tables do not carry -- and everything below them is the host primitive's job.
    return hl_windows_fault_probe((uint64_t)a, (uint64_t)len, 0);
#else
    uintptr_t lo = a & ~(uintptr_t)0xfff;
    if (g_hrm_slow < 0) g_hrm_slow = 0;
    if (g_hrm_slow) {
        for (uintptr_t p = lo; p < end; p += 0x1000)
            if (!host_addr_mapped(p)) return 0;
        return 1;
    }
    volatile int ok = 1;
    if (sigsetjmp(g_hrm_jb, 0)) {
        ok = 0; // a probe load faulted -> some page in the range is unmapped
    } else {
        g_hrm_lo = lo;
        g_hrm_hi = end;
        for (uintptr_t p = lo; p < end; p += 0x1000) {
            (void)*(volatile const uint8_t *)p;
            /* A file mapping can end in the middle of a guest page.  Darwin's
               VM region query and a probe at the page start both succeed, but
               bytes after EOF raise SIGBUS.  Probe the last covered byte too;
               mapped ranges cannot contain an interior hole, so the pair
               proves the complete page fragment is readable. */
            uintptr_t q = p + 0xfff;
            if (q >= end) q = end - 1;
            if (q != p) (void)*(volatile const uint8_t *)q;
        }
    }
    g_hrm_lo = 0;
    g_hrm_hi = 0; // probe window closed (hook inert again)
    return ok;
#endif
}

/* Prove that every guest-page fragment in a range accepts stores without ever
   storing into guest memory. Guest mappings and their read-only/PROT_NONE/EOF
   intervals are tracked when they are created or protected; the guarded READ
   probe catches unmapped and Darwin file-tail SIGBUS bytes. */
static int host_range_writable(uintptr_t a, size_t len) {
    if (!len) return 1;
    uintptr_t end = a + len;
    if (end < a || gna_hit((uint64_t)a, (uint64_t)len) || gro_hit((uint64_t)a, (uint64_t)len) ||
        hl_linux_bus_hit((uint64_t)a, (uint64_t)len))
        return 0;
#if defined(_WIN32)
    // A WRITE probe, not the read probe host_range_mapped issues. Two things follow from that on this host
    // and neither is available to the POSIX arm: a page that is mapped but not writable answers correctly
    // without consulting any registry, and the page is left present and dirty -- which is what closes the
    // kernel-write hole for the call this validation precedes, where a kernel store into a not-yet-good
    // page fails with no exception raised anywhere and no handler entered.
    return hl_windows_fault_probe((uint64_t)a, (uint64_t)len, 1);
#else
    return host_range_mapped(a, len);
#endif
}

static void abs_from_rel(struct timespec *abs, const struct timespec *ts) {
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_REALTIME, abs);
    abs->tv_sec += ts->tv_sec;
    abs->tv_nsec += ts->tv_nsec;
    if (abs->tv_nsec >= 1000000000) {
        abs->tv_sec++;
        abs->tv_nsec -= 1000000000;
    }
}

// FUTEX_WAIT_BITSET (op 9) passes an ABSOLUTE deadline, not a relative duration: against
// CLOCK_REALTIME when FUTEX_CLOCK_REALTIME is set (e.g. glibc's pthread_cond_timedwait on a
// CLOCK_REALTIME condvar) and CLOCK_MONOTONIC otherwise. That clock flag is masked off before
// the syscall reaches us, so we recover the intended clock from the deadline itself: only one
// of the two clocks leaves a sane remaining time -- the other is off by the decades-wide gap
// between realtime (~now since 1970) and monotonic (~uptime), yielding a negative or absurdly
// large value. Fills `rel` with the remaining time until the deadline, clamped at zero.
static void futex_rel_from_abs(struct timespec *rel, const struct timespec *deadline) {
    struct timespec rt, mono;
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_REALTIME, &rt);
    hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &mono);
    int64_t drt = (int64_t)(deadline->tv_sec - rt.tv_sec) * 1000000000 + (deadline->tv_nsec - rt.tv_nsec);
    int64_t dmono = (int64_t)(deadline->tv_sec - mono.tv_sec) * 1000000000 + (deadline->tv_nsec - mono.tv_nsec);
    int64_t ns;
    if (drt < 0)
        ns = dmono; // deadline predates realtime "now" -> it must be a monotonic deadline
    else if (dmono < 0)
        ns = drt;
    else
        ns = drt < dmono ? drt : dmono; // both plausible: the true clock gives the smaller remainder
    if (ns < 0) ns = 0;
    rel->tv_sec = ns / 1000000000;
    rel->tv_nsec = ns % 1000000000;
}

// ---- interruptible waits: let a cross-thread tgkill wake a thread parked in futex_op ----
// A guest FUTEX_WAIT lands the host thread in an (otherwise uninterruptible) pthread_cond_wait. A signal
// aimed at that thread via tkill/tgkill only sets its cpu->tpending; the thread must then round-trip through
// the dispatcher for maybe_deliver_signal to run the guest handler. A thread spinning in translated code
// crosses that boundary continuously (Go's SIGURG stop-the-world preemption relies on exactly that), but a
// PARKED thread sits in cond_wait and never returns -- so the handler never runs. This bit Go's
// runtime.doAllThreadsSyscall (glibc/musl setuid/setgid across all OS threads): the coordinator tgkills each
// sibling M with realtime signal 33 and busy-waits (sched_yield) for its handler to perform the per-thread
// syscall and ack; a parked sibling never woke, so the coordinator spun forever (postgres/mysql/mariadb
// gosu/su-exec privilege-drop hung). Fix: publish the wait primitive so the signaler can wake the parked
// thread, and check for a deliverable thread-directed signal around the wait so it returns -EINTR (the guest
// retries the futex and the dispatcher delivers the handler first), exactly as a real futex is interrupted.

// A thread-directed signal is "actionable" for thread c iff it is pending and NOT blocked by c's mask: the
// dispatcher will then either run its guest handler or apply the default action, so the thread makes
// progress. A blocked pending signal must NOT interrupt a wait (it stays pending, as on Linux) -- otherwise
// the guest would re-wait, see it still pending, and spin returning EINTR forever.
static int cpu_has_actionable_tsig(const struct cpu *c) {
    uint64_t t = __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    if (!t) return 0; // no thread-directed signal pending -- the common case on the hot futex path
    for (int s = 1; s <= 64; s++)
        if ((t & (1ull << s)) && !(c->sigmask & (1ull << (s - 1)))) return 1;
    return 0;
}

// A futex/interruptible wait must abort (return -EINTR so the guest round-trips through the dispatcher) when
// either an actionable thread-directed signal arrives OR this thread has been flagged exited by a peer's
// execve teardown (thread_exit_others) -- in both cases the dispatcher must regain control.
static int cpu_wait_interrupted(const struct cpu *c) {
    return ckpt_pending() || __atomic_load_n(&c->exited, __ATOMIC_SEQ_CST) || cpu_has_actionable_tsig(c);
}

// This thread's slot in g_threg (set on register), so it can publish the primitive it is about to wait on
// without re-scanning the registry on the hot futex path.
static __thread int g_my_threg = -1;
// Publish/clear (defined after g_threg) the mutex+condvar this thread is blocked on. thread_wait_publish is
// called UNDER the wait's own mutex and BEFORE the tpending re-check, so the publish (store) is ordered
// ahead of that check (load): with the signaler's store-tpending-then-load-waitc, this seq_cst StoreLoad
// handshake guarantees at least one side observes the other -- no lost wakeup.
static void thread_wait_publish(pthread_mutex_t *m, pthread_cond_t *cnd);
static void thread_wait_clear(void);

// Task-state stamping (defined later in container/vfs.c, same translation unit). A guest that parks in a
// host blocking wait publishes 'S' (interruptible sleep) into the cross-process task-state table, and 'R'
// on wake, so a peer reading /proc/<pid>/stat (field 3) or /proc/<pid>/status (State:) sees the true state.
// FUTEX_WAIT is a blocking wait exactly like recv/read/epoll_wait (which already bracket their parks), so
// the actual pthread_cond_wait park below must be bracketed too -- otherwise a futex-blocked waiter is
// reported 'R' (running) instead of 'S', hiding blocked threads from monitors and deadlock diagnostics.
static inline void ts_wait_enter(void);
static inline void ts_wait_leave(void);

// Wake up to `n` waiters parked on uaddr's W5C bucket and report the number actually woken (capped at n).
// Factored out of the FUTEX_WAKE path so FUTEX_WAKE_OP wakes through the SAME buckets -- its wakes must
// reach real WAIT-parked waiters (in this process or, across a shared page, a forked peer). Mirrors the
// WAKE block exactly: PROF fast/slow split, lock+broadcast (the lock orders the guest's pre-syscall store
// to *uaddr ahead of an arriving waiter's under-lock value-check, so no wakeup is lost), and a count taken
// from the per-address slot (the number of waiters that will re-check their word and leave).
// `grant_all` selects EVERY matching waiter (returning only the first `n` as the woken count) for the
// REQUEUE family, whose parked peers have no secondary queue to be woken from later, so this
// approximation moves them by waking them all; plain FUTEX_WAKE(n) passes 0 and grants exactly `n`.
static int futex_wake_bucket(const void *key, int n, uint32_t match, int grant_all) {
    struct futex_bucket *b = fbk_of(key);
    if (g_prof) {
        if (atomic_load_explicit(&b->waiters, memory_order_relaxed))
            g_futex_wake_slow++;
        else
            g_futex_wake_fast++;
    }
    pthread_mutex_lock(&b->m);
    // FUTEX_WAKE_BITSET only wakes waiters whose bitset overlaps `match`; a plain FUTEX_WAKE passes ~0u.
    // If no parked waiter on this address can match, wake nobody (Linux does not disturb them).
    if (!fbk_match(b, futex_key(key), match)) {
        pthread_mutex_unlock(&b->m);
        return 0;
    }
    int registered = 0;
    int woke = fbk_wait_grant(b, futex_key(key), n, match, &registered);
    if (grant_all && registered) { // REQUEUE approximation: also release the peers left behind
        int r2 = 0;
        (void)fbk_wait_grant(b, futex_key(key), INT_MAX, match, &r2);
    }
    // PI and overflow fallback waiters do not occupy ordinary-wait slots;
    // their loops re-check ownership/value after a broadcast, so the old
    // bounded parked count remains correct for that exceptional path.
    if (!registered) {
        woke = fbk_parked(b, futex_key(key));
        if (woke > n) woke = n;
    }
    pthread_cond_broadcast(&b->c); // waiters re-check their own word; spurious wakes are legal
    pthread_mutex_unlock(&b->m);
    return woke;
}

// FUTEX_WAKE_OP arithmetic half (Linux-exact): atomically apply the val3-encoded op to *uaddr2 and report,
// via *do_wake2, whether the PRE-mutation value satisfies the encoded comparison (=> uaddr2 waiters are also
// woken by the caller). val3 layout: bits 28-31 op (bit 3 = FUTEX_OP_OPARG_SHIFT), 24-27 cmp, 12-23 oparg,
// 0-11 cmparg. Returns 0, or -EFAULT for an unmapped uaddr2 (kernel's copy semantics), -ENOSYS for an
// unknown op/cmp. Comparisons are signed, exactly as the kernel's futex_atomic_op_inuser.
static int futex_wake_op_apply(int *uaddr2, uint32_t val3, int *do_wake2) {
    unsigned enc = val3;
    int op2 = (enc >> 28) & 0xf;
    int cmp = (enc >> 24) & 0xf;
    int oparg = (enc >> 12) & 0xfff;
    int cmparg = enc & 0xfff;
    if (op2 & 8) { // FUTEX_OP_OPARG_SHIFT: oparg is a shift count (masked to 31, as the kernel does)
        op2 &= 7;
        oparg = 1 << (oparg & 31);
    }
    if (!uaddr2 || !host_addr_mapped((uintptr_t)uaddr2)) return -EFAULT;
    int oldval;
    switch (op2) {
    case 0: oldval = __atomic_exchange_n(uaddr2, oparg, __ATOMIC_SEQ_CST); break; // FUTEX_OP_SET
    case 1: oldval = __atomic_fetch_add(uaddr2, oparg, __ATOMIC_SEQ_CST); break;  // FUTEX_OP_ADD
    case 2: oldval = __atomic_fetch_or(uaddr2, oparg, __ATOMIC_SEQ_CST); break;   // FUTEX_OP_OR
    case 3: oldval = __atomic_fetch_and(uaddr2, ~oparg, __ATOMIC_SEQ_CST); break; // FUTEX_OP_ANDN
    case 4: oldval = __atomic_fetch_xor(uaddr2, oparg, __ATOMIC_SEQ_CST); break;  // FUTEX_OP_XOR
    default: return -ENOSYS;
    }
    int cond;
    switch (cmp) {
    case 0: cond = (oldval == cmparg); break; // FUTEX_OP_CMP_EQ
    case 1: cond = (oldval != cmparg); break; // FUTEX_OP_CMP_NE
    case 2: cond = (oldval < cmparg); break;  // FUTEX_OP_CMP_LT
    case 3: cond = (oldval <= cmparg); break; // FUTEX_OP_CMP_LE
    case 4: cond = (oldval > cmparg); break;  // FUTEX_OP_CMP_GT
    case 5: cond = (oldval >= cmparg); break; // FUTEX_OP_CMP_GE
    default: return -ENOSYS;
    }
    *do_wake2 = cond;
    return 0;
}

// FUTEX PI (priority-inheritance) mutex constants: the futex word holds the owner's guest TID in the low 30
// bits, plus FUTEX_WAITERS (contended -> userspace must trap into the kernel) and FUTEX_OWNER_DIED (robust:
// the owner exited still holding it). hl does not model priority BOOSTING (a latency/QoS property, not a
// correctness one), but it enforces real MUTUAL EXCLUSION and the exact futex-word contract glibc's userspace
// fast paths depend on -- so two threads can never both believe they own a PTHREAD_PRIO_INHERIT/robust mutex
// (the old return-0 fake-acquire silently let them into the critical section together -> data corruption).
#define HL_FUTEX_WAITERS 0x80000000u
#define HL_FUTEX_OWNER_DIED 0x40000000u
#define HL_FUTEX_TID_MASK 0x3fffffffu

static int cpu_tid(const struct cpu *c);

// Acquire the PI mutex at uaddr for this thread (block until free, unless `trylock`). On success writes
// this thread's TID -- OR'd with FUTEX_WAITERS when other threads remain queued on the same word -- into the
// futex word and returns 0, or -EOWNERDEAD when the prior owner died holding it (robust recovery). NEVER
// returns 0 without the word actually naming this thread as the owner. `mono`: the (absolute) timeout is on
// CLOCK_MONOTONIC (FUTEX_LOCK_PI2) rather than the FUTEX_LOCK_PI default of CLOCK_REALTIME. Interruptible
// (a thread-directed signal returns -EINTR, exactly as a real LOCK_PI is interrupted -> glibc retries).
static long futex_lock_pi(struct cpu *c, int *uaddr, const void *key, int trylock, const struct timespec *ts,
                          int mono) {
    if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
    int mytid = cpu_tid(c);
    struct futex_bucket *b = fbk_of(key);
    pthread_mutex_lock(&b->m);
    int parked = 0;
    long ret;
    for (;;) {
        int expect = __atomic_load_n(uaddr, __ATOMIC_SEQ_CST);
        uint32_t v = (uint32_t)expect;
        uint32_t owner = v & HL_FUTEX_TID_MASK;
        int others = fbk_parked(b, futex_key(key)) - (parked ? 1 : 0); // waiters left behind
        // A fresh arrival must not steal from a queued waiter: Linux hands the rt_mutex to the top waiter, so
        // an unlocker that immediately re-locks queues behind it. Stealing starved waiters out to ETIMEDOUT.
        // Only when the parked count is exact -- an overflowed bucket counts foreign addresses, and blocking
        // on a phantom waiter would park us with nobody left to wake us.
        int must_queue = !parked && others > 0 && !b->imprecise;
        if (owner == 0 && !must_queue) { // free (FUTEX_OWNER_DIED may still be set on a robust mutex)
            int nv = (int)((uint32_t)mytid | (v & HL_FUTEX_OWNER_DIED) | (others > 0 ? HL_FUTEX_WAITERS : 0));
            // Acquire atomically vs a racing userspace fast-path locker (cmpxchg 0->tid): if the word moved
            // underfoot, retry from the re-read instead of clobbering the new owner (double-ownership bug).
            if (!__atomic_compare_exchange_n(uaddr, &expect, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) continue;
            /*
             * PI owner death is reported in the futex word, not as a negative
             * FUTEX_LOCK_PI syscall result. glibc consumes OWNER_DIED after a
             * successful syscall and turns it into pthread EOWNERDEAD.
             */
            ret = 0;
            break;
        }
        if (owner == (uint32_t)mytid) {
            ret = -EDEADLK;
            break;
        } // this thread already owns it
        if (trylock) {
            ret = -EAGAIN;
            break;
        } // TRYLOCK_PI: contended -> fail, no block
        // Contended: set FUTEX_WAITERS so glibc's userspace unlock fast path traps to FUTEX_UNLOCK_PI and a
        // userspace lock fast path can't steal ahead of us. Do it as a CMPXCHG under b->m: if the owner just
        // released in userspace (word -> 0) our swap fails and we loop to re-read + acquire -- WITHOUT this
        // the stale store would resurrect a dead owner and every waiter would block forever (the deadlock).
        if (!__atomic_compare_exchange_n(uaddr, &expect, (int)(v | HL_FUTEX_WAITERS), 0, __ATOMIC_SEQ_CST,
                                         __ATOMIC_SEQ_CST))
            continue;
        if (!parked) {
            atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
            fbk_park(b, futex_key(key), ~0u); // PI-mutex waiter: matches any wake bitset
            parked = 1;
        }
        thread_wait_publish(&b->m, &b->c);
        if (cpu_wait_interrupted(c)) {
            thread_wait_clear();
            ret = -EINTR;
            break;
        }
        ts_wait_enter(); // 'S' while parked in FUTEX_LOCK_PI/PI2 (PI-mutex contention)
        int rc = 0;
        if (ts) {
            struct timespec abs, rel;
            if (mono) { // FUTEX_LOCK_PI2: absolute CLOCK_MONOTONIC deadline -> realtime abs for the condvar
                futex_rel_from_abs(&rel, ts);
                abs_from_rel(&abs, &rel);
            } else {
                abs = *ts; // FUTEX_LOCK_PI: already an absolute CLOCK_REALTIME deadline (the condvar's clock)
            }
            rc = pthread_cond_timedwait(&b->c, &b->m, &abs);
        } else {
            pthread_cond_wait(&b->c, &b->m);
        }
        ts_wait_leave();
        thread_wait_clear();
        if (cpu_wait_interrupted(c)) {
            ret = -EINTR;
            break;
        }
        if (rc == ETIMEDOUT) {
            ret = -ETIMEDOUT;
            break;
        }
        // otherwise loop and re-read the word (the releaser cleared the owner; race for ownership under b->m)
    }
    if (parked) {
        fbk_unpark(b, futex_key(key));
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
    }
    pthread_mutex_unlock(&b->m);
    return ret;
}

// Release the PI mutex at uaddr (FUTEX_UNLOCK_PI): only the owner may unlock (-EPERM otherwise). Hand off by
// clearing the owner TID; if waiters remain, keep FUTEX_WAITERS set (word = FUTEX_WAITERS, owner 0) so a
// userspace fast-path locker can't steal in ahead of a parked waiter, and broadcast -- the woken waiters
// re-contend for ownership under the bucket mutex in futex_lock_pi, so exactly one acquires. Returns 0.
static long futex_unlock_pi(struct cpu *c, int *uaddr, const void *key) {
    if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
    int mytid = cpu_tid(c);
    struct futex_bucket *b = fbk_of(key);
    pthread_mutex_lock(&b->m);
    uint32_t v = (uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST);
    if ((v & HL_FUTEX_TID_MASK) != (uint32_t)mytid) {
        pthread_mutex_unlock(&b->m);
        return -EPERM; // not the owner -- Linux rejects an UNLOCK_PI from a non-owner
    }
    int waiters = fbk_parked(b, futex_key(key));
    __atomic_store_n(uaddr, (int)(waiters > 0 ? HL_FUTEX_WAITERS : 0), __ATOMIC_SEQ_CST);
    if (waiters > 0) pthread_cond_broadcast(&b->c);
    pthread_mutex_unlock(&b->m);
    return 0;
}

// nr_wake2 is the raw 4th syscall arg (a3) reinterpreted as a count for FUTEX_WAKE_OP (WAIT ops use a3 as a
// timespec instead -- the two never overlap because op selects one interpretation); uaddr2 (a4) + val3 (a5)
// carry the WAKE_OP / REQUEUE second-address operands, and are ignored by the WAIT/plain-WAKE branches.
static long futex_op(struct cpu *c, int *uaddr, const void *key, int op, int private, int val,
                     const struct timespec *ts, int nr_wake2, int *uaddr2, const void *key2, uint32_t val3) {
    // Linux FUTEX_PRIVATE_FLAG promises that no other process can participate. Keep those high-frequency
    // pthread waits on process-private host mutexes/condvars; macOS process-shared pthread primitives are
    // substantially heavier and eventually fault/livelock under sustained condvar churn. Non-private ops
    // retain the MAP_SHARED table required by forked and independently-mapped shared futexes.
    g_fbk_active = private ? g_fbk_private : g_fbk;
    // FUTEX_WAIT_BITSET(9)/WAKE_BITSET(10) require a non-empty bitset: Linux rejects val3==0 with EINVAL
    // (a zero mask can match no waiter). The old shared WAIT/WAKE path ignored val3 entirely and accepted it.
    if ((op == 9 || op == 10) && val3 == 0) return -EINVAL;
    // PI-mutex ops use per-address ownership tracked in the (always-present) buckets, independent of the
    // legacy single-queue mode below, so dispatch them first. FUTEX_LOCK_PI=6, UNLOCK_PI=7, TRYLOCK_PI=8,
    // WAIT_REQUEUE_PI=11, CMP_REQUEUE_PI=12, LOCK_PI2=13.
    if (op == 6) return futex_lock_pi(c, uaddr, key, 0, ts, 0);
    if (op == 13) return futex_lock_pi(c, uaddr, key, 0, ts, 1);
    if (op == 8) return futex_lock_pi(c, uaddr, key, 1, NULL, 0);
    if (op == 7) return futex_unlock_pi(c, uaddr, key);
    if (op == 11) { // FUTEX_WAIT_REQUEUE_PI: wait on uaddr (while *uaddr==val), then acquire the PI mutex uaddr2.
        // Modern glibc (>=2.25) condvars no longer use requeue_pi, so this path is cold; implement it as a
        // plain WAIT followed by a LOCK_PI on uaddr2 -- semantically what pthread_cond_wait on a PI mutex
        // needs, and always CORRECT (a woken waiter re-acquires uaddr2 itself; see CMP_REQUEUE_PI below).
        if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
        struct futex_bucket *b = fbk_of(key);
        pthread_mutex_lock(&b->m);
        if ((uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != (uint32_t)val) {
            pthread_mutex_unlock(&b->m);
            return -EAGAIN;
        }
        atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
        fbk_park(b, futex_key(key), ~0u); // PI-mutex waiter: matches any wake bitset
        thread_wait_publish(&b->m, &b->c);
        long ret = 0;
        if (cpu_wait_interrupted(c)) {
            ret = -EINTR;
        } else {
            ts_wait_enter(); // 'S' while parked in FUTEX_WAIT_REQUEUE_PI
            int rc = 0;
            if (ts) {
                struct timespec abs, rel;
                futex_rel_from_abs(&rel, ts); // WAIT_REQUEUE_PI's timeout is an absolute deadline
                abs_from_rel(&abs, &rel);
                rc = pthread_cond_timedwait(&b->c, &b->m, &abs);
            } else {
                pthread_cond_wait(&b->c, &b->m);
            }
            ts_wait_leave();
            if (cpu_wait_interrupted(c))
                ret = -EINTR;
            else if (rc == ETIMEDOUT)
                ret = -ETIMEDOUT;
        }
        thread_wait_clear();
        fbk_unpark(b, futex_key(key));
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
        pthread_mutex_unlock(&b->m);
        if (ret < 0) return ret;                         // on error the caller does NOT own uaddr2 (kernel-exact)
        return futex_lock_pi(c, uaddr2, key2, 0, ts, 0); // woken -> acquire the target PI mutex before returning
    }
    if (op == 12) { // FUTEX_CMP_REQUEUE_PI: verify *uaddr==val3, wake uaddr waiters (they self-acquire uaddr2).
        // We don't physically move queues -- each woken WAIT_REQUEUE_PI waiter re-acquires uaddr2's PI lock on
        // its own, so waking them and letting them serialize there is correct (the requeue is only a
        // thundering-herd optimization). Returns the number woken.
        if (uaddr && host_addr_mapped((uintptr_t)uaddr) && (uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != val3)
            return -EAGAIN;
        // Wake up to `val` signalled + `nr_wake2` requeue-budget waiters in one broadcast; each self-acquires
        // uaddr2, so the physical requeue is unnecessary. (A single broadcast wakes all parked in the bucket.)
        long budget = (long)(val < 1 ? 1 : val) + (nr_wake2 > 0 ? nr_wake2 : 0);
        return futex_wake_bucket(key, budget > 0x7fffffff ? 0x7fffffff : (int)budget, ~0u, 1);
    }
    // ---- W5C per-address buckets ----
    struct futex_bucket *b = fbk_of(key);
    // FUTEX_WAIT / WAIT_BITSET: sleep while *uaddr == val
    if (op == 0 || op == 9) {
        if (g_prof) g_futex_wait_n++;
        pthread_mutex_lock(&b->m);
        // Count this bucket's sleepers (PROF only). The value-check below runs under b->m, and so
        // does every WAKE's broadcast, so the lock -- not this counter -- closes the lost-wakeup
        // window: we see the waker's new *uaddr here (and bail) or we cond_wait and it broadcasts.
        atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
        if (__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != val) {
            atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed);
            pthread_mutex_unlock(&b->m);
            return -EAGAIN;
        }
        // Publish, then re-check tpending (the StoreLoad handshake with thread_target_signal) so a
        // thread-directed signal arriving just before we sleep interrupts the wait instead of deadlocking.
        thread_wait_publish(&b->m, &b->c);
        if (cpu_wait_interrupted(c)) {
            thread_wait_clear();
            atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed);
            pthread_mutex_unlock(&b->m);
            return -EINTR;
        }
        // We are now about to actually park: record this waiter against its uaddr so a FUTEX_WAKE (this
        // process or, across a shared page, another) can report the true woken count. Kept in the SHARED
        // bucket, so a cross-fork waker sees it. Carry the wait bitset (op 9 = FUTEX_WAIT_BITSET's val3;
        // plain FUTEX_WAIT matches any) so FUTEX_WAKE_BITSET can skip a non-overlapping wake.
        fbk_park(b, futex_key(key), op == 9 ? val3 : ~0u);
        int wait_slot = fbk_wait_register(b, futex_key(key), op == 9 ? val3 : ~0u);
        ts_wait_enter(); // 'S' (sleeping) while parked in FUTEX_WAIT; peer /proc/<pid>/stat|status must not show 'R'
        int rc = 0;
        struct timespec abs;
        if (ts) {
            struct timespec rel;
            // op 9 (FUTEX_WAIT_BITSET): ts is an absolute deadline; op 0: it is relative.
            if (op == 9) futex_rel_from_abs(&rel, ts);
            abs_from_rel(&abs, op == 9 ? &rel : ts);
        }
        // FUTEX_WAKE(n) selects EXACTLY n waiters by setting their wgrant slot (fbk_wait_grant); the
        // pthread_cond_broadcast is only the transport that makes sleepers runnable. An unselected peer
        // that wakes must therefore re-park rather than return success -- otherwise a WAKE(1) releases
        // every waiter in the bucket. Loop until we are granted, time out, or a signal interrupts us. A
        // waiter that overflowed the exact-selection slots (wait_slot < 0) keeps the legacy re-check-word
        // wake so it can never be stranded.
        int intr = 0;
        for (;;) {
            rc = ts ? pthread_cond_timedwait(&b->c, &b->m, &abs) : pthread_cond_wait(&b->c, &b->m);
            if (cpu_wait_interrupted(c)) {
                intr = 1;
                break;
            }
            if (rc == ETIMEDOUT) break;
            if (wait_slot < 0) break;
            if (b->wgrant[wait_slot]) {
                b->wgrant[wait_slot] = 0;
                break;
            }
        }
        ts_wait_leave();
        thread_wait_clear();
        fbk_wait_unregister(b, wait_slot);
        fbk_unpark(b, futex_key(key));
        // fetch_sub returns the PREVIOUS value; == 1 means the bucket just fully drained -> a stale
        // `imprecise` flag (set by a past slot overflow) can be cleared so exact counting resumes.
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
        pthread_mutex_unlock(&b->m);
        if (intr) return -EINTR; // woken by a cross-thread signal -> guest retries; dispatcher delivers it
        // A pure-timeout wait must report -ETIMEDOUT so the guest stops re-waiting.
        return rc == ETIMEDOUT ? -ETIMEDOUT : 0;
    }
    // FUTEX_WAKE / WAKE_BITSET / REQUEUE / CMP_REQUEUE: wake the waiters on THIS address's bucket.
    // REQUEUE(3)/CMP_REQUEUE(4) ask to wake `val` waiters on uaddr and MOVE the rest onto a second
    // futex (uaddr2) to be woken later by its owner. musl's pthread_cond_broadcast issues exactly this
    // (wake 1, requeue the rest onto the mutex) -- so dropping it (the old "other ops -> return 0")
    // silently lost every broadcast wakeup and any joiner/cond waiter slept forever (node's V8 worker
    // threads never exit -> hang at process shutdown). We don't model the secondary queue; instead we
    // broadcast ALL waiters on uaddr. Waking is always safe -- a spuriously woken waiter re-checks its
    // word and re-waits if needed -- and the requeue target is only an optimization to avoid a
    // thundering herd, so broadcasting is correct, just less efficient under heavy contention.
    if (op == 1 || op == 10 || op == 3 || op == 4) {
        // A real FUTEX_WAKE returns the NUMBER of waiters actually woken (capped at the requested `val`),
        // NOT `val` itself. futex_wake_bucket takes the bucket mutex + broadcasts (the lock orders the
        // guest's pre-syscall store to *uaddr ahead of an arriving waiter's under-lock value-check, so no
        // wakeup is lost -- the old lock-free no-sleeper skip lost wakeups on ARM) and counts the waiters
        // parked on THIS uaddr: each re-checks its word, sees the store, and leaves -- exactly the woken
        // count. (Returning `val` broke LTP tst_checkpoint_wake's `waked += WAKE(INT_MAX)` -> fork04.)
        // FUTEX_WAKE_BITSET (op 10) wakes only waiters whose bitset overlaps val3; the others match any.
        // REQUEUE(3)/CMP_REQUEUE(4) have no modelled secondary queue, so wake every parked peer (grant_all);
        // plain WAKE(1)/WAKE_BITSET(10) must release EXACTLY `val` -- unselected peers re-park (see WAIT loop).
        int woke = futex_wake_bucket(key, val, op == 10 ? val3 : ~0u, op == 3 || op == 4);
        return woke;
    }
    if (op == 5) { // FUTEX_WAKE_OP: atomically mutate *uaddr2, wake uaddr waiters, conditionally uaddr2's.
        // glibc's pthread_cond_signal/broadcast issue this (bump the internal seq/counter at uaddr2 and wake
        // the condvar's futex at uaddr) -- the old "other ops -> return 0" reported success WITHOUT waking,
        // so every glibc condvar signal was silently dropped (the waiting thread remained blocked on the
        // an in-process helper thread's condvar -> an application stall).
        int do_wake2 = 0;
        int rc = futex_wake_op_apply(uaddr2, val3, &do_wake2);
        if (rc < 0) return rc; // -EFAULT (bad uaddr2) / -ENOSYS (unknown op|cmp): report to the guest as-is
        int woke = futex_wake_bucket(key, val, ~0u, 0);
        if (do_wake2) woke += futex_wake_bucket(key2, nr_wake2, ~0u, 0);
        return woke;
    }
    // A genuinely undefined command (the removed FUTEX_FD=2, or any value >= 14 that names no futex op) is
    // -ENOSYS on Linux, not a silent success -- the old fall-through masked capability probes. The PI ops
    // (6-8,11-13) that hl does not model are left as best-effort success above to avoid breaking a PI-mutex
    // fast-path fallback; only the truly-undefined range is rejected here.
    if (op == 2 || op >= 14) return -ENOSYS;
    // Any remaining op is unmodelled -- pretend success (baseline behavior).
    return 0;
}

static void futex_wake_addr(uint64_t uaddr) {
    if (!uaddr) return;
    uaddr = nonpie_fold(uaddr); // clear_child_tid is stored in guest coordinates; this is its deref
    // CLONE_CHILD_CLEARTID: zero the word then wake joiners (pthread_join FUTEX_WAITs on this word). A
    // DETACHED guest thread (e.g. musl's __unmapself) munmaps its OWN stack -- which also holds the thread
    // descriptor with this CLEARTID word -- and only THEN issues the exit syscall, so by the time we run
    // here the word can already be unmapped. Linux's clear_child_tid uses put_user() and silently swallows
    // that fault; a raw store here would instead SIGSEGV/SIGBUS the whole process (the flaky rustc-at-exit
    // teardown crash). Skip the store+wake when the address is gone -- a detached thread has no joiner to
    // wake, and a joinable thread never unmaps its own stack so its ctid is always still live.
    if (!host_addr_mapped((uintptr_t)uaddr)) return;
    *(int *)uaddr = 0;
    // libc implementations use both private and shared futex operations for
    // thread joins.  The kernel-generated clear-child-tid wake is not tagged
    // by the exiting guest syscall, so notify both key spaces.  The word is
    // already zero and waiters re-check it, making the second notification a
    // harmless spurious wake while avoiding a permanently parked exact waiter.
    struct futex_bucket *tables[] = {g_fbk_private, g_fbk};
    for (size_t i = 0; i < sizeof tables / sizeof tables[0]; ++i) {
        g_fbk_active = tables[i];
        struct futex_bucket *b = fbk_of((const void *)(uintptr_t)uaddr);
        pthread_mutex_lock(&b->m);
        int registered = 0;
        (void)fbk_wait_grant(b, futex_key((const void *)(uintptr_t)uaddr), INT_MAX, ~0u, &registered);
        pthread_cond_broadcast(&b->c);
        pthread_mutex_unlock(&b->m);
    }
}

static volatile int g_next_tid = 1000;

// ---------------- live-thread registry (for thread-directed signals: tkill/tgkill) ----------------
// A tgkill()/tkill() names a specific guest tid; to deliver to THAT thread (and only it) we must map the
// tid back to its struct cpu and host pthread. Each thread (init + every spawned one) registers on entry
// to run_guest and unregisters when it leaves. Small fixed table guarded by a mutex; a lookup miss (target
// already gone, or table full) just drops the signal, exactly as Linux drops a tgkill to a dead tid.
#define THREAD_REG_MAX 4096

static struct {
    struct cpu *c;
    pthread_t th;
    // The mutex+condvar this thread is currently parked on in an interruptible futex wait (NULL if none), so
    // thread_target_signal can wake it out of pthread_cond_wait. waitc is the published flag (accessed via
    // __atomic); waitm points at a permanent (bucket / global) mutex, valid whenever waitc != NULL.
    pthread_cond_t *volatile waitc;
    pthread_mutex_t *waitm;
} g_threg[THREAD_REG_MAX];

// O(1) live-thread count, maintained under g_threg_m by thread_register/thread_unregister. Lets
// thread_after_fork() detect a SINGLE-THREADED parent (count <= 1) and skip the phantom-registry rebuild
// and the private-futex table reset -- both exist ONLY to repair state a vanished PEER thread could have
// left inconsistent across the guest fork (a held lock, a phantom tid entry, a stale parked-waiter slot).
// With no peer, none of those can occur: the inherited registry already holds only the calling thread and
// every private-futex bucket lock is unlocked with empty waiter slots, so the reset is pure overhead
// (~130us/fork: 256 bucket mutex+cond re-inits plus a 128KB registry memset). A threaded parent (count > 1)
// still takes the full reset. Correctness gate mirrors jit_after_fork's own single-threaded fast path.
static int g_threg_live;

static pthread_mutex_t g_threg_m = PTHREAD_MUTEX_INITIALIZER;

// fork() only clones the calling thread. Any process-PRIVATE engine mutex a dead peer held at the instant
// the guest forked is inherited LOCKED with no owner to release it, so the single-threaded child deadlocks
// the first time it takes that lock (the go/npm/cargo build hang). Reinitialise this module's
// private locks to a clean unlocked state in the child (the calling thread never holds one across a guest
// syscall, and no peer survives, so this is always safe). Only the PRIVATE futex table is reset; g_fbk lives
// in a PROCESS_SHARED MAP_SHARED page and must retain cross-fork FUTEX_WAKE/WAIT state (glibc process-shared
// semaphores). Called from the fork child path in proc.c.
static void thread_after_fork(void) {
    pthread_mutex_init(&g_threg_m, NULL); // thread registry (tkill/tgkill lookup, thread_register)
    struct cpu *self = (g_my_threg >= 0) ? g_threg[g_my_threg].c : NULL;
    pthread_mutex_init(&g_process_owner_lock, NULL);
    pthread_cond_init(&g_process_owner_cond, NULL);
    g_process_owner_cpu = self;
    g_process_exec_cpu = NULL;
    g_process_exec_pending = 0;
    g_process_exec_complete = 0;
    // The sole thread surviving fork becomes the child process leader. Linux
    // requires its tid to equal the new pid even when a non-leader parent
    // thread called fork. Keeping the caller's old tid gives unrelated child
    // processes duplicate thread identities and breaks process-shared users
    // that key ownership by tid.
    if (self) self->tid = 0;
    /*
     * Only the calling host thread survives a guest fork. A vanished peer may
     * have held either process-private file-map lock while publishing or
     * replaying shared mutations. The child owns a private copy of the
     * registry and cursor, so rebuild both locks before its first replay.
     */
    pthread_mutex_init(&g_filemap_lock, NULL);
    pthread_mutex_init(&g_filemap_replay_lock, NULL);
    // SINGLE-THREADED-PARENT FAST PATH: with no peer thread at fork, no private-futex bucket lock could have
    // been held (bucket locks are taken only transiently by the holder, never across a guest fork) and no
    // waiter could be parked, and the tid registry already lists only the calling thread. The entire reset
    // below (256 bucket re-inits + a 128KB registry memset, ~130us) exists solely to repair peer-induced
    // damage, so a lone forker can skip all of it -- the inherited state is already exactly correct. The
    // g_shkey_m lock re-init is likewise unnecessary (no peer to hold it), but it is O(1) so we keep it
    // unconditionally rather than reason about it per branch.
    pthread_mutex_init(&g_shkey_m, NULL);
    if (g_threg_live <= 1) {
        g_fbk_active = g_fbk_private; // the child's __thread active pointer must still select the private table
        return;
    }
    futex_private_table_after_fork();
    // fork() clones ONLY the calling thread, but the tid->thread registry still lists every PARENT thread.
    // Those phantom entries never unregister (no thread backs them in the child), so they (1) poison
    // tgkill/tkill routing -- thread_target_signal matches a phantom by tid (or pid 1 for the main thread)
    // and "delivers" the signal onto a dead cpu, dropping a real thread's SIGURG/preempt (Go's async
    // preemption then spins one goroutine at 100% while its peers park -- the livelock) -- and (2) make
    // the child's next execve teardown (thread_exit_others) busy-wait its full ~10s ceiling for phantom peers
    // that can never leave (the go build fork+exec stall: ~14s PER compile child, measured). Rebuild the
    // registry to hold ONLY the surviving (calling) thread, exactly as stw_after_fork() does for the STW
    // registry -- reinitialised this module's LOCKS but left the registry CONTENTS inherited.
    memset(g_threg, 0, sizeof g_threg);
    if (self) {
        g_threg[0].c = self;
        g_threg[0].th = pthread_self();
        g_my_threg = 0;
        g_threg_live = 1; // only the calling thread survives the fork
    } else {
        g_my_threg = -1;
        g_threg_live = 0;
    }
}

// A dedicated host signal used only to INTERRUPT a sibling guest thread out of a blocking host syscall
// (kevent/read/poll/nanosleep/...) so it observes cpu->exited and leaves the dispatcher -- see
// thread_exit_others (execve teardown). macOS has no realtime signals; SIGINFO(29) is unused by the guest
// signal map (sig_l2m omits 7/EMT and 29/INFO), so a process-wide handler for it cannot collide with an
// emulated guest signal -- the same free-signal reasoning the STW code uses for SIGEMT.
#define THREAD_INT_SIG SIGINFO

static void thread_int_handler(int sig) {
    (void)sig;
    // Its base job is to make a blocked host syscall return EINTR (empty body suffices). When checkpoint/
    // restore is armed, ALSO set cpu->irq so a chained in-cache guest loop (which never returns to the
    // dispatcher on its own) is bounced out to the safepoint where ckpt_poll runs. Inert on a normal launch
    // (the snapshot state is disabled), so the gate is unchanged; SIGINFO is guest-clobber-proof (sig_l2m omits 29).
    if (hl_linux_snapshot_enabled(&g_ckpt_snapshot)) {
        struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
        if (c) c->irq = 1;
    }
}

static pthread_once_t g_thread_int_once = PTHREAD_ONCE_INIT;

static void thread_int_install(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = thread_int_handler;
    sigemptyset(&sa.sa_mask);
    // Host AArch64 executes with the guest SP live.  Keep this engine-only
    // interrupt frame off that stack; NO SA_RESTART still makes a blocking
    // syscall return EINTR so its retry loop can bail on exited.
    sa.sa_flags = SA_ONSTACK;
    sigaction(THREAD_INT_SIG, &sa, NULL);
}

// The guest tid this cpu answers gettid() with (see proc.c case 178): its own id, or the init's pid 1.
static int cpu_tid(const struct cpu *c) {
    return c->tid ? c->tid : container_pid();
}

// Publish/clear the wait primitive this thread is blocked on (see the futex_op waits + thread_target_signal).
static void thread_wait_publish(pthread_mutex_t *m, pthread_cond_t *cnd) {
    if (g_my_threg < 0) return;
    g_threg[g_my_threg].waitm = m; // ordered ahead of the waitc store below (a reader only reads it when set)
    __atomic_store_n(&g_threg[g_my_threg].waitc, cnd, __ATOMIC_SEQ_CST);
}

static void thread_wait_clear(void) {
    if (g_my_threg < 0) return;
    __atomic_store_n(&g_threg[g_my_threg].waitc, NULL, __ATOMIC_SEQ_CST);
}

static void thread_register(struct cpu *c) {
    c->bus_filter = (uint64_t)(uintptr_t)g_bus_page_filter;
    c->bus_force = (uint64_t)(uintptr_t)&g_bus_filter_force;
    pthread_once(&g_thread_int_once, thread_int_install);
    // Keep THREAD_INT_SIG deliverable on this thread so a peer's execve teardown can interrupt its syscalls.
    sigset_t unb;
    sigemptyset(&unb);
    sigaddset(&unb, THREAD_INT_SIG);
    pthread_sigmask(SIG_UNBLOCK, &unb, NULL);
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (!g_threg[i].c) {
            g_threg[i].c = c;
            g_threg[i].th = pthread_self();
            __atomic_store_n(&g_threg[i].waitc, NULL, __ATOMIC_SEQ_CST);
            g_my_threg = i;
            g_threg_live++;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
}

static void thread_unregister(struct cpu *c) {
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c == c) {
            __atomic_store_n(&g_threg[i].waitc, NULL, __ATOMIC_SEQ_CST);
            g_threg[i].c = NULL;
            if (g_threg_live > 0) g_threg_live--;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    g_my_threg = -1;
}

// Deliver signal `sig` to the guest thread `tid`: set that thread's per-thread pending bit so it (and not
// some other thread) runs the handler at its next dispatcher safepoint. A thread that is preempted while
// running translated code (e.g. Go's sysmon tgkill'ing a worker with SIGURG to stop-the-world) crosses a
// dispatcher boundary continuously, so the per-thread pending is observed promptly without poking the host
// thread. But a thread PARKED in an interruptible futex wait (pthread_cond_wait) reaches no boundary on its
// own, so if the signal is deliverable now we wake it out of that wait (matching a real futex interrupted by
// a signal) -- without this, Go's doAllThreadsSyscall (setuid/setgid across all Ms, via signal 33) hangs
// because a parked sibling M never runs the per-thread-syscall handler the coordinator busy-waits on.
// Returns 1 if the target was found and flagged, 0 if no live thread carries that tid (caller then falls
// back to process semantics, as Linux drops a tgkill to a dead tid).
static int thread_target_signal(int tid, int sig) {
    int found = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c && cpu_tid(g_threg[i].c) == tid) {
            __atomic_or_fetch(&g_threg[i].c->tpending, 1ull << sig, __ATOMIC_SEQ_CST);
            // also kick the target out of any no-syscall in-cache loop so its emitted body check
            // (cpu->irq) exits to the dispatcher and maybe_deliver_signal runs the handler at a boundary.
            __atomic_store_n(&g_threg[i].c->irq, 1, __ATOMIC_SEQ_CST);
            // Load waitc AFTER storing tpending: the seq_cst StoreLoad here pairs with the target's
            // publish-then-recheck in futex_op so the wakeup is never lost. Only wake for a signal that is
            // actionable now (unblocked) -- a blocked one stays pending without interrupting the wait.
            if (cpu_has_actionable_tsig(g_threg[i].c)) {
                pthread_cond_t *cnd = __atomic_load_n(&g_threg[i].waitc, __ATOMIC_SEQ_CST);
                if (cnd) {
                    pthread_mutex_t *m = g_threg[i].waitm;
                    pthread_mutex_lock(m); // serialize with the target's pre-wait window; broadcast can't be lost
                    pthread_cond_broadcast(cnd);
                    pthread_mutex_unlock(m);
                } else if (!pthread_equal(g_threg[i].th, pthread_self()) &&
                           __atomic_load_n(&g_threg[i].c->in_service, __ATOMIC_SEQ_CST)) {
                    // No published futex wait: the target is either running translated code (cpu->irq already
                    // bounces it to a dispatcher boundary) or PARKED IN A BLOCKING HOST SYSCALL (read/accept/
                    // recv/poll/nanosleep/...), which reaches no boundary on its own. Poke it with
                    // THREAD_INT_SIG (no SA_RESTART) so that syscall returns EINTR; syscall_should_restart
                    // (now tpending-aware) then declines to restart and the guest sees EINTR + the delivered
                    // signal. Harmless (near-empty handler) if the target was in-cache. Skip a self-signal.
                    pthread_kill(g_threg[i].th, THREAD_INT_SIG);
                }
            }
            found = 1;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    return found;
}

// Does the live guest thread `tid` currently BLOCK signal `sig`? A thread-directed tkill/tgkill of a signal
// the target has blocked must be held pending on THAT specific thread -- so the thread's own sigwait/
// sigtimedwait dequeues it (or it is delivered when the thread unblocks) -- rather than being dropped into
// the process-wide g_pending, where any thread (often the sender) could consume it. That misrouting is what
// left a pthread_kill()+sigwait target hung / a peer thread waking instead of the addressed one.
static int thread_tid_blocks_signal(int tid, int sig) {
    if (sig < 1 || sig > 64) return 0;
    int blocked = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c && cpu_tid(g_threg[i].c) == tid) {
            blocked = (g_threg[i].c->sigmask & (1ull << (sig - 1))) != 0;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    return blocked;
}

// Is `tid` a LIVE guest thread of this process? tkill/tgkill (syscall/signal.c) use it to return ESRCH for
// a tid no thread carries -- e.g. a joined/exited thread whose id LTP tgkill03 reuses ("defunct tid"). The
// process shares one thread-group, so a tid absent from the registry is gone. (The caller's own tid is
// checked separately at the call site: it is always live even if not yet enumerated here.)
static int thread_tid_alive(int tid) {
    int alive = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c && cpu_tid(g_threg[i].c) == tid) {
            alive = 1;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    return alive;
}

// Count of currently-live guest threads of THIS process (main + every spawned one still in run_guest). The
// /proc/<self>/task st_nlink synth reports 2 + this (Linux: `.`, `..`, one subdir per thread) so guest
// sandbox `IsSingleThreaded` (fstatat st_nlink == 3) and per-tid `IsThreadPresentInProcFS` (fstatat ENOENT
// on a joined/exited thread) both track the real thread set -- otherwise a process's thread helpers
// spins 30 iterations waiting for a stopped thread's /proc/self/task/<tid> to disappear, then FATALs.
static int thread_live_count(void) {
    int n = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c) n++;
    pthread_mutex_unlock(&g_threg_m);
    return n < 1 ? 1 : n; // the caller is always itself a live thread even in a registration window
}

// Fill `out` with the tids of every live guest thread of THIS process (the registered spawned threads;
// the main/init thread carries tid 0 in the registry and reports `main_tid` to the guest, so callers pass
// their own pid as main_tid to substitute it). Returns the count written (<= max). Used to enumerate
// /proc/<self>/task so a directory walk sees every live TID, not just the main thread.
static int thread_tid_list(int *out, int max, int main_tid) {
    int n = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX && n < max; i++) {
        if (!g_threg[i].c) continue;
        int tid = cpu_tid(g_threg[i].c);
        if (tid == 0) tid = main_tid; // init thread -> its guest-visible tid (== pid)
        int dup = 0;
        for (int j = 0; j < n; j++)
            if (out[j] == tid) {
                dup = 1;
                break;
            }
        if (!dup) out[n++] = tid;
    }
    pthread_mutex_unlock(&g_threg_m);
    return n;
}

// execve makes the process single-threaded: the kernel terminates every OTHER thread in the group before the
// new image runs. The JIT re-loads the new image IN-PROCESS, so we must do that teardown by hand -- BEFORE
// flushing the address space and closing CLOEXEC fds -- or a surviving sibling M keeps running the old image
// against freed state (e.g. Go's netpoller M, parked in epoll_wait, crashes with EBADF the instant execve
// closes its epoll fd; every postgres/mysql/mariadb entrypoint `exec gosu ...` right past a Go all-threads
// setuid, so this is on the DB showcase path). Flag every peer exited, wake a futex-parked one and interrupt
// any other blocking host syscall (or nudge one running translated code), and BLOCK until all peers have left
// run_guest and unregistered -- only then is it safe for the caller to munmap/flush the shared address space.
static void thread_exit_others(struct cpu *self) {
    struct timespec slice = {0, 500000};          // 0.5ms between rounds; re-signal each round to catch a peer that was
    for (int round = 0; round < 20000; round++) { // between syscalls when we first flagged it (~10s ceiling)
        int others = 0;
        pthread_mutex_lock(&g_threg_m);
        for (int i = 0; i < THREAD_REG_MAX; i++) {
            struct cpu *tc = g_threg[i].c;
            if (!tc || tc == self) continue;
            others++;
            __atomic_store_n(&tc->exited, 1, __ATOMIC_SEQ_CST);
            pthread_cond_t *cnd = __atomic_load_n(&g_threg[i].waitc, __ATOMIC_SEQ_CST);
            if (cnd) { // parked in a futex wait -> wake it (see thread_target_signal)
                pthread_mutex_t *m = g_threg[i].waitm;
                pthread_mutex_lock(m);
                pthread_cond_broadcast(cnd);
                pthread_mutex_unlock(m);
            }
            pthread_kill(g_threg[i].th, THREAD_INT_SIG); // EINTR any other blocking host syscall
        }
        pthread_mutex_unlock(&g_threg_m);
        if (!others) return;
        nanosleep(&slice, NULL);
    }
}

// ---- robust futex list (set_robust_list / thread-exit cleanup) ----------------------------------------
// A thread that dies while holding a robust mutex must not wedge its waiters forever: the kernel walks the
// thread's registered robust list on exit and, for each futex it still owns, sets FUTEX_OWNER_DIED and wakes
// one waiter so a blocked peer returns EOWNERDEAD (and can pthread_mutex_consistent the lock) instead of
// hanging. hl did neither (set_robust_list was a no-op), so a crash/exit under a PTHREAD_MUTEX_ROBUST lock
// deadlocked every waiter. Layout (LP64 kernel/glibc ABI, 24-byte head):
//   struct robust_list_head { void *list; long futex_offset; void *list_op_pending; };
// `list` chains each mutex's embedded robust_list node (first word = next; LSB is a PI flag we mask off) and
// terminates by pointing back at &head->list (== head, list is at offset 0). The futex word for a node is at
// node + futex_offset. list_op_pending covers a mutex mid-(un)lock and is handled once, at the end.
#define HL_ROBUST_LIST_LIMIT 2048

// Robust-list links are guest pointers read directly from guest memory, so they do not pass through the
// syscall dispatcher's pointer translation. A static ET_EXEC can therefore put a low link address in a
// high-mapped list head. Translate each link before validating or dereferencing it; for PIE, heap, stack,
// and mmap pointers this is an identity operation.
static inline uint64_t robust_guest_to_host(uint64_t address) {
    return nonpie_fold(address);
}

static int robust_pin(uint64_t address, size_t length, uint32_t protection, hl_logical_vma_pin *pin, void **host) {
    memset(pin, 0, sizeof(*pin));
    int logical = hl_logical_vma_pin_data(address, length, protection, pin);
    if (logical < 0 || pin->contiguous < length) {
        hl_logical_vma_unpin(pin);
        return 0;
    }
    if (logical > 0) {
        *host = pin->host;
        return 1;
    }
    if (!host_range_mapped((uintptr_t)address, length)) {
        hl_logical_vma_unpin(pin);
        return 0;
    }
    *host = (void *)(uintptr_t)address;
    return 1;
}

static int robust_copy_from(void *destination, uint64_t source, size_t length) {
    hl_logical_vma_pin pin;
    void *host;
    if (!robust_pin(source, length, HL_LOGICAL_VMA_READ, &pin, &host)) return 0;
    memcpy(destination, host, length);
    hl_logical_vma_unpin(&pin);
    return 1;
}

// If the dying thread still owns *futex_addr, set FUTEX_OWNER_DIED (preserving FUTEX_WAITERS) and wake one
// waiter. cmpxchg-loops so a concurrent lock/unlock on the same word can't clobber the OWNER_DIED marking.
static void robust_handle_death(uint64_t futex_addr, int mytid) {
    hl_logical_vma_pin pin;
    void *host;
    if (!futex_addr || !robust_pin(futex_addr, 4, HL_LOGICAL_VMA_WRITE, &pin, &host)) return;
    int *w = host;
    int v = __atomic_load_n(w, __ATOMIC_SEQ_CST);
    for (;;) {
        if (((uint32_t)v & HL_FUTEX_TID_MASK) != (uint32_t)mytid) break; // not (or no longer) ours
        int nv = (int)(((uint32_t)v & HL_FUTEX_WAITERS) | HL_FUTEX_OWNER_DIED);
        if (__atomic_compare_exchange_n(w, &v, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
            if ((uint32_t)v & HL_FUTEX_WAITERS) futex_wake_bucket(w, 1, ~0u, 0); // one waiter -> EOWNERDEAD
            break;
        }
        // v was reloaded with the current word by the failed cmpxchg -> re-check ownership and retry
    }
    hl_logical_vma_unpin(&pin);
}

// Walk this thread's robust list (if any) and mark+wake each still-owned mutex. Clears c->robust_list so a
// second call (thread exit then process exit) is a no-op. Every guest pointer is bounds-checked before deref.
static void futex_robust_exit(struct cpu *c) {
    // Like clear-child-tid, Linux's kernel-driven robust-list wake has no FUTEX_PRIVATE_FLAG and uses the
    // shared key class. Keep it aligned with glibc's robust owner-death wait path.
    g_fbk_active = g_fbk;
    // The head is a guest pointer like every list entry below it (set_robust_list stores it unfolded).
    // Left raw, an in-image head simply failed robust_copy_from and the whole walk was silently skipped --
    // and the `entry == head` wrap test compared a folded entry against an unfolded head.
    uint64_t head = robust_guest_to_host(c->robust_list);
    c->robust_list = 0;
    uint8_t head_copy[24];
    if (!head || !robust_copy_from(head_copy, head, sizeof(head_copy))) return;
    uint64_t raw_first, raw_pending;
    long futex_offset;
    memcpy(&raw_first, head_copy, sizeof(raw_first));
    memcpy(&futex_offset, head_copy + 8, sizeof(futex_offset));
    memcpy(&raw_pending, head_copy + 16, sizeof(raw_pending));
    uint64_t pending = robust_guest_to_host(raw_pending & ~1ULL); // head->list_op_pending
    int mytid = cpu_tid(c);
    uint64_t entry = robust_guest_to_host(raw_first & ~1ULL);
    for (int limit = 0; limit < HL_ROBUST_LIST_LIMIT; limit++) {
        if (entry == head) break; // wrapped back to &head->list -> done
        uint64_t raw_next;
        if (!robust_copy_from(&raw_next, entry, sizeof(raw_next))) break;
        uint64_t next = robust_guest_to_host(raw_next & ~1ULL); // entry->next
        if (entry != pending) robust_handle_death(entry + (uint64_t)futex_offset, mytid);
        entry = next;
    }
    if (pending && pending != head) robust_handle_death(pending + (uint64_t)futex_offset, mytid);
}

static void *thread_trampoline(void *p) {
    struct cpu *child = (struct cpu *)p;
    sentry_thread_enter(child);
    // sets its own TSD, runs to thread exit
    run_guest(child);
    int exit_status = child->exit_code;
    sentry_thread_leave();
    // cgroup pids: task ended
    atomic_fetch_sub(&g_pids_cur, 1);
    acct_publish_tasks(); // update this process's container-wide task-count contribution
    // robust mutexes this thread still holds -> mark OWNER_DIED + wake a waiter (before the join wakeup)
    futex_robust_exit(child);
    // pthread_join waits on this
    futex_wake_addr(child->ctid);
    thread_exec_owner_complete(child, exit_status);
    (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)cpu_tid(child), 0);
    free(child);
    return NULL;
}

/* Resume every saved non-leader CPU image as a peer host thread. Checkpoint restore has already sanitized
 * host-transient fields while preserving architectural state, signal state, TLS, TID and clear-child-tid. */
static int thread_restore_group(const struct cpu *images, int count, const struct cpu *leader) {
    if (!images || !leader || count < 1) return -EINVAL;
    int peers = 0;
    int highest_tid = leader->tid;
    for (int i = 0; i < count; i++) {
        if (images[i].tid > highest_tid) highest_tid = images[i].tid;
        if (images[i].tid != 0) peers++;
    }
    if (highest_tid > g_next_tid) g_next_tid = highest_tid;
    if (!peers) return 0;
    txln_activate();
    // See spawn_thread: discard single-threaded (barrier-elided) blocks before the restored peers run.
    if (!g_threaded && !G_THREAD_START_FLUSH()) return -EAGAIN;
    g_threaded = 1;
    for (int i = 0; i < count; i++) {
        if (images[i].tid == 0) continue;
        struct cpu *child = malloc(sizeof *child);
        if (!child) return -ENOMEM;
        *child = images[i];
        if (sentry_thread_prepare(child) != 0) {
            free(child);
            return -EAGAIN;
        }
        pthread_t thread;
        if (pthread_create(&thread, NULL, thread_trampoline, child) != 0) {
            sentry_thread_cancel(child);
            free(child);
            return -EAGAIN;
        }
        atomic_fetch_add(&g_pids_cur, 1);
        acct_publish_tasks();
        pthread_detach(thread);
    }
    return 0;
}

// Spawn a guest thread sharing this address space. stack_top is the initial sp.
static int spawn_thread(struct cpu *parent, uint64_t flags, uint64_t stack_top, uint64_t tls, uint64_t ptid,
                        uint64_t ctid) {
    // cgroup pids.max -- gated on the CONTAINER-WIDE task count (all engine processes), not just this
    // process's threads, so the limit is one shared budget across the whole process tree.
    if (g_pids_max && acct_pids_total() >= g_pids_max) return -EAGAIN;
    struct cpu *child = malloc(sizeof *child);
    // ENOMEM
    if (!child) return -12;
    *child = *parent;
    // child sees clone return 0
    G_RET(child) = 0;
    G_SP(child) = stack_top;
    // resume just after the clone svc
    G_THREAD_RESUME(child, parent);
    // §B: child starts with an EMPTY shadow stack (no parent frames)
    G_SHADOW_RESET(child);
    G_SMC_QUEUE_RESET(child);
    /* clone() inherits architectural register state and the signal mask, but
       these fields describe an in-flight engine operation on the parent host
       thread.  Copying them can deliver the parent's synchronous fault to the
       child or resume a BUS/service handoff with stale scratch state. */
    child->irq = 0;
    child->tpending = 0;
    child->sync_signal = 0;
    child->sync_code = 0;
    child->sync_address = 0;
    child->vdirty = 0;
    child->fault_addr = 0;
    child->bus_ea = 0;
    child->in_service = 0;
    child->exited = 0;
    child->redirect = 0;
#ifdef G_SOFT_STATE_RESET
    G_SOFT_STATE_RESET(child);
#endif
    // CLONE_SETTLS
    if (flags & 0x00080000) G_TLS(child) = tls;
    int tid = __sync_add_and_fetch(&g_next_tid, 1);
    // This thread's gettid() identity (see proc.c case 178): a unique id, distinct from the init's pid 1.
    child->tid = tid;
    if (!hl_target_task_event(parent, HL_TASK_EVENT_CLONE_THREAD, (uint64_t)tid, (uint64_t)cpu_tid(parent), 0)) {
        free(child);
        return -EAGAIN;
    }
    // CLONE_PARENT_SETTID / CLONE_CHILD_SETTID. clone(2)'s tid slots are ordinary guest pointers and a
    // non-PIE guest may hand over a .bss one, so fold for the store (thread.c's rule); c->ctid keeps the
    // guest value, and futex_wake_addr folds again at thread exit.
    if ((flags & 0x00100000) && ptid) *(int *)nonpie_fold(ptid) = tid;
    if ((flags & 0x01000000) && ctid) *(int *)nonpie_fold(ctid) = tid;
    // CLONE_CHILD_CLEARTID
    child->ctid = (flags & 0x00200000) ? ctid : 0;
    // robust list is per-thread and NOT inherited: a new thread starts empty and re-registers via
    // set_robust_list itself (otherwise the copied parent head would be walked twice on exit).
    child->robust_list = 0;
    // A new CLONE_VM thread starts with no alternate signal stack (Linux
    // sigaltstack(2)); it installs its own stack after startup when needed.
    child->alt_sp = 0;
    child->alt_size = 0;
    child->alt_flags = 2; /* SS_DISABLE */
    // A peer thread may self-modify code; arm eager line-set recording (and back-fill the lines of every
    // block translated so far) NOW, while still single-threaded, so the set is complete before any peer runs.
    txln_activate();
    // 0->1 transition: while STILL single-threaded (no peer exists yet), flush the code cache so any block
    // translated under the single-threaded x86-TSO-barrier-elision regime is discarded and re-translated
    // WITH barriers before this new peer can execute a guest memory op. Only on the transition -- a later
    // clone (g_threaded already 1) must NOT reset the arena in place under live peers. See emit.c /
    // hl_x86_flush_for_thread_start.
    if (!g_threaded && !G_THREAD_START_FLUSH()) {
        (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)tid, 0);
        free(child);
        return -EAGAIN;
    }
    g_threaded = 1;
    if (sentry_thread_prepare(child) != 0) {
        (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)tid, 0);
        free(child);
        return -EAGAIN;
    }
    pthread_t th;
    if (pthread_create(&th, NULL, thread_trampoline, child) != 0) {
        sentry_thread_cancel(child);
        (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)tid, 0);
        free(child);
        return -EAGAIN;
    }
    // cgroup pids: task created
    atomic_fetch_add(&g_pids_cur, 1);
    acct_publish_tasks(); // update this process's container-wide task-count contribution
    pthread_detach(th);
    return tid;
}
