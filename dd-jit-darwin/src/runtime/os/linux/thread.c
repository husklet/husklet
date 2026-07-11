// dd/runtime/os/linux -- threads & futex (clone -> pthread; per-thread cpu; futex via condvars).

#include <mach/mach.h>
#include <mach/mach_vm.h>       // mach_vm_region: probe whether a guest address is still mapped (see cleartid)
#include <mach/mach_time.h>     // mach_timebase_info: ns<->mach-abs for the precise-sleep RT window
#include <mach/thread_policy.h> // THREAD_TIME_CONSTRAINT_POLICY: precise (uncoalesced) timer wakeups

// macOS coalesces ordinary timer wakeups by ~1-2.5ms to save power (nanosleep/mach_wait_until alike),
// which blows LTP nanosleep01's 450us threshold -- Linux hrtimers are exact. A THREAD_TIME_CONSTRAINT
// (soft real-time) policy makes THIS thread's next wakeup precise (~10us). We apply it only for the
// duration of a guest sleep and drop back to the standard timeshare policy after, so the thread's normal
// scheduling is unchanged outside the sleep and no thread is left permanently real-time.
static void sleep_precise_begin(void) {
    mach_timebase_info_data_t tb;
    if (mach_timebase_info(&tb) != KERN_SUCCESS || tb.numer == 0) return;
    double ns2abs = (double)tb.denom / (double)tb.numer; // nanoseconds -> mach abs ticks
    thread_time_constraint_policy_data_t p;
    p.period = (uint32_t)(500000.0 * ns2abs);      // 0.5ms nominal cadence
    p.computation = (uint32_t)(100000.0 * ns2abs); // 0.1ms of "work" (we only need a timely wake)
    p.constraint = (uint32_t)(500000.0 * ns2abs);  // wake within 0.5ms of the deadline
    p.preemptible = 1;                             // fully preemptible: never starves other threads
    thread_policy_set(mach_thread_self(), THREAD_TIME_CONSTRAINT_POLICY, (thread_policy_t)&p,
                      THREAD_TIME_CONSTRAINT_POLICY_COUNT);
}

static void sleep_precise_end(void) {
    thread_standard_policy_data_t sp = {0}; // back to the default timeshare scheduling
    thread_policy_set(mach_thread_self(), THREAD_STANDARD_POLICY, (thread_policy_t)&sp, THREAD_STANDARD_POLICY_COUNT);
}

// ---------------- syscalls ----------------
// ---------------- threads & futex ----------------
// fwd: thread trampoline runs the dispatcher
static void run_guest(struct cpu *c);

// ---------------- futex: per-address hashed wait queues ----------------
// Legacy (NOFUTEXQ=1): ONE global mutex + condvar. Correct but a WAKE on ANY address takes
// the global lock and broadcasts EVERY waiter on EVERY address (thundering herd) -> the real
// multi-thread DB bottleneck. The S3 uncontended fast path helped only the no-sleeper case.
//
// W5C (default): a fixed table of per-address buckets {mutex, condvar, waiter-count}, keyed by
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

struct futex_bucket {
    pthread_mutex_t m;
    pthread_cond_t c;
    _Atomic int waiters;           // aggregate parked count in this bucket (PROF + imprecise fallback)
    uintptr_t saddr[FUTEX_ASLOTS]; // distinct uaddrs with >=1 parked waiter (0 == free slot)
    uint32_t scnt[FUTEX_ASLOTS];   // parked-waiter count for saddr[i]
    uint32_t sbits[FUTEX_ASLOTS];  // OR of the FUTEX_WAIT_BITSET masks parked on saddr[i] (plain WAIT = ~0)
    int imprecise;                 // slots overflowed while waiters were parked -> WAKE count approximate
};

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
// PTHREAD_PROCESS_SHARED, so a FUTEX_WAKE in one process matches a FUTEX_WAIT in another across dd's
// fork() -- e.g. a glibc process-shared (named/unnamed-on-shm) semaphore where the child sem_post()s
// and the parent sem_wait()s. dd's fork() is a real host fork(): the child inherits the identical guest
// address space, so a shared-memory futex word resolves to the SAME host address in parent and child
// and both hash to the same bucket, while the underlying MAP_SHARED guest page is one physical page.
// The table is created ONCE at engine startup (constructor, before any guest fork) so every forked
// worker inherits the same physical buckets. The lock-free no-sleeper WAKE fast path is unchanged --
// only the slow path (a real sleeper exists) touches the now-cross-process mutex/condvar. In-process
// (multi-threaded) futexes still hit the same table, keyed by their shared virtual address, as before.
static struct futex_bucket *g_fbk;

static void futex_table_init(void) {
    if (g_fbk) return;
    size_t sz = sizeof(struct futex_bucket) * FUTEX_NBUCKET;
    void *mem = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (mem == MAP_FAILED) // cross-process wakeups degrade, but in-process futexes still work
        mem = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (mem == MAP_FAILED) abort();
    struct futex_bucket *t = (struct futex_bucket *)mem;
    pthread_mutexattr_t ma;
    pthread_condattr_t ca;
    pthread_mutexattr_init(&ma);
    pthread_condattr_init(&ca);
    pthread_mutexattr_setpshared(&ma, PTHREAD_PROCESS_SHARED);
    pthread_condattr_setpshared(&ca, PTHREAD_PROCESS_SHARED);
    for (int i = 0; i < FUTEX_NBUCKET; i++) {
        pthread_mutex_init(&t[i].m, &ma);
        pthread_cond_init(&t[i].c, &ca);
        atomic_store_explicit(&t[i].waiters, 0, memory_order_relaxed);
    }
    pthread_mutexattr_destroy(&ma);
    pthread_condattr_destroy(&ca);
    g_fbk = t;
}

