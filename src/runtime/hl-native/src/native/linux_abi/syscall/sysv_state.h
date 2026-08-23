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
// svc_done_host() does the macOS->Linux boundary translation at the tail (e.g. ENOMSG 91->42, EIDRM 90->43,
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
