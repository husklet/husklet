// Extracted from service(): SysV IPC syscalls (shm/sem/msg). Returns 1 if nr was handled (G_RET set), 0 otherwise.
// Included by service.c after service/helpers.c, before service(); sees the same TU scope (globals + helpers).
//
// ============================================================================================
// -- HL-internal SysV IPC emulation (formerly host-macOS shmget/semget/msgget passthrough).
// --------------------------------------------------------------------------------------------
// The macOS host SysV table is tiny (kern.sysv.shmmni=32) and GLOBAL: it is not per-container, so real
// software (postgres allocates many segments) hit ENOSPC where Linux succeeds, every container + the whole
// test matrix shared one 32-slot table, and a killed run leaked mode-0 segments that filled it. We no
// longer touch the host SysV table at all. Instead each container gets a private in-shared-memory registry
// with Linux-like limits:
//
//   * A per-container CONTROL BLOCK, a named POSIX shared-memory object (shm_open) keyed by the container
//     identity (HL_NETNS, else the container init / engine-root pid) so two containers never collide and a
//     leak in one namespace can never break another. It holds a robust cross-process spinlock plus the
//     descriptor tables for shm segments, semaphore sets (values inline) and message queues. Every process
//     in the container mmap()s the SAME object MAP_SHARED, so they all see one coherent id<->key table --
//     the property SysV requires (an UNRELATED process may shmget(key) and get the same id).
//   * Each shm SEGMENT is its own named POSIX object; shmat() maps it MAP_SHARED, so two guest processes
//     that attach the same id map the SAME physical pages (genuine cross-process shared memory). Verified:
//     macOS POSIX shm objects share across fork()ed and unrelated processes and may be sized (ONCE, at
//     creation -- macOS forbids a second ftruncate) up to hundreds of MB.
//   * Each message QUEUE is its own named object holding a slot ring; msgsnd/msgrcv move bytes through it.
//   * Semaphore values / message data live in shared memory and blocking semop/msgsnd/msgrcv poll the
//     shared state under the spinlock (never held across a wait), giving Linux blocking semantics without
//     the host's 32-object ceiling and without a process-shared pthread mutex (macOS has no robust mutex,
//     so a killed holder would deadlock the container -- the spinlock instead steals a dead owner's lock).
//
// GC: shm segments / queues are unlinked on IPC_RMID (deferred for shm until nattch hits 0, per Linux), and
// the namespace CREATOR unlinks every live object + the control block on normal exit (atexit). A SIGKILL
// still leaks -- exactly as real Linux SysV persists until IPC_RMID or reboot -- but leaks are per-run
// (the id namespace hashes in the root pid), so they never break another run the way the host-32 table did.
//
// COMPLETENESS: every control-command behavior is preserved -- all shm/sem/msg ctl ops, IPC_STAT
// marshaling into the arch-specific *id64_ds, uid/gid virtualization (we now store the container identity
// natively so no host<->guest mapping is needed), ipcperms EACCES + owner EPERM, EFAULT, key semantics,
// *_INFO/*_STAT -- all byte-exact vs the oracle. errno values below are the *macOS* <errno.h> constants;
// svc_done() does the macOS->Linux boundary translation at the tail (e.g. ENOMSG 91->42, EIDRM 90->43,
// EAGAIN 35->11), same as every other svc_<family>().

// shm sizes round to the host granule; reported limits are in guest pages.
#include "../page.h"
#include "../../host/range.h"

// ---- Linux control-command numbers ---------------------------------------------------------------
#ifndef IPC_INFO
#define IPC_INFO 3
#endif
#define L_IPC_RMID 0
#define L_IPC_SET 1
#define L_IPC_STAT 2
#define L_IPC_INFO 3
#define L_SHM_LOCK 11
#define L_SHM_UNLOCK 12
#define L_SHM_STAT 13
#define L_SHM_INFO 14
#define L_SHM_STAT_ANY 15
#define L_MSG_STAT 11
#define L_MSG_INFO 12
#define L_MSG_STAT_ANY 13
#define L_SEM_STAT 18
#define L_SEM_INFO 19
#define L_SEM_STAT_ANY 20
// Flag bits as the guest passes them (Linux asm-generic ABI). These happen to coincide with the macOS SDK
// values, but we spell them out so the emulation never depends on a host header.
#define L_IPC_CREAT 01000
#define L_IPC_EXCL 02000
#define L_IPC_NOWAIT 04000
#define L_SEM_UNDO 0x1000
#define L_GETNCNT 14
#define L_GETPID 11
#define L_GETVAL 12
#define L_GETALL 13
#define L_GETZCNT 15
#define L_SETVAL 16
#define L_SETALL 17
#define L_SHM_RDONLY 010000
#define L_SHM_RND 020000
#define L_MSG_NOERROR 010000
#define L_MSG_EXCEPT 020000
#define L_IPC_PRIVATE 0

// The guest's `struct ipc64_perm` (aarch64 asm-generic, 48 bytes) -- the leading member of every *id64_ds.
struct ipc64_perm_guest {
    int32_t key;
    uint32_t uid, gid, cuid, cgid;
    uint32_t mode;
    uint16_t seq, pad2;
    uint64_t unused1, unused2;
};

// struct shmid64_ds (aarch64 asm-generic, 112 bytes).
struct shmid64_ds_guest {
    struct ipc64_perm_guest shm_perm;
    uint64_t shm_segsz;
    int64_t shm_atime, shm_dtime, shm_ctime;
    int32_t shm_cpid, shm_lpid;
    uint64_t shm_nattch, unused4, unused5;
};

// struct semid64_ds -- the ONE SysV struct whose 64-bit layout is arch-specific (shmid64_ds/msqid64_ds are
// identical across x86-64 and aarch64). x86-64's `struct semid64_ds` carries a reserved slot after each
// time field (otime_high/ctime_high, an old x86 quirk), pushing sem_nsems to offset 80 in a 104-byte
// struct; the aarch64 asm-generic form has neither, with sem_nsems at 64 in an 88-byte struct. Verified by
// raw-syscall probe on both arches. CANON_X86ONLY is defined only in the x86_64 engine.
#ifdef CANON_X86ONLY
struct semid64_ds_guest {
    struct ipc64_perm_guest sem_perm;     // 0   (48)
    int64_t sem_otime, sem_otime_high;    // 48, 56
    int64_t sem_ctime, sem_ctime_high;    // 64, 72
    uint64_t sem_nsems, unused3, unused4; // 80, 88, 96 -> 104
};
#else
struct semid64_ds_guest {
    struct ipc64_perm_guest sem_perm;     // 0   (48)
    int64_t sem_otime, sem_ctime;         // 48, 56
    uint64_t sem_nsems, unused3, unused4; // 64, 72, 80 -> 88
};
#endif
// struct msqid64_ds (aarch64 asm-generic, 64-bit form, 120 bytes).
struct msqid64_ds_guest {
    struct ipc64_perm_guest msg_perm;
    int64_t msg_stime, msg_rtime, msg_ctime;
    uint64_t msg_cbytes, msg_qnum, msg_qbytes;
    int32_t msg_lspid, msg_lrpid;
    uint64_t unused4, unused5;
};

// Linux IPC_INFO/*_INFO limit + resource structs (as the guest expects them, 64-bit ABI).
struct shminfo_guest {
    uint64_t shmmax, shmmin, shmmni, shmseg, shmall, unused[4];
};

struct shm_info_guest {
    int32_t used_ids;
    uint64_t shm_tot, shm_rss, shm_swp, swap_attempts, swap_successes;
};

struct seminfo_guest {
    int32_t semmap, semmni, semmns, semmnu, semmsl, semopm, semume, semusz, semvmx, semaem;
};

struct msginfo_guest {
    int32_t msgpool, msgmap, msgmax, msgmnb, msgmni, msgssz, msgtql;
    uint16_t msgseg;
};

// The guest's `struct sembuf` (Linux, 6 bytes) -- what semop() receives.
struct sembuf_guest {
    uint16_t sem_num;
    int16_t sem_op;
    int16_t sem_flg;
};

// ============================================================================================
// HL-internal shared registry
// ============================================================================================
// Guest-visible limits (also mirrored in /proc/sys/kernel/*).  These must be
// the capacities actually enforced below: applications legitimately size and
// exhaust IPC resources from these values, and an optimistic synthetic value
// makes semget/msgget fail before the advertised limit.
#define HL_IPC_SHMMAX 0xffffffffffffffffULL
#define HL_IPC_SHMMNI_ADV 4096
#define HL_IPC_SEMMNI_ADV 512
#define HL_IPC_SEMMSL_ADV 256
#define HL_IPC_SEMMNS_ADV (512 * 256)
#define HL_IPC_SEMOPM_ADV 500
#define HL_IPC_SEMVMX 32767
#define HL_IPC_MSGMAX 8192
#define HL_IPC_MSGMNB 16384
#define HL_IPC_MSGMNI_ADV 512
// Table capacities we allocate and enforce. They agree exactly with the
// discovery plane above and the generated procfs values.
#define HL_IPC_SHMMNI 4096                     // shm segment descriptors (metadata only; data in a per-segment object)
#define HL_IPC_SEMMNI 512                      // semaphore SETS
#define HL_IPC_SEMMSL 256                      // semaphores per set (inline values)
#define HL_IPC_MSGMNI 512                      // message queues (metadata; data in a per-queue object)
#define HL_MSG_SLOTS 512                       // messages a single queue can hold
#define HL_MSG_MAX_SIZE 8192                   // == MSGMAX: largest single message body
#define HL_IPC_CTRL_MAGIC UINT32_C(0x43494c48) // "HLIC" (LE)
#define HL_MSG_MAGIC UINT32_C(0x514d4c48)      // "HLMQ" (LE)