__attribute__((constructor)) static void futex_table_ctor(void) {
    futex_table_init();
}

// ===================== shared-memory futex key (Linux "shared" futex semantics) =================
// dd hashes a futex bucket by the WORD's host virtual address. That is exactly Linux's PRIVATE futex key
// (mm + address) and is correct for anon/private words -- including a fork-inherited MAP_SHARED page, which
// lands at the SAME VA in parent and child. But a file-backed MAP_SHARED object (memfd, shm) is mapped
// INDEPENDENTLY by each peer: Chrome's renderer and GPU-service map the command-buffer shmem at DIFFERENT
// addresses, so the SAME physical futex word has a different VA in each. Linux keys such a word by the
// SHARED object identity (inode + page offset), so a FUTEX_WAKE through one mapping reaches a FUTEX_WAIT
// parked through another. dd's VA-only key put the two in different buckets and LOST the wake -- the
// documented "Wall 7": the renderer's command-buffer flush never woke the GPU service, so page content was
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
    return &g_fbk[h];
}

// legacy global queue (NOFUTEXQ=1)
static pthread_mutex_t g_futex_m = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t g_futex_c = PTHREAD_COND_INITIALIZER;
// Aggregate parked count for the legacy single-queue path, so its FUTEX_WAKE can report a woken count
// (min(val, parked)) instead of the requested `val`. Approximate (one global queue, no per-address split)
// but correct for the common single-futex case; the default W5C path counts per-address exactly.
static int g_futex_parked;
// PROF: fast (no-lock) wakes, slow (locked) wakes, eagain pre-checks
static uint64_t g_futex_wake_fast, g_futex_wake_slow, g_futex_wait_n;

// ===================== guest PROT_NONE region registry ==========================================
// dd maps every guest anon page R+W on the host (case 222 ORs in PROT_READ|WRITE) so that a later
// mprotect-to-writable -- which dd no-ops, since the JIT never enforces guest page protection -- is
// already in effect. A consequence: a guest mapping the guest genuinely made INACCESSIBLE (mmap
// PROT_NONE, e.g. LTP's tst_get_bad_addr, a malloc-arena guard page, a Go/V8 reservation) is still
// PHYSICALLY readable, so host_range_mapped's page probe wrongly reports it mapped -- and a syscall
// whose user buffer lands there returns success instead of -EFAULT (LTP sched_getaffinity01's EFAULT
// case). Track the guest-requested PROT_NONE ranges so host_range_mapped can fault them exactly as the
// kernel's copy_to_user would. Lock-free like g_gmap/g_anonmap (mem.c): a race can at worst mis-window a
// concurrently-changing mapping, never corrupt memory. Updated by mmap/mprotect/munmap in mem.c.
#define GNA_MAX 512

static struct {
    uint64_t lo, hi;
} g_gna[GNA_MAX];

static int g_ngna;
static void gna_clear(uint64_t lo, uint64_t hi);

static void gna_add(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    gna_clear(lo, hi); // coalesce: drop any prior coverage so re-marking never double-counts
    if (g_ngna < GNA_MAX) {
        g_gna[g_ngna].lo = lo;
        g_gna[g_ngna].hi = hi;
        __atomic_store_n(&g_ngna, g_ngna + 1, __ATOMIC_RELEASE);
    }
}

// Remove [lo,hi) from the set (access granted, or the range unmapped/re-mapped), splitting any interval
// that straddles the boundary so a partial grant (mprotect of a sub-range of a big PROT_NONE reservation)
// keeps the still-inaccessible remainder tracked.
static void gna_clear(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    for (int i = 0; i < g_ngna;) {
        uint64_t b = g_gna[i].lo, e = g_gna[i].hi;
        if (lo >= e || hi <= b) {
            i++;
            continue;
        }
        int keep_head = b < lo, keep_tail = hi < e;
        if (!keep_head && !keep_tail) {
            g_gna[i] = g_gna[--g_ngna];
            continue;
        }
        if (keep_head)
            g_gna[i].hi = lo; // trim to the surviving head [b,lo)
        else
            g_gna[i].lo = hi;                             // keep_tail only: [hi,e)
        if (keep_head && keep_tail && g_ngna < GNA_MAX) { // middle grant -> tail becomes a 2nd entry
            g_gna[g_ngna].lo = hi;
            g_gna[g_ngna].hi = e;
            __atomic_store_n(&g_ngna, g_ngna + 1, __ATOMIC_RELEASE);
        }
        i++;
    }
}

// True iff any byte of [a,a+len) lies in a tracked guest PROT_NONE region.
static int gna_hit(uint64_t a, uint64_t len) {
    if (!len || __atomic_load_n(&g_ngna, __ATOMIC_ACQUIRE) == 0) return 0; // lock-free fast path (common)
    uint64_t end = a + len;
    for (int i = 0; i < g_ngna; i++)
        if (a < g_gna[i].hi && end > g_gna[i].lo) return 1;
    return 0;
}