// A cross-process robust spinlock: 0 == free, else == holder host pid. A holder that dies with the lock
// held is detected (kill(pid,0)==ESRCH) and its lock stolen -- macOS has no PTHREAD_MUTEX_ROBUST, and the
// critical sections are short (table edits + a couple of shm_open/ftruncate; never a blocking wait).
struct hl_ipc_lock {
    atomic_uint owner;
};

// The container-visible permission block (we store the GUEST identity natively -- no host<->guest map).
struct hl_ipc_perm {
    int32_t key;
    uint32_t uid, gid, cuid, cgid, mode, seq;
};

struct hl_shm_entry {
    uint32_t inuse, removed;
    struct hl_ipc_perm perm;
    uint64_t segsz; // caller-requested size (reported by IPC_STAT, Linux-faithful)
    int32_t cpid, lpid;
    int64_t atime, dtime, ctime;
    uint32_t nattch; // authoritative attach count across all processes
};

struct hl_sem_entry {
    uint32_t inuse;
    struct hl_ipc_perm perm;
    uint32_t nsems;
    int64_t otime, ctime;
    uint16_t val[HL_IPC_SEMMSL];
    int32_t pid[HL_IPC_SEMMSL];  // last process to op each sem (GETPID)
    int32_t ncnt[HL_IPC_SEMMSL]; // processes waiting for the sem to rise (GETNCNT)
    int32_t zcnt[HL_IPC_SEMMSL]; // processes waiting for the sem to reach 0 (GETZCNT)
};

struct hl_msg_queue {
    uint32_t inuse, removed;
    struct hl_ipc_perm perm;
    int64_t stime, rtime, ctime;
    int32_t lspid, lrpid;
    uint64_t qnum, cbytes, qbytes;
};

struct hl_ipc_ctrl {
    atomic_uint magic;
    struct hl_ipc_lock lock;
    struct hl_shm_entry shm[HL_IPC_SHMMNI];
    struct hl_sem_entry sem[HL_IPC_SEMMNI];
    struct hl_msg_queue msg[HL_IPC_MSGMNI];
};

// The width of `mtype`, the leading member of the guest's struct msgbuf, and therefore the offset at which
// every msgsnd/msgrcv payload begins. It is the GUEST's `long` -- 8 bytes on every guest this engine
// targets -- and NOT the host's, which is why it is spelled out rather than taken from the host `long`: on an
// LLP64 host `long` is 4 bytes, so that spelling read the payload from offset 4 of an 8-byte header and
// wrote it back to the same wrong place. Send and receive were consistently wrong, so a round trip still
// returned the caller's own bytes and the ordinary msgsnd/msgrcv case looked correct; what it actually
// moved was four bytes of the type word plus the first half of the body.
#define HL_IPC_MSG_TYPE_SIZE ((size_t)8)

// A message queue's backing object: a slot ring + free list. head/tail are the FIFO order; msgrcv may
// unlink any matching slot from the middle.
struct hl_ipc_msg_slot {
    int64_t mtype;
    uint32_t size;
    int32_t next;
    uint8_t data[HL_MSG_MAX_SIZE];
};

struct hl_ipc_msg_store {
    atomic_uint magic;
    int32_t head, tail, freehead;
    struct hl_ipc_msg_slot slots[HL_MSG_SLOTS];
};

// ---- in-process (COW-inherited across fork) state ------------------------------------------------
static struct hl_ipc_ctrl *g_ctrl; // this process's mapping of the control block
static uint32_t g_ns_hash;         // namespace id (0 == not yet computed)
static int g_ipc_creator;          // did THIS process create the control block?
static int g_ipc_atexit_armed;
static int g_ipc_ctor_pid;                                        // engine-root pid (constructor; COW-inherited)
static pthread_mutex_t g_ipc_local_m = PTHREAD_MUTEX_INITIALIZER; // guards the in-process caches below

#define HL_SHMAT_MAX 256

static struct {
    int used;
    void *addr;
    uint32_t idx;
    size_t len;
} g_shmat[HL_SHMAT_MAX];

#define HL_MSGCACHE_MAX 256

static struct {
    int used;
    uint32_t idx;
    uint32_t seq;
    struct hl_ipc_msg_store *p;
} g_msgcache[HL_MSGCACHE_MAX];

#define HL_UNDO_MAX 256

static struct {
    int used;
    uint32_t idx; // sem set slot
    uint32_t seq; // set's seq (guard against slot reuse)
    uint16_t semnum;
    int adj; // accumulated undo adjustment (subtract on process exit)
} g_undo[HL_UNDO_MAX];

static void ipc_init(void) {
    g_ipc_ctor_pid = (int)getpid();
}

#ifndef HL_EMBEDDED_BUILD
__attribute__((constructor)) static void ipc_ctor(void) {
    ipc_init();
}
#endif

static int64_t hl_ipc_now(void) {
    return (int64_t)time(NULL);
}

// Round a shm segment size up to a whole HOST mapping unit; both call sites (ftruncate, mmap) round
// identically, so they need only agree. hl_host_page_size() validates power-of-two and reports 0 on failure
// -- sysconf signals failure with -1, not 0, so a `pg == 0` guard on bare sysconf let SIZE_MAX through and
// the mask degenerated to `& 1`. Fall back to the guest page; 16 KB would quadruple a 4 KB-host segment.
static size_t hl_ipc_pground(size_t n) {
    size_t pg = hl_host_page_size();
    if (pg == 0) pg = HL_LINUX_GUEST_PAGE_SIZE;
    return (n + pg - 1) & ~(pg - 1);
}

// ---- namespace + object names --------------------------------------------------------------------
// Key the namespace by HL_NETNS (per-IPC-namespace isolation; host networking leaves it unset and shared). When
// unset we fall back to the container init pid (daemon path) or the engine-root pid (single-binary/test
// path, captured by the constructor and COW-inherited by every child) -- unique per run, shared by the
// whole process tree, so a leak is per-run and cross-run runs never collide.
static uint32_t ipc_ns(void) {
    if (g_ns_hash) return g_ns_hash;
    char buf[80];
    const char *ns = hl_option_get("HL_NETNS");
    if (ns && ns[0])
        snprintf(buf, sizeof buf, "n:%s", ns);
    else
        snprintf(buf, sizeof buf, "p:%d", g_init_hostpid ? g_init_hostpid : g_ipc_ctor_pid);
    uint32_t h = 2166136261u;
    for (const char *p = buf; *p; p++) {
        h ^= (uint8_t)*p;
        h *= 16777619u;
    }
    if (h == 0) h = 1;
    g_ns_hash = h;
    return h;
}

static void hl_ipc_control_name(char *out, size_t n) {
    snprintf(out, n, "/hl%08xC", ipc_ns());
}

static void hl_ipc_shm_name(char *out, size_t n, uint32_t idx) {
    snprintf(out, n, "/hl%08xs%x", ipc_ns(), idx);
}

static void hl_ipc_message_name(char *out, size_t n, uint32_t idx) {
    snprintf(out, n, "/hl%08xm%x", ipc_ns(), idx);
}

// ---- robust spinlock -----------------------------------------------------------------------------
static void hl_ipc_lock(struct hl_ipc_lock *L) {
    uint32_t me = (uint32_t)getpid();
    for (long spin = 0;; spin++) {
        uint32_t exp = 0;
        if (atomic_compare_exchange_weak(&L->owner, &exp, me)) return;
        if (exp != 0 && exp != me && kill((pid_t)exp, 0) < 0 && errno == ESRCH) {
            if (atomic_compare_exchange_strong(&L->owner, &exp, me)) return; // steal a dead owner's lock
        }
        if (spin < 200) {
            continue; // spin
        }
        struct timespec ts = {0, 50000}; // 50us
        nanosleep(&ts, NULL);
        if (spin > 400000) { // ~20s: last-resort steal so a wedged holder can't deadlock the container
            atomic_store(&L->owner, me);
            return;
        }
    }
}

static void hl_ipc_unlock(struct hl_ipc_lock *L) {
    atomic_store(&L->owner, 0);
}

// ---- control block attach ------------------------------------------------------------------------
static void sysv_on_exit(void);

static struct hl_ipc_ctrl *hl_ipc_ctrl(void) {
    if (g_ctrl) return g_ctrl;
    char nm[40];
    hl_ipc_control_name(nm, sizeof nm);
    int created = 0, fd = shm_open(nm, O_CREAT | O_EXCL | O_RDWR, 0600);
    if (fd >= 0) {
        created = 1;
        if (ftruncate(fd, (off_t)sizeof(struct hl_ipc_ctrl)) < 0) {
            close(fd);
            shm_unlink(nm);
            return NULL;
        }
    } else if (errno == EEXIST) {
        fd = shm_open(nm, O_RDWR, 0600);
    }
    if (fd < 0) return NULL;
    void *p = mmap(NULL, sizeof(struct hl_ipc_ctrl), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (p == MAP_FAILED) return NULL;
    struct hl_ipc_ctrl *c = (struct hl_ipc_ctrl *)p;
    if (created) {
        // A fresh POSIX shm object is zero-filled, which is our valid empty state; publish magic last.
        atomic_store(&c->magic, HL_IPC_CTRL_MAGIC);
        g_ipc_creator = 1;
    } else {
        for (int i = 0; i < 200000 && atomic_load(&c->magic) != HL_IPC_CTRL_MAGIC; i++) {
            struct timespec ts = {0, 20000};
            nanosleep(&ts, NULL);
        }
        if (atomic_load(&c->magic) != HL_IPC_CTRL_MAGIC) {
            munmap(c, sizeof *c);
            return NULL;
        }
    }
    g_ctrl = c;
    if (!g_ipc_atexit_armed) {
        g_ipc_atexit_armed = 1;
        atexit(sysv_on_exit);
    }
    return c;
}

// ---- message-queue backing object ----------------------------------------------------------------
// Cache the per-queue mapping in-process (keyed by idx+seq so a reused slot never serves a stale store).
static struct hl_ipc_msg_store *hl_ipc_msg_store(uint32_t idx, uint32_t seq, int create) {
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < HL_MSGCACHE_MAX; i++)
        if (g_msgcache[i].used && g_msgcache[i].idx == idx && g_msgcache[i].seq == seq) {
            struct hl_ipc_msg_store *r = g_msgcache[i].p;
            pthread_mutex_unlock(&g_ipc_local_m);
            return r;
        }
    pthread_mutex_unlock(&g_ipc_local_m);

    char nm[40];
    hl_ipc_message_name(nm, sizeof nm, idx);
    int fd;
    if (create) {
        shm_unlink(nm); // clear any stale object at this (ns,idx) before (re)creating
        fd = shm_open(nm, O_CREAT | O_EXCL | O_RDWR, 0600);
        if (fd < 0)
            fd = shm_open(nm, O_RDWR, 0600); // lost a create race -> open the winner's
        else if (ftruncate(fd, (off_t)sizeof(struct hl_ipc_msg_store)) < 0) {
            close(fd);
            shm_unlink(nm);
            return NULL;
        }
    } else {
        fd = shm_open(nm, O_RDWR, 0600);
    }
    if (fd < 0) return NULL;
    void *p = mmap(NULL, sizeof(struct hl_ipc_msg_store), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (p == MAP_FAILED) return NULL;
    struct hl_ipc_msg_store *s = (struct hl_ipc_msg_store *)p;
    if (create && atomic_load(&s->magic) != HL_MSG_MAGIC) {
        s->head = s->tail = -1;
        for (int i = 0; i < HL_MSG_SLOTS; i++)
            s->slots[i].next = (i + 1 < HL_MSG_SLOTS) ? i + 1 : -1;
        s->freehead = 0;
        atomic_store(&s->magic, HL_MSG_MAGIC);
    } else {
        for (int i = 0; i < 200000 && atomic_load(&s->magic) != HL_MSG_MAGIC; i++) {
            struct timespec ts = {0, 20000};
            nanosleep(&ts, NULL);
        }
        if (atomic_load(&s->magic) != HL_MSG_MAGIC) {
            munmap(s, sizeof *s);
            return NULL;
        }
    }
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < HL_MSGCACHE_MAX; i++)
        if (!g_msgcache[i].used) {
            g_msgcache[i].used = 1;
            g_msgcache[i].idx = idx;
            g_msgcache[i].seq = seq;
            g_msgcache[i].p = s;
            break;
        }
    pthread_mutex_unlock(&g_ipc_local_m);
    return s;
}

static void hl_ipc_msg_uncache(uint32_t idx) {
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < HL_MSGCACHE_MAX; i++)
        if (g_msgcache[i].used && g_msgcache[i].idx == idx) {
            munmap(g_msgcache[i].p, sizeof(struct hl_ipc_msg_store));
            g_msgcache[i].used = 0;
        }
    pthread_mutex_unlock(&g_ipc_local_m);
}

// ---- permission checks (against the stored container identity) -----------------------------------
static int hl_ipc_access(const struct hl_ipc_perm *p, int want) {
    cred_init();
    if (cred_euid() == 0) return 0;
    int eu = cred_euid(), eg = cred_egid(), granted;
    if ((uint32_t)eu == p->uid || (uint32_t)eu == p->cuid)
        granted = (p->mode >> 6) & 7;
    else if ((uint32_t)eg == p->gid || (uint32_t)eg == p->cgid)
        granted = (p->mode >> 3) & 7;
    else
        granted = p->mode & 7;
    return (granted & want) == want ? 0 : -EACCES;
}

static int hl_ipc_owner(const struct hl_ipc_perm *p) {
    cred_init();
    if (cred_euid() == 0) return 0;
    return ((uint32_t)cred_euid() == p->uid || (uint32_t)cred_euid() == p->cuid) ? 0 : -EPERM;
}

static void hl_perm_to_guest(struct ipc64_perm_guest *g, const struct hl_ipc_perm *p) {
    g->key = p->key;
    g->uid = p->uid;
    g->gid = p->gid;
    g->cuid = p->cuid;
    g->cgid = p->cgid;
    g->mode = p->mode;
    g->seq = (uint16_t)p->seq;
    g->pad2 = 0;
    g->unused1 = g->unused2 = 0;
}

static void hl_perm_init(struct hl_ipc_perm *p, int32_t key, int flag) {
    cred_init();
    p->key = key;
    p->uid = p->cuid = (uint32_t)cred_euid();
    p->gid = p->cgid = (uint32_t)cred_egid();
    p->mode = (uint32_t)(flag & 0777);
    // seq is preserved across free/realloc by the caller.
}

// ---- id build / decode ---------------------------------------------------------------------------
static uint64_t hl_ipc_id(int mni, uint32_t idx, uint32_t seq) {
    return (uint64_t)seq * (uint32_t)mni + idx;
}

// ============================================================================================
//  SHARED MEMORY
// ============================================================================================
static struct hl_shm_entry *shm_by_id(struct hl_ipc_ctrl *C, int id) {
    if (id < 0) return NULL;
    uint32_t idx = (uint32_t)id % HL_IPC_SHMMNI, seq = (uint32_t)id / HL_IPC_SHMMNI;
    struct hl_shm_entry *s = &C->shm[idx];
    if (!s->inuse || s->removed || s->perm.seq != seq) return NULL;
    return s;
}

static uint32_t shm_idx_of(struct hl_ipc_ctrl *C, const struct hl_shm_entry *s) {
    return (uint32_t)(s - C->shm);
}

static void shm_free(struct hl_ipc_ctrl *C, uint32_t idx) {
    char nm[40];
    hl_ipc_shm_name(nm, sizeof nm, idx);
    shm_unlink(nm);
    uint32_t seq = C->shm[idx].perm.seq + 1;
    memset(&C->shm[idx], 0, sizeof C->shm[idx]);
    C->shm[idx].perm.seq = seq;
}

// Marshal descriptor idx -> the guest shmid64_ds at gbuf (already access-checked). Returns 0 or -errno.
static uint64_t shm_stat_to_guest(struct hl_ipc_ctrl *C, uint32_t idx, uint64_t gbuf) {
    struct hl_shm_entry *s = &C->shm[idx];
    struct shmid64_ds_guest g;
    memset(&g, 0, sizeof g);
    hl_perm_to_guest(&g.shm_perm, &s->perm);
    g.shm_segsz = s->segsz;
    g.shm_atime = s->atime;
    g.shm_dtime = s->dtime;
    g.shm_ctime = s->ctime;
    g.shm_cpid = s->cpid;
    g.shm_lpid = s->lpid;
    g.shm_nattch = s->nattch;
    return guest_copy_to(gbuf, &g, sizeof(g)) == sizeof(g) ? 0 : (uint64_t)(-EFAULT);
}

// ============================================================================================
//  SEMAPHORES
// ============================================================================================
static struct hl_sem_entry *sem_by_id(struct hl_ipc_ctrl *C, int id) {
    if (id < 0) return NULL;
    uint32_t idx = (uint32_t)id % HL_IPC_SEMMNI, seq = (uint32_t)id / HL_IPC_SEMMNI;
    struct hl_sem_entry *s = &C->sem[idx];
    if (!s->inuse || s->perm.seq != seq) return NULL;
    return s;
}

static uint32_t sem_idx_of(struct hl_ipc_ctrl *C, const struct hl_sem_entry *s) {
    return (uint32_t)(s - C->sem);
}

static void sem_free(struct hl_ipc_ctrl *C, uint32_t idx) {
    uint32_t seq = C->sem[idx].perm.seq + 1;
    memset(&C->sem[idx], 0, sizeof C->sem[idx]);
    C->sem[idx].perm.seq = seq;
}