// execve replaces the whole address space -> drop all tracked PROT_NONE ranges (they're gone with the old
// image; a stale entry could otherwise wrongly EFAULT a fresh mapping the new image lays at the same address).
static void gna_reset(void) {
    __atomic_store_n(&g_ngna, 0, __ATOMIC_RELEASE);
}

// True iff host virtual address `a` is currently mapped. mincore() is useless on macOS (returns 0 for ANY
// address), so query the VM map directly: mach_vm_region returns the first region at-or-above `a`, and `a`
// is mapped iff it falls inside [start, start+size). Same technique as the x86 loader's lazy_addr_mapped.
// Used to mirror the kernel's fault-tolerant put_user() on the CLEARTID teardown path (futex_wake_addr).
static int host_addr_mapped(uintptr_t a) {
    mach_vm_address_t addr = a;
    mach_vm_size_t size = 0;
    vm_region_basic_info_data_64_t info;
    mach_msg_type_number_t cnt = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t obj = MACH_PORT_NULL;
    if (mach_vm_region(mach_task_self(), &addr, &size, VM_REGION_BASIC_INFO_64, (vm_region_info_t)&info, &cnt, &obj) !=
        KERN_SUCCESS)
        return 0; // nothing at/above -> unmapped
    return a >= (uintptr_t)addr && a < (uintptr_t)addr + (uintptr_t)size;
}

// per-thread ALTERNATE signal stack for the synchronous-fault guards. On the aarch64 frontend the
// host SP == the guest SP while a translated block runs, so a guest STACK OVERFLOW leaves no room for the
// kernel to push the SIGSEGV/SIGBUS guard's signal frame -- without an altstack the handler double-faults
// and the guest dies of a spurious SIGILL/SIGBUS instead of a clean, guard-delivered SIGSEGV. Installed
// once per thread (main + every guest thread) from run_guest, before any guest code executes, and torn down
// at run_guest exit. (x86 keeps host SP != guest SP, so its guards don't take SA_ONSTACK and never use it;
// the reservation is uncommitted there.)
#define HOST_ALTSTK_SZ (512u << 10)
static _Thread_local void *g_altstk_mem;

// Idempotent: (re)registers the alternate signal stack for THIS thread, allocating one on first use and
// reusing the existing region otherwise. The sigaltstack() registration is not reliably inherited across
// fork() on Apple Silicon (like the W^X/APRR state -- see fork_child_hooks), so the fork child re-arms via
// this same call with its COW-inherited region.
static void install_host_sigaltstack(void) {
    void *mem = g_altstk_mem;
    if (!mem) {
        mem = mmap(NULL, HOST_ALTSTK_SZ, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
        if (mem == MAP_FAILED) return;
    }
    stack_t ss = {.ss_sp = mem, .ss_size = HOST_ALTSTK_SZ, .ss_flags = 0};
    if (sigaltstack(&ss, NULL) != 0) {
        if (mem != g_altstk_mem) munmap(mem, HOST_ALTSTK_SZ);
        return;
    }
    g_altstk_mem = mem;
}

static void uninstall_host_sigaltstack(void) {
    if (!g_altstk_mem) return;
    stack_t ss = {.ss_flags = SS_DISABLE};
    sigaltstack(&ss, NULL);
    munmap(g_altstk_mem, HOST_ALTSTK_SZ);
    g_altstk_mem = 0;
}

// Range form of host_addr_mapped: true iff every page spanning [a, a+len) is mapped. Used to validate a
// guest-supplied syscall buffer (a result struct to write, an argument struct to read) BEFORE dereferencing
// it, so a bad/garbage user pointer returns -EFAULT to the guest instead of faulting the engine (the
// kernel's access_ok() role). A zero length is vacuously OK; an address-space-wrapping range is rejected.
//
// PERF (sqlite/fcntl): the original implementation issued one mach_vm_region -- a full Mach
// message round-trip (~200ns+) -- PER PAGE PER CALL. `sample` showed ~97% of the dd-side overhead of the
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
// exception port intercepts EXC_BAD_ACCESS before the POSIX guards) and DDJIT_NOFASTHRM=1 keep the
// byte-identical mach_vm_region path.
#include <setjmp.h>
static _Thread_local sigjmp_buf g_hrm_jb;                   // probe return point (valid while g_hrm_hi != 0)
static _Thread_local volatile uintptr_t g_hrm_lo, g_hrm_hi; // page range being probed; probing iff hi != 0
static int g_hrm_slow = -1; // DDJIT_NOFASTHRM=1 / CRASHDBG -> per-page mach_vm_region (A/B kill switch)

// Called FIRST by every SIGSEGV/SIGBUS handler on the run path: when the fault is this thread's own probe
// load, long-jump back to host_range_mapped ("unmapped"). The faulting signal was auto-blocked at handler
// entry and siglongjmp(.,0) does not restore masks, so unblock it here or the NEXT probe fault would be
// force-killed instead of caught. Returns 0 (fault not ours) in every other case.
static int hrm_fault_hook(siginfo_t *si) {
    if (!g_hrm_hi) return 0;
    uintptr_t va = (uintptr_t)(si ? si->si_addr : NULL);
    if (va < g_hrm_lo || va >= g_hrm_hi) return 0; // not the probe access -> normal fault handling
    sigset_t s;
    sigemptyset(&s);
    sigaddset(&s, SIGSEGV);
    sigaddset(&s, SIGBUS);
    pthread_sigmask(SIG_UNBLOCK, &s, NULL);
    siglongjmp(g_hrm_jb, 1); // never returns
}

static int host_range_mapped(uintptr_t a, size_t len) {
    if (!len) return 1;
    uintptr_t end = a + len;
    if (end < a) return 0; // wrap -> bogus pointer
    // A guest PROT_NONE mapping is physically R+W under dd (see the g_gna registry above), so the page
    // probe below would call it mapped; the kernel's copy_to/from_user faults it. Reject up front.
    if (gna_hit((uint64_t)a, (uint64_t)len)) return 0;
    uintptr_t lo = a & ~(uintptr_t)0xfff;
    if (g_hrm_slow < 0) g_hrm_slow = (getenv("DDJIT_NOFASTHRM") || getenv("CRASHDBG")) ? 1 : 0;
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
        for (uintptr_t p = lo; p < end; p += 0x1000)
            (void)*(volatile const uint8_t *)p;
    }
    g_hrm_lo = 0;
    g_hrm_hi = 0; // probe window closed (hook inert again)
    return ok;
}