static uint64_t sem_stat_to_guest(struct hl_ipc_ctrl *C, uint32_t idx, uint64_t gbuf) {
    struct hl_sem_entry *s = &C->sem[idx];
    struct semid64_ds_guest g;
    memset(&g, 0, sizeof g);
    hl_perm_to_guest(&g.sem_perm, &s->perm);
    g.sem_otime = s->otime;
    g.sem_ctime = s->ctime;
    g.sem_nsems = s->nsems;
    return guest_copy_to(gbuf, &g, sizeof(g)) == sizeof(g) ? 0 : (uint64_t)(-EFAULT);
}

// Drop this process's undo record for (idx,semnum) -- SETVAL/SETALL clear the semadj (Linux semantics).
static void sem_undo_clear(uint32_t idx, uint32_t seq, int semnum /* -1 == whole set */) {
    for (int i = 0; i < HL_UNDO_MAX; i++)
        if (g_undo[i].used && g_undo[i].idx == idx && g_undo[i].seq == seq &&
            (semnum < 0 || g_undo[i].semnum == (uint16_t)semnum))
            g_undo[i].used = 0;
}

static void sem_undo_add(uint32_t idx, uint32_t seq, uint16_t semnum, int adj) {
    if (adj == 0) return;
    for (int i = 0; i < HL_UNDO_MAX; i++)
        if (g_undo[i].used && g_undo[i].idx == idx && g_undo[i].seq == seq && g_undo[i].semnum == semnum) {
            g_undo[i].adj += adj;
            return;
        }
    for (int i = 0; i < HL_UNDO_MAX; i++)
        if (!g_undo[i].used) {
            g_undo[i].used = 1;
            g_undo[i].idx = idx;
            g_undo[i].seq = seq;
            g_undo[i].semnum = semnum;
            g_undo[i].adj = adj;
            return;
        }
}

// ============================================================================================
//  MESSAGE QUEUES
// ============================================================================================
static struct hl_msg_queue *msg_by_id(struct hl_ipc_ctrl *C, int id) {
    if (id < 0) return NULL;
    uint32_t idx = (uint32_t)id % HL_IPC_MSGMNI, seq = (uint32_t)id / HL_IPC_MSGMNI;
    struct hl_msg_queue *q = &C->msg[idx];
    if (!q->inuse || q->removed || q->perm.seq != seq) return NULL;
    return q;
}

static uint32_t msg_idx_of(struct hl_ipc_ctrl *C, const struct hl_msg_queue *q) {
    return (uint32_t)(q - C->msg);
}

static void msg_free(struct hl_ipc_ctrl *C, uint32_t idx) {
    hl_ipc_msg_uncache(idx);
    char nm[40];
    hl_ipc_message_name(nm, sizeof nm, idx);
    shm_unlink(nm);
    uint32_t seq = C->msg[idx].perm.seq + 1;
    memset(&C->msg[idx], 0, sizeof C->msg[idx]);
    C->msg[idx].perm.seq = seq;
}

static uint64_t msg_stat_to_guest(struct hl_ipc_ctrl *C, uint32_t idx, uint64_t gbuf) {
    struct hl_msg_queue *q = &C->msg[idx];
    struct msqid64_ds_guest g;
    memset(&g, 0, sizeof g);
    hl_perm_to_guest(&g.msg_perm, &q->perm);
    g.msg_stime = q->stime;
    g.msg_rtime = q->rtime;
    g.msg_ctime = q->ctime;
    g.msg_cbytes = q->cbytes;
    g.msg_qnum = q->qnum;
    g.msg_qbytes = q->qbytes;
    g.msg_lspid = q->lspid;
    g.msg_lrpid = q->lrpid;
    return guest_copy_to(gbuf, &g, sizeof(g)) == sizeof(g) ? 0 : (uint64_t)(-EFAULT);
}

// ---- IPC_INFO / *_INFO fill (Linux-like limits + live counts) ------------------------------------
static int shm_count(struct hl_ipc_ctrl *C, int *maxid) {
    int n = 0, m = -1;
    for (int i = 0; i < HL_IPC_SHMMNI; i++)
        if (C->shm[i].inuse) {
            n++;
            m = i;
        }
    if (maxid) *maxid = m;
    return n;
}

static int sem_count(struct hl_ipc_ctrl *C, int *maxid) {
    int n = 0, m = -1;
    for (int i = 0; i < HL_IPC_SEMMNI; i++)
        if (C->sem[i].inuse) {
            n++;
            m = i;
        }
    if (maxid) *maxid = m;
    return n;
}

static int msg_count(struct hl_ipc_ctrl *C, int *maxid) {
    int n = 0, m = -1;
    for (int i = 0; i < HL_IPC_MSGMNI; i++)
        if (C->msg[i].inuse) {
            n++;
            m = i;
        }
    if (maxid) *maxid = m;
    return n;
}

// ============================================================================================
//  fork / teardown hooks
// ============================================================================================
static int g_ipc_did_exit; // one-shot guard: exit_group calls sysv_on_exit() explicitly (_exit bypasses
                           // atexit), and a normal host-side exit runs the atexit wrapper -- only one fires.

// fork() clones only the calling thread, leaving the in-process cache mutex possibly inherited-locked; the
// child also inherits the parent's shm ATTACHMENTS (Linux increments shm_nattch for each) and must NOT be
// treated as the namespace creator. SEM_UNDO adjustments are per-process and NOT inherited (Linux resets
// the child's semadj to 0). The SHARED control-block spinlock is untouched (it belongs to every process; a
// dead holder is recovered by hl_ipc_lock's steal). Called from proc.c after fork.
static void sysv_on_exit(void);

static void sysv_after_fork(void) {
    pthread_mutex_init(&g_ipc_local_m, NULL);
    g_ipc_creator = 0;                // only the parent owns the atexit GC
    memset(g_undo, 0, sizeof g_undo); // semadj is not inherited across fork
    g_ipc_did_exit = 0;               // the child gets its own exit pass
    if (g_ctrl) {                     // inherited attachments bump nattch (Linux VM_SHM fork)
        hl_ipc_lock(&g_ctrl->lock);
        for (int i = 0; i < HL_SHMAT_MAX; i++)
            if (g_shmat[i].used) {
                struct hl_shm_entry *s = &g_ctrl->shm[g_shmat[i].idx];
                if (s->inuse) s->nattch++;
            }
        hl_ipc_unlock(&g_ctrl->lock);
        if (!g_ipc_atexit_armed) {  // the child short-circuits hl_ipc_ctrl() (g_ctrl inherited), so
            g_ipc_atexit_armed = 1; // arm its own exit pass here (undo apply on the child's exit)
            atexit(sysv_on_exit);
        }
    } else {
        g_ipc_atexit_armed = 0;
    }
}

// execve detaches every attached shm segment and clears this process's SEM_UNDO adjustments (Linux
// semantics), while the shared registry (control block + queues) survives into the new image. Called from
// proc.c after the CLOEXEC sweep, before the guest address space is torn down.
static void sysv_after_exec(void) {
    struct hl_ipc_ctrl *C = g_ctrl;
    if (C) {
        hl_ipc_lock(&C->lock);
        for (int i = 0; i < HL_SHMAT_MAX; i++)
            if (g_shmat[i].used) {
                struct hl_shm_entry *s = &C->shm[g_shmat[i].idx];
                munmap(g_shmat[i].addr, g_shmat[i].len);
                if (s->inuse) {
                    if (s->nattch) s->nattch--;
                    if (s->removed && s->nattch == 0) shm_free(C, g_shmat[i].idx);
                }
                g_shmat[i].used = 0;
            }
        hl_ipc_unlock(&C->lock);
    }
    memset(g_undo, 0, sizeof g_undo); // semadj is cleared across execve
}

// Apply this process's outstanding SEM_UNDO adjustments (process exit undoes them, Linux semantics) and, if
// we created the namespace (or we are the container init), unlink every live object + the control block.
static void sysv_on_exit(void) {
    if (g_ipc_did_exit) return;
    g_ipc_did_exit = 1;
    struct hl_ipc_ctrl *C = g_ctrl;
    if (!C) return;
    hl_ipc_lock(&C->lock);
    // Process exit detaches every segment this process still holds (Linux: shm_nattch drops, and a segment
    // already marked for deletion is destroyed once nattch hits 0).
    for (int i = 0; i < HL_SHMAT_MAX; i++)
        if (g_shmat[i].used) {
            struct hl_shm_entry *s = &C->shm[g_shmat[i].idx];
            if (s->inuse) {
                if (s->nattch) s->nattch--;
                if (s->removed && s->nattch == 0) shm_free(C, g_shmat[i].idx);
            }
            g_shmat[i].used = 0;
        }
    for (int i = 0; i < HL_UNDO_MAX; i++)
        if (g_undo[i].used) {
            uint32_t idx = g_undo[i].idx;
            if (idx < HL_IPC_SEMMNI && C->sem[idx].inuse && C->sem[idx].perm.seq == g_undo[i].seq &&
                g_undo[i].semnum < C->sem[idx].nsems) {
                int v = (int)C->sem[idx].val[g_undo[i].semnum] - g_undo[i].adj;
                if (v < 0) v = 0;
                if (v > HL_IPC_SEMVMX) v = HL_IPC_SEMVMX;
                C->sem[idx].val[g_undo[i].semnum] = (uint16_t)v;
            }
            g_undo[i].used = 0;
        }
    int gc = g_ipc_creator || (g_init_hostpid && (int)getpid() == g_init_hostpid);
    if (gc) {
        for (int i = 0; i < HL_IPC_SHMMNI; i++)
            if (C->shm[i].inuse) {
                char nm[40];
                hl_ipc_shm_name(nm, sizeof nm, (uint32_t)i);
                shm_unlink(nm);
            }
        for (int i = 0; i < HL_IPC_MSGMNI; i++)
            if (C->msg[i].inuse) {
                char nm[40];
                hl_ipc_message_name(nm, sizeof nm, (uint32_t)i);
                shm_unlink(nm);
            }
    }
    hl_ipc_unlock(&C->lock);
    if (gc) {
        char nm[40];
        hl_ipc_control_name(nm, sizeof nm);
        shm_unlink(nm);
    }
}

static int svc_sysv(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    (void)a5;
    struct hl_ipc_ctrl *C;
    switch (nr) {
    // ===================== SysV shared memory =====================
    case 194: { // shmget(key, size, shmflg)
        int32_t key = (int32_t)a0;
        size_t size = (size_t)a1;
        int flag = (int)a2;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        hl_ipc_lock(&C->lock);
        struct hl_shm_entry *found = NULL;
        if (key != L_IPC_PRIVATE)
            for (int i = 0; i < HL_IPC_SHMMNI; i++)
                if (C->shm[i].inuse && !C->shm[i].removed && C->shm[i].perm.key == key) {
                    found = &C->shm[i];
                    break;
                }
        if (found) {
            if ((flag & L_IPC_CREAT) && (flag & L_IPC_EXCL)) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            }
            if (size && found->segsz < size) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            int perr = hl_ipc_access(&found->perm, 4);
            if (perr) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            uint64_t id = hl_ipc_id(HL_IPC_SHMMNI, shm_idx_of(C, found), found->perm.seq);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = id;
            break;
        }
        if (key != L_IPC_PRIVATE && !(flag & L_IPC_CREAT)) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOENT);
            break;
        }
        if (size == 0 || size > HL_IPC_SHMMAX) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int idx = -1;
        for (int i = 0; i < HL_IPC_SHMMNI; i++)
            if (!C->shm[i].inuse) {
                idx = i;
                break;
            }
        if (idx < 0) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        char nm[40];
        hl_ipc_shm_name(nm, sizeof nm, (uint32_t)idx);
        shm_unlink(nm);
        int fd = shm_open(nm, O_CREAT | O_EXCL | O_RDWR, 0600);
        if (fd < 0) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        if (ftruncate(fd, (off_t)hl_ipc_pground(size)) < 0) {
            close(fd);
            shm_unlink(nm);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        close(fd);
        struct hl_shm_entry *s = &C->shm[idx];
        uint32_t seq = s->perm.seq;
        memset(s, 0, sizeof *s);
        s->perm.seq = seq;
        hl_perm_init(&s->perm, key, flag);
        s->segsz = size;
        s->cpid = container_pid();
        s->ctime = hl_ipc_now();
        s->inuse = 1;
        uint64_t id = hl_ipc_id(HL_IPC_SHMMNI, (uint32_t)idx, seq);
        hl_ipc_unlock(&C->lock);
        G_RET(c) = id;
        break;
    }
    case 196: { // shmat(shmid, shmaddr, shmflg)
        int id = (int)a0, flag = (int)a2;
        void *shmaddr = (void *)a1;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        hl_ipc_lock(&C->lock);
        struct hl_shm_entry *s = shm_by_id(C, id);
        if (!s) { // a removed-but-attached id resolves nowhere new; report EIDRM if it exists-but-removed
            uint32_t idx = (uint32_t)id % HL_IPC_SHMMNI;
            int eid = (id >= 0 && C->shm[idx].inuse && C->shm[idx].removed &&
                       C->shm[idx].perm.seq == (uint32_t)id / HL_IPC_SHMMNI);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(eid ? -EIDRM : -EINVAL);
            break;
        }
        int want = (flag & L_SHM_RDONLY) ? 4 : 6;
        int perr = hl_ipc_access(&s->perm, want);
        if (perr) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)perr;
            break;
        }
        uint32_t idx = shm_idx_of(C, s);
        size_t len = hl_ipc_pground(s->segsz);
        char nm[40];
        hl_ipc_shm_name(nm, sizeof nm, idx);
        hl_ipc_unlock(&C->lock); // shm_open/mmap can be slow -- don't hold the lock across them
        int fd = shm_open(nm, O_RDWR, 0600);
        if (fd < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int prot = (flag & L_SHM_RDONLY) ? PROT_READ : (PROT_READ | PROT_WRITE);
        void *want_addr = NULL;
        int mflags = MAP_SHARED;
        if (shmaddr) {
            uintptr_t a = (uintptr_t)shmaddr;
            size_t pg = hl_linux_host_map_granularity();
            if (flag & L_SHM_RND)
                a &= ~(uintptr_t)(pg - 1);
            else if (a & (pg - 1)) {
                close(fd);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            want_addr = (void *)a;
            mflags |= MAP_FIXED;
        }
        void *p = mmap(want_addr, len, prot, mflags, fd, 0);
        close(fd);
        if (p == MAP_FAILED) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        pthread_mutex_lock(&g_ipc_local_m);
        int tracked = 0;
        for (int i = 0; i < HL_SHMAT_MAX; i++)
            if (!g_shmat[i].used) {
                g_shmat[i].used = 1;
                g_shmat[i].addr = p;
                g_shmat[i].idx = idx;
                g_shmat[i].len = len;
                tracked = 1;
                break;
            }
        pthread_mutex_unlock(&g_ipc_local_m);
        if (!tracked) {
            // Attach table full: an untracked mapping can never be found by shmdt -> its munmap AND the
            // matching nattch-- both leak (the segment then never reaches nattch==0 to be freed on RMID).
            // Undo the map and report ENOMEM (a valid shmat failure) instead of returning a leaking pointer.
            munmap(p, len);
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        hl_ipc_lock(&C->lock);
        s = shm_by_id(C, id);
        if (s) {
            s->nattch++;
            s->lpid = container_pid();
            s->atime = hl_ipc_now();
        }
        hl_ipc_unlock(&C->lock);
        G_RET(c) = (uint64_t)p;
        break;
    }
    case 197: { // shmdt(shmaddr)
        void *addr = (void *)a0;
        C = hl_ipc_ctrl();
        pthread_mutex_lock(&g_ipc_local_m);
        int slot = -1;
        for (int i = 0; i < HL_SHMAT_MAX; i++)
            if (g_shmat[i].used && g_shmat[i].addr == addr) {
                slot = i;
                break;
            }
        if (slot < 0) {
            pthread_mutex_unlock(&g_ipc_local_m);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t idx = g_shmat[slot].idx;
        size_t len = g_shmat[slot].len;
        g_shmat[slot].used = 0;
        pthread_mutex_unlock(&g_ipc_local_m);
        munmap(addr, len);
        if (C) {
            hl_ipc_lock(&C->lock);
            struct hl_shm_entry *s = &C->shm[idx];
            if (s->inuse) {
                if (s->nattch) s->nattch--;
                s->lpid = container_pid();
                s->dtime = hl_ipc_now();
                if (s->removed && s->nattch == 0) shm_free(C, idx);
            }
            hl_ipc_unlock(&C->lock);
        }
        G_RET(c) = 0;
        break;
    }
    case 195: { // shmctl(shmid, cmd, buf)
        int id = (int)a0, cmd = (int)a1;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (cmd == L_IPC_INFO || cmd == L_SHM_INFO) {
            hl_ipc_lock(&C->lock);
            int maxid = -1, n = shm_count(C, &maxid);
            uint64_t rc = 0;
            if (cmd == L_IPC_INFO) {
                struct shminfo_guest info = {.shmmax = HL_IPC_SHMMAX,
                                             .shmmin = 1,
                                             .shmmni = HL_IPC_SHMMNI_ADV,
                                             .shmseg = HL_IPC_SHMMNI_ADV,
                                             .shmall = HL_IPC_SHMMAX / 4096};
                if (guest_copy_to(a2, &info, sizeof(info)) != sizeof(info)) rc = (uint64_t)(-EFAULT);
            } else {
                struct shm_info_guest info = {.used_ids = n};
                if (guest_copy_to(a2, &info, sizeof(info)) != sizeof(info)) rc = (uint64_t)(-EFAULT);
            }
            hl_ipc_unlock(&C->lock);
            G_RET(c) = rc ? rc : (uint64_t)(maxid < 0 ? 0 : maxid);
            break;
        }
        if (cmd == L_SHM_STAT || cmd == L_SHM_STAT_ANY) {
            hl_ipc_lock(&C->lock);
            if (id < 0 || id >= HL_IPC_SHMMNI || !C->shm[id].inuse) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (cmd == L_SHM_STAT) {
                int perr = hl_ipc_access(&C->shm[id].perm, 4);
                if (perr) {
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)perr;
                    break;
                }
            }
            uint64_t retid = hl_ipc_id(HL_IPC_SHMMNI, (uint32_t)id, C->shm[id].perm.seq);
            uint64_t rc = shm_stat_to_guest(C, (uint32_t)id, a2);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = rc ? rc : retid;
            break;
        }
        hl_ipc_lock(&C->lock);
        struct hl_shm_entry *s = shm_by_id(C, id);
        if (!s) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t idx = shm_idx_of(C, s);
        uint64_t rc;
        switch (cmd) {
        case L_IPC_STAT: {
            int perr = hl_ipc_access(&s->perm, 4);
            rc = perr ? (uint64_t)perr : shm_stat_to_guest(C, idx, a2);
            break;
        }
        case L_IPC_SET: {
            int perr = hl_ipc_owner(&s->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            struct shmid64_ds_guest value;
            if (guest_copy_from(&value, a2, sizeof(value)) != sizeof(value)) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            s->perm.uid = value.shm_perm.uid;
            s->perm.gid = value.shm_perm.gid;
            s->perm.mode = (s->perm.mode & ~0777u) | (value.shm_perm.mode & 0777);
            s->ctime = hl_ipc_now();
            rc = 0;
            break;
        }
        case L_IPC_RMID: {
            int perr = hl_ipc_owner(&s->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            s->removed = 1;
            s->perm.key = L_IPC_PRIVATE; // no longer findable by key
            if (s->nattch == 0) shm_free(C, idx);
            rc = 0;
            break;
        }
        case L_SHM_LOCK:
        case L_SHM_UNLOCK: // no wired pages to (un)lock; just gate on ownership (Linux CAP_IPC_LOCK/owner)
            rc = (uint64_t)hl_ipc_owner(&s->perm);
            break;
        default: rc = (uint64_t)(-EINVAL); break;
        }
        hl_ipc_unlock(&C->lock);
        G_RET(c) = rc;
        break;
    }

    // ===================== SysV semaphores =====================
    case 190: { // semget(key, nsems, semflg)
        int32_t key = (int32_t)a0;
        int nsems = (int)a1, flag = (int)a2;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        hl_ipc_lock(&C->lock);
        struct hl_sem_entry *found = NULL;
        if (key != L_IPC_PRIVATE)
            for (int i = 0; i < HL_IPC_SEMMNI; i++)
                if (C->sem[i].inuse && C->sem[i].perm.key == key) {
                    found = &C->sem[i];
                    break;
                }
        if (found) {
            if ((flag & L_IPC_CREAT) && (flag & L_IPC_EXCL)) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            }
            if (nsems > 0 && (uint32_t)nsems > found->nsems) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            int perr = hl_ipc_access(&found->perm, 4);
            if (perr) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            uint64_t id = hl_ipc_id(HL_IPC_SEMMNI, sem_idx_of(C, found), found->perm.seq);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = id;
            break;
        }
        if (key != L_IPC_PRIVATE && !(flag & L_IPC_CREAT)) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOENT);
            break;
        }
        if (nsems <= 0 || nsems > HL_IPC_SEMMSL) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int idx = -1;
        for (int i = 0; i < HL_IPC_SEMMNI; i++)
            if (!C->sem[i].inuse) {
                idx = i;
                break;
            }
        if (idx < 0) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        struct hl_sem_entry *s = &C->sem[idx];
        uint32_t seq = s->perm.seq;
        memset(s, 0, sizeof *s);
        s->perm.seq = seq;
        hl_perm_init(&s->perm, key, flag);
        s->nsems = (uint32_t)nsems;
        s->ctime = hl_ipc_now();
        s->inuse = 1;
        uint64_t id = hl_ipc_id(HL_IPC_SEMMNI, (uint32_t)idx, seq);
        hl_ipc_unlock(&C->lock);
        G_RET(c) = id;
        break;
    }
    case 192:   // semtimedop(semid, sops, nsops, timeout)
    case 193: { // semop(semid, sops, nsops)
        int id = (int)a0;
        size_t nsops = (size_t)a2;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (nsops == 0 || nsops > HL_IPC_SEMOPM_ADV) {
            G_RET(c) = (uint64_t)(nsops == 0 ? -EINVAL : -E2BIG);
            break;
        }
        struct sembuf_guest *sops = malloc(nsops * sizeof(*sops));
        if (!sops) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (guest_copy_from(sops, a1, nsops * sizeof(*sops)) != (ssize_t)(nsops * sizeof(*sops))) {
            free(sops);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Optional relative timeout (semtimedop): compute an absolute monotonic deadline.
        struct timespec deadline;
        int have_deadline = 0;
        if (nr == 192 && a3) {
            struct timespec timeout;
            if (guest_copy_from(&timeout, a3, sizeof(timeout)) != sizeof(timeout)) {
                free(sops);
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            struct timespec *to = &timeout;
            if (to->tv_nsec < 0 || to->tv_nsec >= 1000000000L || to->tv_sec < 0) {
                free(sops);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += to->tv_sec;
            deadline.tv_nsec += to->tv_nsec;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_nsec -= 1000000000L;
                deadline.tv_sec++;
            }
            have_deadline = 1;
        }
        int did_wait = 0, waited_marked = 0;
        for (;;) {
            hl_ipc_lock(&C->lock);
            struct hl_sem_entry *s = sem_by_id(C, id);
            if (!s) {
                if (waited_marked) waited_marked = 0; // set gone while blocking -> EIDRM
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(did_wait ? -EIDRM : -EINVAL);
                break;
            }
            // Validate every sem_num up front.
            int bad = 0;
            for (size_t i = 0; i < nsops; i++)
                if (sops[i].sem_num >= s->nsems) {
                    bad = 1;
                    break;
                }
            if (bad) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EFBIG); // EFBIG(27 mac==linux) -- Linux returns EFBIG for sem_num OOR
                break;
            }
            // Can every op proceed atomically?
            int block_on = -1, would_block = 0;
            for (size_t i = 0; i < nsops; i++) {
                int cur = (int)s->val[sops[i].sem_num], op = sops[i].sem_op;
                if (op == 0) {
                    if (cur != 0) {
                        would_block = 1;
                        block_on = (int)i;
                        break;
                    }
                } else if (op < 0) {
                    if (cur + op < 0) {
                        would_block = 1;
                        block_on = (int)i;
                        break;
                    }
                } else if (cur + op > HL_IPC_SEMVMX) {
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-ERANGE);
                    goto sem_done;
                }
            }
            if (!would_block) {
                if (waited_marked) { // leaving the wait: drop our ncnt/zcnt bookkeeping
                    for (size_t i = 0; i < nsops; i++) {
                        if (sops[i].sem_op < 0 && s->ncnt[sops[i].sem_num] > 0)
                            s->ncnt[sops[i].sem_num]--;
                        else if (sops[i].sem_op == 0 && s->zcnt[sops[i].sem_num] > 0)
                            s->zcnt[sops[i].sem_num]--;
                    }
                    waited_marked = 0;
                }
                int gp = container_pid();
                for (size_t i = 0; i < nsops; i++) {
                    int op = sops[i].sem_op;
                    s->val[sops[i].sem_num] = (uint16_t)((int)s->val[sops[i].sem_num] + op);
                    s->pid[sops[i].sem_num] = gp;
                    if ((sops[i].sem_flg & L_SEM_UNDO) && op != 0)
                        sem_undo_add(sem_idx_of(C, s), s->perm.seq, sops[i].sem_num, op);
                }
                s->otime = hl_ipc_now();
                hl_ipc_unlock(&C->lock);
                G_RET(c) = 0;
                break;
            }
            // Cannot proceed: NOWAIT -> EAGAIN; else register as a waiter and poll.
            if (sops[block_on].sem_flg & L_IPC_NOWAIT) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EAGAIN);
                break;
            }
            if (!waited_marked) {
                for (size_t i = 0; i < nsops; i++) {
                    if (sops[i].sem_op < 0)
                        s->ncnt[sops[i].sem_num]++;
                    else if (sops[i].sem_op == 0)
                        s->zcnt[sops[i].sem_num]++;
                }
                waited_marked = 1;
            }
            hl_ipc_unlock(&C->lock);
            did_wait = 1;
            if (have_deadline) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                if (now.tv_sec > deadline.tv_sec ||
                    (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
                    hl_ipc_lock(&C->lock);
                    struct hl_sem_entry *s2 = sem_by_id(C, id);
                    if (s2)
                        for (size_t i = 0; i < nsops; i++) {
                            if (sops[i].sem_op < 0 && s2->ncnt[sops[i].sem_num] > 0)
                                s2->ncnt[sops[i].sem_num]--;
                            else if (sops[i].sem_op == 0 && s2->zcnt[sops[i].sem_num] > 0)
                                s2->zcnt[sops[i].sem_num]--;
                        }
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
            }
            struct timespec ts = {0, 200000}; // 200us poll
            if (nanosleep(&ts, NULL) < 0 && errno == EINTR) {
                hl_ipc_lock(&C->lock);
                struct hl_sem_entry *s2 = sem_by_id(C, id);
                if (s2)
                    for (size_t i = 0; i < nsops; i++) {
                        if (sops[i].sem_op < 0 && s2->ncnt[sops[i].sem_num] > 0)
                            s2->ncnt[sops[i].sem_num]--;
                        else if (sops[i].sem_op == 0 && s2->zcnt[sops[i].sem_num] > 0)
                            s2->zcnt[sops[i].sem_num]--;
                    }
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINTR);
                break;
            }
        }
    sem_done:
        free(sops);
        break;
    }
    case 191: { // semctl(semid, semnum, cmd, arg)
        int id = (int)a0, semnum = (int)a1, cmd = (int)a2;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (cmd == L_IPC_INFO || cmd == L_SEM_INFO) {
            hl_ipc_lock(&C->lock);
            int maxid = -1, n = sem_count(C, &maxid);
            uint64_t rc = 0;
            struct seminfo_guest info;
            memset(&info, 0, sizeof info);
            info.semmni = HL_IPC_SEMMNI_ADV;
            info.semmsl = HL_IPC_SEMMSL_ADV;
            info.semmns = HL_IPC_SEMMNS_ADV;
            info.semopm = HL_IPC_SEMOPM_ADV;
            info.semvmx = HL_IPC_SEMVMX;
            info.semaem = HL_IPC_SEMVMX;
            info.semmnu = 2147483647;
            info.semume = HL_IPC_SEMOPM_ADV;
            if (cmd == L_SEM_INFO) {
                info.semusz = n;
                info.semaem = n;
            }
            if (guest_copy_to(a3, &info, sizeof(info)) != sizeof(info)) rc = (uint64_t)(-EFAULT);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = rc ? rc : (uint64_t)(maxid < 0 ? 0 : maxid);
            break;
        }
        if (cmd == L_SEM_STAT || cmd == L_SEM_STAT_ANY) {
            hl_ipc_lock(&C->lock);
            if (id < 0 || id >= HL_IPC_SEMMNI || !C->sem[id].inuse) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (cmd == L_SEM_STAT) {
                int perr = hl_ipc_access(&C->sem[id].perm, 4);
                if (perr) {
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)perr;
                    break;
                }
            }
            uint64_t retid = hl_ipc_id(HL_IPC_SEMMNI, (uint32_t)id, C->sem[id].perm.seq);
            uint64_t rc = sem_stat_to_guest(C, (uint32_t)id, a3);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = rc ? rc : retid;
            break;
        }
        hl_ipc_lock(&C->lock);
        struct hl_sem_entry *s = sem_by_id(C, id);
        if (!s) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t idx = sem_idx_of(C, s);
        uint64_t rc;
        switch (cmd) {
        case L_IPC_STAT: {
            int perr = hl_ipc_access(&s->perm, 4);
            rc = perr ? (uint64_t)perr : sem_stat_to_guest(C, idx, a3);
            break;
        }
        case L_IPC_SET: {
            int perr = hl_ipc_owner(&s->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            struct semid64_ds_guest value;
            if (guest_copy_from(&value, a3, sizeof(value)) != sizeof(value)) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            s->perm.uid = value.sem_perm.uid;
            s->perm.gid = value.sem_perm.gid;
            s->perm.mode = (s->perm.mode & ~0777u) | (value.sem_perm.mode & 0777);
            s->ctime = hl_ipc_now();
            rc = 0;
            break;
        }
        case L_IPC_RMID: {
            int perr = hl_ipc_owner(&s->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            sem_undo_clear(idx, s->perm.seq, -1);
            sem_free(C, idx);
            rc = 0;
            break;
        }
        case L_GETVAL: {
            int perr = hl_ipc_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)s->val[semnum];
            break;
        }
        case L_GETPID: {
            int perr = hl_ipc_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)(uint32_t)s->pid[semnum];
            break;
        }
        case L_GETNCNT: {
            int perr = hl_ipc_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)(uint32_t)s->ncnt[semnum];
            break;
        }
        case L_GETZCNT: {
            int perr = hl_ipc_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)(uint32_t)s->zcnt[semnum];
            break;
        }
        case L_SETVAL: {
            int perr = hl_ipc_access(&s->perm, 2);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            if (semnum < 0 || (uint32_t)semnum >= s->nsems) {
                rc = (uint64_t)(-EINVAL);
                break;
            }
            int v = (int)a3;
            if (v < 0 || v > HL_IPC_SEMVMX) {
                rc = (uint64_t)(-ERANGE);
                break;
            }
            s->val[semnum] = (uint16_t)v;
            s->pid[semnum] = container_pid();
            s->ctime = hl_ipc_now();
            sem_undo_clear(idx, s->perm.seq, semnum);
            rc = 0;
            break;
        }
        case L_GETALL: {
            int perr = hl_ipc_access(&s->perm, 4);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            uint16_t values[HL_IPC_SEMMSL_ADV];
            for (uint32_t i = 0; i < s->nsems; i++)
                values[i] = s->val[i];
            if (guest_copy_to(a3, values, s->nsems * sizeof(uint16_t)) != (ssize_t)(s->nsems * sizeof(uint16_t))) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            rc = 0;
            break;
        }
        case L_SETALL: {
            int perr = hl_ipc_access(&s->perm, 2);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            uint16_t arr[HL_IPC_SEMMSL_ADV];
            if (guest_copy_from(arr, a3, s->nsems * sizeof(uint16_t)) != (ssize_t)(s->nsems * sizeof(uint16_t))) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            for (uint32_t i = 0; i < s->nsems; i++) {
                if (arr[i] > HL_IPC_SEMVMX) {
                    rc = (uint64_t)(-ERANGE);
                    goto sem_setall_out;
                }
            }
            for (uint32_t i = 0; i < s->nsems; i++)
                s->val[i] = arr[i];
            s->ctime = hl_ipc_now();
            sem_undo_clear(idx, s->perm.seq, -1);
            rc = 0;
        sem_setall_out:
            break;
        }
        default: rc = (uint64_t)(-EINVAL); break;
        }
        hl_ipc_unlock(&C->lock);
        G_RET(c) = rc;
        break;
    }

    // ===================== SysV message queues =====================
    case 186: { // msgget(key, msgflg)
        int32_t key = (int32_t)a0;
        int flag = (int)a1;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        hl_ipc_lock(&C->lock);
        struct hl_msg_queue *found = NULL;
        if (key != L_IPC_PRIVATE)
            for (int i = 0; i < HL_IPC_MSGMNI; i++)
                if (C->msg[i].inuse && !C->msg[i].removed && C->msg[i].perm.key == key) {
                    found = &C->msg[i];
                    break;
                }
        if (found) {
            if ((flag & L_IPC_CREAT) && (flag & L_IPC_EXCL)) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            }
            int perr = hl_ipc_access(&found->perm, 4);
            if (perr) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            uint64_t id = hl_ipc_id(HL_IPC_MSGMNI, msg_idx_of(C, found), found->perm.seq);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = id;
            break;
        }
        if (key != L_IPC_PRIVATE && !(flag & L_IPC_CREAT)) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOENT);
            break;
        }
        int idx = -1;
        for (int i = 0; i < HL_IPC_MSGMNI; i++)
            if (!C->msg[i].inuse) {
                idx = i;
                break;
            }
        if (idx < 0) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        struct hl_msg_queue *q = &C->msg[idx];
        uint32_t seq = q->perm.seq;
        memset(q, 0, sizeof *q);
        q->perm.seq = seq;
        hl_perm_init(&q->perm, key, flag);
        q->qbytes = HL_IPC_MSGMNB;
        q->ctime = hl_ipc_now();
        q->inuse = 1;
        hl_ipc_unlock(&C->lock);
        // Create the backing store OUTSIDE the lock (shm_open/ftruncate can be slow).
        if (!hl_ipc_msg_store((uint32_t)idx, seq, 1)) {
            hl_ipc_lock(&C->lock);
            if (q->inuse && q->perm.seq == seq) msg_free(C, (uint32_t)idx);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        G_RET(c) = hl_ipc_id(HL_IPC_MSGMNI, (uint32_t)idx, seq);
        break;
    }
    case 189: { // msgsnd(msqid, msgp, msgsz, msgflg)
        int id = (int)a0;
        size_t msgsz = (size_t)a2;
        int flag = (int)a3;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (msgsz > HL_MSG_MAX_SIZE) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint8_t *message = malloc(HL_IPC_MSG_TYPE_SIZE + msgsz);
        if (!message) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if (guest_copy_from(message, a1, HL_IPC_MSG_TYPE_SIZE + msgsz) != (ssize_t)(HL_IPC_MSG_TYPE_SIZE + msgsz)) {
            free(message);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int64_t mtype;
        memcpy(&mtype, message, sizeof(mtype));
        if (mtype < 1) {
            free(message);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        const uint8_t *body = message + HL_IPC_MSG_TYPE_SIZE;
        int did_wait = 0;
        for (;;) {
            hl_ipc_lock(&C->lock);
            struct hl_msg_queue *q = msg_by_id(C, id);
            if (!q) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(did_wait ? -EIDRM : -EINVAL);
                break;
            }
            uint32_t idx = msg_idx_of(C, q);
            uint32_t qseq = q->perm.seq;
            int perr = hl_ipc_access(&q->perm, 2);
            if (perr) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            int full = (q->cbytes + msgsz > q->qbytes) || (q->qnum >= HL_MSG_SLOTS);
            if (full) {
                if (flag & L_IPC_NOWAIT) {
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
                hl_ipc_unlock(&C->lock);
                did_wait = 1;
                struct timespec ts = {0, 200000};
                if (nanosleep(&ts, NULL) < 0 && errno == EINTR) {
                    G_RET(c) = (uint64_t)(-EINTR);
                    break;
                }
                continue;
            }
            struct hl_ipc_msg_store *st = hl_ipc_msg_store(idx, qseq, 0);
            if (!st || st->freehead < 0) {
                hl_ipc_unlock(&C->lock);
                if (flag & L_IPC_NOWAIT) {
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
                did_wait = 1;
                struct timespec ts = {0, 200000};
                if (nanosleep(&ts, NULL) < 0 && errno == EINTR) {
                    G_RET(c) = (uint64_t)(-EINTR);
                    break;
                }
                continue;
            }
            int slot = st->freehead;
            st->freehead = st->slots[slot].next;
            st->slots[slot].mtype = mtype;
            st->slots[slot].size = (uint32_t)msgsz;
            st->slots[slot].next = -1;
            if (msgsz) memcpy(st->slots[slot].data, body, msgsz);
            if (st->tail < 0)
                st->head = st->tail = slot;
            else {
                st->slots[st->tail].next = slot;
                st->tail = slot;
            }
            q->qnum++;
            q->cbytes += msgsz;
            q->stime = hl_ipc_now();
            q->lspid = container_pid();
            hl_ipc_unlock(&C->lock);
            G_RET(c) = 0;
            break;
        }
        free(message);
        break;
    }
    case 188: { // msgrcv(msqid, msgp, msgsz, msgtyp, msgflg)
        int id = (int)a0;
        size_t msgsz = (size_t)a2;
        int64_t msgtyp = (int64_t)a3;
        int flag = (int)a4;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (guest_accessible_prefix(a1, HL_IPC_MSG_TYPE_SIZE + msgsz, PROT_WRITE) != HL_IPC_MSG_TYPE_SIZE + msgsz) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int did_wait = 0;
        for (;;) {
            hl_ipc_lock(&C->lock);
            struct hl_msg_queue *q = msg_by_id(C, id);
            if (!q) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(did_wait ? -EIDRM : -EINVAL);
                break;
            }
            uint32_t idx = msg_idx_of(C, q);
            uint32_t qseq = q->perm.seq;
            int perr = hl_ipc_access(&q->perm, 4);
            if (perr) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            struct hl_ipc_msg_store *st = hl_ipc_msg_store(idx, qseq, 0);
            if (!st) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Select a message: type 0 = first; >0 = first of that type (or first NOT of it w/ MSG_EXCEPT);
            // <0 = the message with the lowest mtype that is <= |msgtyp|.
            int prev = -1, cur = st->head, best = -1, bestprev = -1;
            while (cur != -1) {
                struct hl_ipc_msg_slot *sl = &st->slots[cur];
                if (msgtyp == 0) {
                    best = cur;
                    bestprev = prev;
                    break;
                } else if (msgtyp > 0) {
                    int match = (flag & L_MSG_EXCEPT) ? (sl->mtype != msgtyp) : (sl->mtype == msgtyp);
                    if (match) {
                        best = cur;
                        bestprev = prev;
                        break;
                    }
                } else {
                    if (sl->mtype <= -msgtyp && (best == -1 || sl->mtype < st->slots[best].mtype)) {
                        best = cur;
                        bestprev = prev;
                    }
                }
                prev = cur;
                cur = sl->next;
            }
            if (best >= 0) {
                struct hl_ipc_msg_slot *sl = &st->slots[best];
                if (sl->size > msgsz && !(flag & L_MSG_NOERROR)) {
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-E2BIG);
                    break;
                }
                size_t copy = sl->size > msgsz ? msgsz : sl->size;
                uint8_t *message = malloc(HL_IPC_MSG_TYPE_SIZE + copy);
                if (!message) {
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-ENOMEM);
                    break;
                }
                memcpy(message, &sl->mtype, HL_IPC_MSG_TYPE_SIZE);
                if (copy) memcpy(message + HL_IPC_MSG_TYPE_SIZE, sl->data, copy);
                if (guest_copy_to(a1, message, HL_IPC_MSG_TYPE_SIZE + copy) != (ssize_t)(HL_IPC_MSG_TYPE_SIZE + copy)) {
                    free(message);
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-EFAULT);
                    break;
                }
                free(message);
                // unlink best from the list
                if (bestprev < 0)
                    st->head = sl->next;
                else
                    st->slots[bestprev].next = sl->next;
                if (st->tail == best) st->tail = bestprev;
                sl->next = st->freehead;
                st->freehead = best;
                q->qnum--;
                q->cbytes -= sl->size;
                q->rtime = hl_ipc_now();
                q->lrpid = container_pid();
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)copy;
                break;
            }
            if (flag & L_IPC_NOWAIT) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-ENOMSG);
                break;
            }
            hl_ipc_unlock(&C->lock);
            did_wait = 1;
            struct timespec ts = {0, 200000};
            if (nanosleep(&ts, NULL) < 0 && errno == EINTR) {
                G_RET(c) = (uint64_t)(-EINTR);
                break;
            }
        }
        break;
    }
    case 187: { // msgctl(msqid, cmd, buf)
        int id = (int)a0, cmd = (int)a1;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (cmd == L_IPC_INFO || cmd == L_MSG_INFO) {
            hl_ipc_lock(&C->lock);
            int maxid = -1, n = msg_count(C, &maxid);
            uint64_t rc = 0;
            struct msginfo_guest info;
            memset(&info, 0, sizeof info);
            info.msgmax = HL_IPC_MSGMAX;
            info.msgmni = HL_IPC_MSGMNI_ADV;
            info.msgmnb = HL_IPC_MSGMNB;
            info.msgssz = 16;
            info.msgtql = HL_IPC_MSGMNI_ADV;
            info.msgseg = 0xffff;
            if (cmd == L_MSG_INFO) {
                info.msgpool = n;
                info.msgtql = n;
            }
            if (guest_copy_to(a2, &info, sizeof(info)) != sizeof(info)) rc = (uint64_t)(-EFAULT);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = rc ? rc : (uint64_t)(maxid < 0 ? 0 : maxid);
            break;
        }
        if (cmd == L_MSG_STAT || cmd == L_MSG_STAT_ANY) {
            hl_ipc_lock(&C->lock);
            if (id < 0 || id >= HL_IPC_MSGMNI || !C->msg[id].inuse) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (cmd == L_MSG_STAT) {
                int perr = hl_ipc_access(&C->msg[id].perm, 4);
                if (perr) {
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)perr;
                    break;
                }
            }
            uint64_t retid = hl_ipc_id(HL_IPC_MSGMNI, (uint32_t)id, C->msg[id].perm.seq);
            uint64_t rc = msg_stat_to_guest(C, (uint32_t)id, a2);
            hl_ipc_unlock(&C->lock);
            G_RET(c) = rc ? rc : retid;
            break;
        }
        hl_ipc_lock(&C->lock);
        struct hl_msg_queue *q = msg_by_id(C, id);
        if (!q) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t idx = msg_idx_of(C, q);
        uint64_t rc;
        switch (cmd) {
        case L_IPC_STAT: {
            int perr = hl_ipc_access(&q->perm, 4);
            rc = perr ? (uint64_t)perr : msg_stat_to_guest(C, idx, a2);
            break;
        }
        case L_IPC_SET: {
            int perr = hl_ipc_owner(&q->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            struct msqid64_ds_guest value;
            if (guest_copy_from(&value, a2, sizeof(value)) != sizeof(value)) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            // Raising qbytes above the default ceiling needs privilege (CAP_SYS_RESOURCE); lowering is free.
            if (value.msg_qbytes > HL_IPC_MSGMNB && cred_euid() != 0) {
                rc = (uint64_t)(-EPERM);
                break;
            }
            q->perm.uid = value.msg_perm.uid;
            q->perm.gid = value.msg_perm.gid;
            q->perm.mode = (q->perm.mode & ~0777u) | (value.msg_perm.mode & 0777);
            if (value.msg_qbytes) q->qbytes = value.msg_qbytes;
            q->ctime = hl_ipc_now();
            rc = 0;
            break;
        }
        case L_IPC_RMID: {
            int perr = hl_ipc_owner(&q->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            msg_free(C, idx);
            rc = 0;
            break;
        }
        default: rc = (uint64_t)(-EINVAL); break;
        }
        hl_ipc_unlock(&C->lock);
        G_RET(c) = rc;
        break;
    }
    default: return 0;
    }
    // Map the host(macOS) errno left in G_RET to the Linux errno the guest expects (e.g. ENOMSG 91->42,
    // EIDRM 90->43, EAGAIN 35->11). Like every other svc_<family>() tail, sysv early-returns from
    // service_local before its trailing m2l boundary, so it must translate here.
    return svc_done(c);
}