static void abs_from_rel(struct timespec *abs, const struct timespec *ts) {
    clock_gettime(CLOCK_REALTIME, abs);
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
    clock_gettime(CLOCK_REALTIME, &rt);
    clock_gettime(CLOCK_MONOTONIC, &mono);
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
    return __atomic_load_n(&c->exited, __ATOMIC_SEQ_CST) || cpu_has_actionable_tsig(c);
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
static int futex_wake_bucket(const int *uaddr, int n, uint32_t match) {
    struct futex_bucket *b = fbk_of(uaddr);
    if (g_prof) {
        if (atomic_load_explicit(&b->waiters, memory_order_relaxed))
            g_futex_wake_slow++;
        else
            g_futex_wake_fast++;
    }
    pthread_mutex_lock(&b->m);
    // FUTEX_WAKE_BITSET only wakes waiters whose bitset overlaps `match`; a plain FUTEX_WAKE passes ~0u.
    // If no parked waiter on this address can match, wake nobody (Linux does not disturb them).
    if (!fbk_match(b, futex_key(uaddr), match)) {
        pthread_mutex_unlock(&b->m);
        return 0;
    }
    int woke = fbk_parked(b, futex_key(uaddr));
    if (woke > n) woke = n;
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
// the owner exited still holding it). dd does not model priority BOOSTING (a latency/QoS property, not a
// correctness one), but it enforces real MUTUAL EXCLUSION and the exact futex-word contract glibc's userspace
// fast paths depend on -- so two threads can never both believe they own a PTHREAD_PRIO_INHERIT/robust mutex
// (the old return-0 fake-acquire silently let them into the critical section together -> data corruption).
#define DD_FUTEX_WAITERS 0x80000000u
#define DD_FUTEX_OWNER_DIED 0x40000000u
#define DD_FUTEX_TID_MASK 0x3fffffffu

static int cpu_tid(const struct cpu *c);

// Acquire the PI mutex at uaddr for this thread (block until free, unless `trylock`). On success writes
// this thread's TID -- OR'd with FUTEX_WAITERS when other threads remain queued on the same word -- into the
// futex word and returns 0, or -EOWNERDEAD when the prior owner died holding it (robust recovery). NEVER
// returns 0 without the word actually naming this thread as the owner. `mono`: the (absolute) timeout is on
// CLOCK_MONOTONIC (FUTEX_LOCK_PI2) rather than the FUTEX_LOCK_PI default of CLOCK_REALTIME. Interruptible
// (a thread-directed signal returns -EINTR, exactly as a real LOCK_PI is interrupted -> glibc retries).
static long futex_lock_pi(struct cpu *c, int *uaddr, int trylock, const struct timespec *ts, int mono) {
    if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
    int mytid = cpu_tid(c);
    struct futex_bucket *b = fbk_of(uaddr);
    pthread_mutex_lock(&b->m);
    int parked = 0;
    long ret;
    for (;;) {
        int expect = __atomic_load_n(uaddr, __ATOMIC_SEQ_CST);
        uint32_t v = (uint32_t)expect;
        uint32_t owner = v & DD_FUTEX_TID_MASK;
        if (owner == 0) { // free (owner slot 0; FUTEX_OWNER_DIED may still be set on a robust mutex)
            int others = fbk_parked(b, futex_key(uaddr)) - (parked ? 1 : 0); // waiters left behind
            int nv = (int)((uint32_t)mytid | (others > 0 ? DD_FUTEX_WAITERS : 0));
            // Acquire atomically vs a racing userspace fast-path locker (cmpxchg 0->tid): if the word moved
            // underfoot, retry from the re-read instead of clobbering the new owner (double-ownership bug).
            if (!__atomic_compare_exchange_n(uaddr, &expect, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) continue;
            ret = (v & DD_FUTEX_OWNER_DIED) ? -EOWNERDEAD : 0;
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
        if (!__atomic_compare_exchange_n(uaddr, &expect, (int)(v | DD_FUTEX_WAITERS), 0, __ATOMIC_SEQ_CST,
                                         __ATOMIC_SEQ_CST))
            continue;
        if (!parked) {
            atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
            fbk_park(b, futex_key(uaddr), ~0u); // PI-mutex waiter: matches any wake bitset
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
        fbk_unpark(b, futex_key(uaddr));
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
    }
    pthread_mutex_unlock(&b->m);
    return ret;
}

// Release the PI mutex at uaddr (FUTEX_UNLOCK_PI): only the owner may unlock (-EPERM otherwise). Hand off by
// clearing the owner TID; if waiters remain, keep FUTEX_WAITERS set (word = FUTEX_WAITERS, owner 0) so a
// userspace fast-path locker can't steal in ahead of a parked waiter, and broadcast -- the woken waiters
// re-contend for ownership under the bucket mutex in futex_lock_pi, so exactly one acquires. Returns 0.
static long futex_unlock_pi(struct cpu *c, int *uaddr) {
    if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
    int mytid = cpu_tid(c);
    struct futex_bucket *b = fbk_of(uaddr);
    pthread_mutex_lock(&b->m);
    uint32_t v = (uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST);
    if ((v & DD_FUTEX_TID_MASK) != (uint32_t)mytid) {
        pthread_mutex_unlock(&b->m);
        return -EPERM; // not the owner -- Linux rejects an UNLOCK_PI from a non-owner
    }
    int waiters = fbk_parked(b, futex_key(uaddr));
    __atomic_store_n(uaddr, (int)(waiters > 0 ? DD_FUTEX_WAITERS : 0), __ATOMIC_SEQ_CST);
    if (waiters > 0) pthread_cond_broadcast(&b->c);
    pthread_mutex_unlock(&b->m);
    return 0;
}

// nr_wake2 is the raw 4th syscall arg (a3) reinterpreted as a count for FUTEX_WAKE_OP (WAIT ops use a3 as a
// timespec instead -- the two never overlap because op selects one interpretation); uaddr2 (a4) + val3 (a5)
// carry the WAKE_OP / REQUEUE second-address operands, and are ignored by the WAIT/plain-WAKE branches.
static long futex_op(struct cpu *c, int *uaddr, int op, int val, const struct timespec *ts, int nr_wake2, int *uaddr2,
                     uint32_t val3) {
    // FUTEX_WAIT_BITSET(9)/WAKE_BITSET(10) require a non-empty bitset: Linux rejects val3==0 with EINVAL
    // (a zero mask can match no waiter). The old shared WAIT/WAKE path ignored val3 entirely and accepted it.
    if ((op == 9 || op == 10) && val3 == 0) return -EINVAL;
    // PI-mutex ops use per-address ownership tracked in the (always-present) buckets, independent of the
    // legacy single-queue mode below, so dispatch them first. FUTEX_LOCK_PI=6, UNLOCK_PI=7, TRYLOCK_PI=8,
    // WAIT_REQUEUE_PI=11, CMP_REQUEUE_PI=12, LOCK_PI2=13.
    if (op == 6) return futex_lock_pi(c, uaddr, 0, ts, 0);
    if (op == 13) return futex_lock_pi(c, uaddr, 0, ts, 1);
    if (op == 8) return futex_lock_pi(c, uaddr, 1, NULL, 0);
    if (op == 7) return futex_unlock_pi(c, uaddr);
    if (op == 11) { // FUTEX_WAIT_REQUEUE_PI: wait on uaddr (while *uaddr==val), then acquire the PI mutex uaddr2.
        // Modern glibc (>=2.25) condvars no longer use requeue_pi, so this path is cold; implement it as a
        // plain WAIT followed by a LOCK_PI on uaddr2 -- semantically what pthread_cond_wait on a PI mutex
        // needs, and always CORRECT (a woken waiter re-acquires uaddr2 itself; see CMP_REQUEUE_PI below).
        if (!uaddr || !host_addr_mapped((uintptr_t)uaddr)) return -EFAULT;
        struct futex_bucket *b = fbk_of(uaddr);
        pthread_mutex_lock(&b->m);
        if ((uint32_t)__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != (uint32_t)val) {
            pthread_mutex_unlock(&b->m);
            return -EAGAIN;
        }
        atomic_fetch_add_explicit(&b->waiters, 1, memory_order_relaxed);
        fbk_park(b, futex_key(uaddr), ~0u); // PI-mutex waiter: matches any wake bitset
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
        fbk_unpark(b, futex_key(uaddr));
        if (atomic_fetch_sub_explicit(&b->waiters, 1, memory_order_relaxed) == 1) b->imprecise = 0;
        pthread_mutex_unlock(&b->m);
        if (ret < 0) return ret;                   // on error the caller does NOT own uaddr2 (kernel-exact)
        return futex_lock_pi(c, uaddr2, 0, ts, 0); // woken -> acquire the target PI mutex before returning
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
        return futex_wake_bucket(uaddr, budget > 0x7fffffff ? 0x7fffffff : (int)budget, ~0u);
    }
    if (!g_futexq) {
        // ---- legacy single global queue ----
        if (op == 0 || op == 9) {
            pthread_mutex_lock(&g_futex_m);
            if (__atomic_load_n(uaddr, __ATOMIC_SEQ_CST) != val) {
                pthread_mutex_unlock(&g_futex_m);
                return -EAGAIN;
            }
            // Publish, then re-check tpending (the StoreLoad handshake with thread_target_signal), so a
            // thread-directed signal that arrives right before we sleep interrupts the wait, not deadlocks.
            thread_wait_publish(&g_futex_m, &g_futex_c);
            if (cpu_wait_interrupted(c)) {
                thread_wait_clear();
                pthread_mutex_unlock(&g_futex_m);
                return -EINTR;
            }
            g_futex_parked++;
            ts_wait_enter(); // 'S' while parked in FUTEX_WAIT (legacy single-queue mode)
            int rc = 0;
            if (ts) {
                struct timespec abs, rel;
                // op 9 (FUTEX_WAIT_BITSET): ts is an absolute deadline; op 0: it is relative.
                if (op == 9) futex_rel_from_abs(&rel, ts);
                abs_from_rel(&abs, op == 9 ? &rel : ts);
                rc = pthread_cond_timedwait(&g_futex_c, &g_futex_m, &abs);
            } else
                pthread_cond_wait(&g_futex_c, &g_futex_m);
            ts_wait_leave();
            thread_wait_clear();
            g_futex_parked--;
            int intr = cpu_wait_interrupted(c);
            pthread_mutex_unlock(&g_futex_m);
            if (intr) return -EINTR; // woken by a cross-thread signal -> guest retries; dispatcher delivers it
            return rc == ETIMEDOUT ? -ETIMEDOUT : 0;
        }
        if (op == 1 || op == 10 || op == 3 || op == 4) { // WAKE / WAKE_BITSET / REQUEUE / CMP_REQUEUE
            pthread_mutex_lock(&g_futex_m);
            int woke = g_futex_parked < val ? g_futex_parked : val; // report woken count, not the request
            pthread_cond_broadcast(&g_futex_c);
            pthread_mutex_unlock(&g_futex_m);
            return woke;
        }
        if (op == 5) { // FUTEX_WAKE_OP on the legacy single global queue
            int do_wake2 = 0;
            int rc = futex_wake_op_apply(uaddr2, val3, &do_wake2);
            if (rc < 0) return rc;
            pthread_mutex_lock(&g_futex_m);
            // One global queue can't split by address: broadcast wakes waiters on both uaddr and uaddr2, so
            // report min(parked, val) as the uaddr count (+ the requested nr_wake2 when the cmp fires).
            int woke = g_futex_parked < val ? g_futex_parked : val;
            if (do_wake2) woke += nr_wake2 < 0 ? 0 : nr_wake2;
            pthread_cond_broadcast(&g_futex_c);
            pthread_mutex_unlock(&g_futex_m);
            return woke;
        }
        return 0;
    }
    // ---- W5C per-address buckets ----
    struct futex_bucket *b = fbk_of(uaddr);
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
        fbk_park(b, futex_key(uaddr), op == 9 ? val3 : ~0u);
        ts_wait_enter(); // 'S' (sleeping) while parked in FUTEX_WAIT; peer /proc/<pid>/stat|status must not show 'R'
        int rc = 0;
        if (ts) {
            struct timespec abs, rel;
            // op 9 (FUTEX_WAIT_BITSET): ts is an absolute deadline; op 0: it is relative.
            if (op == 9) futex_rel_from_abs(&rel, ts);
            abs_from_rel(&abs, op == 9 ? &rel : ts);
            rc = pthread_cond_timedwait(&b->c, &b->m, &abs);
        } else
            pthread_cond_wait(&b->c, &b->m);
        ts_wait_leave();
        thread_wait_clear();
        fbk_unpark(b, futex_key(uaddr));
        int intr = cpu_wait_interrupted(c);
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
        int woke = futex_wake_bucket(uaddr, val, op == 10 ? val3 : ~0u);
        return woke;
    }
    if (op == 5) { // FUTEX_WAKE_OP: atomically mutate *uaddr2, wake uaddr waiters, conditionally uaddr2's.
        // glibc's pthread_cond_signal/broadcast issue this (bump the internal seq/counter at uaddr2 and wake
        // the condvar's futex at uaddr) -- the old "other ops -> return 0" reported success WITHOUT waking,
        // so every glibc condvar signal was silently dropped (Chromium's main thread waited forever on the
        // in-process Viz/GPU thread's condvar -> the live-window stall).
        int do_wake2 = 0;
        int rc = futex_wake_op_apply(uaddr2, val3, &do_wake2);
        if (rc < 0) return rc; // -EFAULT (bad uaddr2) / -ENOSYS (unknown op|cmp): report to the guest as-is
        int woke = futex_wake_bucket(uaddr, val, ~0u);
        if (do_wake2) woke += futex_wake_bucket(uaddr2, nr_wake2, ~0u);
        return woke;
    }
    // A genuinely undefined command (the removed FUTEX_FD=2, or any value >= 14 that names no futex op) is
    // -ENOSYS on Linux, not a silent success -- the old fall-through masked capability probes. The PI ops
    // (6-8,11-13) that dd does not model are left as best-effort success above to avoid breaking a PI-mutex
    // fast-path fallback; only the truly-undefined range is rejected here.
    if (op == 2 || op >= 14) return -ENOSYS;
    // Any remaining op is unmodelled -- pretend success (baseline behavior).
    return 0;
}

static void futex_wake_addr(uint64_t uaddr) {
    if (!uaddr) return;
    // CLONE_CHILD_CLEARTID: zero the word then wake joiners (pthread_join FUTEX_WAITs on this word). A
    // DETACHED guest thread (e.g. musl's __unmapself) munmaps its OWN stack -- which also holds the thread
    // descriptor with this CLEARTID word -- and only THEN issues the exit syscall, so by the time we run
    // here the word can already be unmapped. Linux's clear_child_tid uses put_user() and silently swallows
    // that fault; a raw store here would instead SIGSEGV/SIGBUS the whole process (the flaky rustc-at-exit
    // teardown crash). Skip the store+wake when the address is gone -- a detached thread has no joiner to
    // wake, and a joinable thread never unmaps its own stack so its ctid is always still live.
    if (!host_addr_mapped((uintptr_t)uaddr)) return;
    *(int *)uaddr = 0;
    if (!g_futexq) {
        pthread_mutex_lock(&g_futex_m);
        pthread_cond_broadcast(&g_futex_c);
        pthread_mutex_unlock(&g_futex_m);
        return;
    }
    // Always lock+broadcast (same reasoning as futex_op's WAKE): the joiner's FUTEX_WAIT re-checks
    // *ctid under this bucket's mutex, so the zero store above is ordered ahead of its check.
    struct futex_bucket *b = fbk_of((const void *)(uintptr_t)uaddr);
    pthread_mutex_lock(&b->m);
    pthread_cond_broadcast(&b->c);
    pthread_mutex_unlock(&b->m);
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

static pthread_mutex_t g_threg_m = PTHREAD_MUTEX_INITIALIZER;

// fork() only clones the calling thread. Any process-PRIVATE engine mutex a dead peer held at the instant
// the guest forked is inherited LOCKED with no owner to release it, so the single-threaded child deadlocks
// the first time it takes that lock (the go/npm/cargo build hang). Reinitialise this module's
// private locks to a clean unlocked state in the child (the calling thread never holds one across a guest
// syscall, and no peer survives, so this is always safe). The g_fbk futex buckets are deliberately NOT reset
// here: they live in a PROCESS_SHARED MAP_SHARED page so a cross-fork FUTEX_WAKE/WAIT still matches (glibc
// process-shared semaphores), which is the opposite requirement. Called from the fork child path in proc.c.
static void thread_after_fork(void) {
    pthread_mutex_init(&g_threg_m, NULL); // thread registry (tkill/tgkill lookup, thread_register)
    pthread_mutex_init(&g_futex_m, NULL); // legacy global futex lock (NOFUTEXQ path)
    pthread_cond_init(&g_futex_c, NULL);
    // Shared-futex-key registry lock: a private (process-shared? no -- plain) mutex that a dead peer could
    // have held across the guest fork; the child inherits the VA->object-identity entries (its mappings are
    // the parent's, at the same VAs) but must reset the lock, exactly as the futex/threg locks above.
    pthread_mutex_init(&g_shkey_m, NULL);
    // fork() clones ONLY the calling thread, but the tid->thread registry still lists every PARENT thread.
    // Those phantom entries never unregister (no thread backs them in the child), so they (1) poison
    // tgkill/tkill routing -- thread_target_signal matches a phantom by tid (or pid 1 for the main thread)
    // and "delivers" the signal onto a dead cpu, dropping a real thread's SIGURG/preempt (Go's async
    // preemption then spins one goroutine at 100% while its peers park -- the livelock) -- and (2) make
    // the child's next execve teardown (thread_exit_others) busy-wait its full ~10s ceiling for phantom peers
    // that can never leave (the go build fork+exec stall: ~14s PER compile child, measured). Rebuild the
    // registry to hold ONLY the surviving (calling) thread, exactly as stw_after_fork() does for the STW
    // registry -- reinitialised this module's LOCKS but left the registry CONTENTS inherited.
    struct cpu *self = (g_my_threg >= 0) ? g_threg[g_my_threg].c : NULL;
    memset(g_threg, 0, sizeof g_threg);
    if (self) {
        g_threg[0].c = self;
        g_threg[0].th = pthread_self();
        g_my_threg = 0;
    } else {
        g_my_threg = -1;
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
    // (g_ckpt_armed == 0), so the gate is unchanged; SIGINFO is guest-clobber-proof (sig_l2m omits 29).
    if (g_ckpt_armed) {
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
    sa.sa_flags = 0; // NO SA_RESTART: the interrupted syscall must return EINTR so its retry loop can bail on exited
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
                } else if (!pthread_equal(g_threg[i].th, pthread_self())) {
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
// /proc/<self>/task st_nlink synth reports 2 + this (Linux: `.`, `..`, one subdir per thread) so chromium's
// sandbox `IsSingleThreaded` (fstatat st_nlink == 3) and per-tid `IsThreadPresentInProcFS` (fstatat ENOENT
// on a joined/exited thread) both track the real thread set -- otherwise the GPU process's thread_helpers
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
            if (out[j] == tid) { dup = 1; break; }
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
// hanging. dd did neither (set_robust_list was a no-op), so a crash/exit under a PTHREAD_MUTEX_ROBUST lock
// deadlocked every waiter. Layout (LP64 kernel/glibc ABI, 24-byte head):
//   struct robust_list_head { void *list; long futex_offset; void *list_op_pending; };
// `list` chains each mutex's embedded robust_list node (first word = next; LSB is a PI flag we mask off) and
// terminates by pointing back at &head->list (== head, list is at offset 0). The futex word for a node is at
// node + futex_offset. list_op_pending covers a mutex mid-(un)lock and is handled once, at the end.
#define DD_ROBUST_LIST_LIMIT 2048

// If the dying thread still owns *futex_addr, set FUTEX_OWNER_DIED (preserving FUTEX_WAITERS) and wake one
// waiter. cmpxchg-loops so a concurrent lock/unlock on the same word can't clobber the OWNER_DIED marking.
static void robust_handle_death(uint64_t futex_addr, int mytid) {
    if (!futex_addr || !host_range_mapped((uintptr_t)futex_addr, 4)) return;
    int *w = (int *)(uintptr_t)futex_addr;
    int v = __atomic_load_n(w, __ATOMIC_SEQ_CST);
    for (;;) {
        if (((uint32_t)v & DD_FUTEX_TID_MASK) != (uint32_t)mytid) return; // not (or no longer) ours
        int nv = (int)(((uint32_t)v & DD_FUTEX_WAITERS) | DD_FUTEX_OWNER_DIED);
        if (__atomic_compare_exchange_n(w, &v, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
            if ((uint32_t)v & DD_FUTEX_WAITERS) futex_wake_bucket(w, 1, ~0u); // one waiter -> EOWNERDEAD
            return;
        }
        // v was reloaded with the current word by the failed cmpxchg -> re-check ownership and retry
    }
}

// Walk this thread's robust list (if any) and mark+wake each still-owned mutex. Clears c->robust_list so a
// second call (thread exit then process exit) is a no-op. Every guest pointer is bounds-checked before deref.
static void futex_robust_exit(struct cpu *c) {
    uint64_t head = c->robust_list;
    c->robust_list = 0;
    if (!head || !host_range_mapped((uintptr_t)head, 24)) return;
    uint64_t raw_first = *(uint64_t *)(uintptr_t)head;                // head->list.next (LSB = PI flag)
    long futex_offset = *(long *)(uintptr_t)(head + 8);               // head->futex_offset
    uint64_t pending = (*(uint64_t *)(uintptr_t)(head + 16)) & ~1ULL; // head->list_op_pending
    int mytid = cpu_tid(c);
    uint64_t entry = raw_first & ~1ULL;
    for (int limit = 0; limit < DD_ROBUST_LIST_LIMIT; limit++) {
        if (entry == head) break; // wrapped back to &head->list -> done
        if (!host_range_mapped((uintptr_t)entry, 8)) break;
        uint64_t next = (*(uint64_t *)(uintptr_t)entry) & ~1ULL; // entry->next
        if (entry != pending) robust_handle_death(entry + (uint64_t)futex_offset, mytid);
        entry = next;
    }
    if (pending && pending != head) robust_handle_death(pending + (uint64_t)futex_offset, mytid);
}

static void *thread_trampoline(void *p) {
    struct cpu *child = (struct cpu *)p;
    // sets its own TSD, runs to thread exit
    run_guest(child);
    // cgroup pids: task ended
    atomic_fetch_sub(&g_pids_cur, 1);
    acct_publish_tasks(); // update this process's container-wide task-count contribution
    // robust mutexes this thread still holds -> mark OWNER_DIED + wake a waiter (before the join wakeup)
    futex_robust_exit(child);
    // pthread_join waits on this
    futex_wake_addr(child->ctid);
    free(child);
    return NULL;
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
    child->exited = 0;
    child->redirect = 0;
    // CLONE_SETTLS
    if (flags & 0x00080000) G_TLS(child) = tls;
    int tid = __sync_add_and_fetch(&g_next_tid, 1);
    // This thread's gettid() identity (see proc.c case 178): a unique id, distinct from the init's pid 1.
    child->tid = tid;
    // CLONE_PARENT_SETTID
    if ((flags & 0x00100000) && ptid) *(int *)ptid = tid;
    // CLONE_CHILD_SETTID
    if ((flags & 0x01000000) && ctid) *(int *)ctid = tid;
    // CLONE_CHILD_CLEARTID
    child->ctid = (flags & 0x00200000) ? ctid : 0;
    // robust list is per-thread and NOT inherited: a new thread starts empty and re-registers via
    // set_robust_list itself (otherwise the copied parent head would be walked twice on exit).
    child->robust_list = 0;
    g_threaded = 1;
    pthread_t th;
    if (pthread_create(&th, NULL, thread_trampoline, child) != 0) {
        free(child);
        return -EAGAIN;
    }
    // cgroup pids: task created
    atomic_fetch_add(&g_pids_cur, 1);
    acct_publish_tasks(); // update this process's container-wide task-count contribution
    pthread_detach(th);
    return tid;
}
