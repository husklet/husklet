// Extracted from service(): SysV IPC syscalls (shm/sem/msg). Returns 1 if nr was handled (G_RET set), 0 otherwise.
// Included by service.c after service/helpers.c, before service(); sees the same TU scope (globals + helpers).
//
// ============================================================================================
// -- dd-INTERNAL SysV IPC emulation (was: host-macOS shmget/semget/msgget passthrough).
// --------------------------------------------------------------------------------------------
// The macOS host SysV table is tiny (kern.sysv.shmmni=32) and GLOBAL: it is not per-container, so real
// software (postgres allocates many segments) hit ENOSPC where Linux succeeds, every container + the whole
// test matrix shared one 32-slot table, and a killed run leaked mode-0 segments that filled it. We no
// longer touch the host SysV table at all. Instead each container gets a private in-shared-memory registry
// with Linux-like limits:
//
//   * A per-container CONTROL BLOCK, a named POSIX shared-memory object (shm_open) keyed by the container
//     identity (DD_NETNS, else the container init / engine-root pid) so two containers never collide and a
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
// raw-syscall probe on both arches. CANON_X86ONLY is defined only in the x86_64 engine (translate/x86_64).
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
// dd-internal shared registry
// ============================================================================================
// Advertised Linux-like limits (also mirrored in /proc/sys/kernel/{shmmni,shmmax,shmall,sem,msgmni,...}).
#define DDIPC_SHMMAX 0xffffffffffffffffULL
#define DDIPC_SHMMNI_ADV 4096
#define DDIPC_SEMMNI_ADV 32000
#define DDIPC_SEMMSL_ADV 32000
#define DDIPC_SEMMNS_ADV 1024000000
#define DDIPC_SEMOPM_ADV 500
#define DDIPC_SEMVMX 32767
#define DDIPC_MSGMAX 8192
#define DDIPC_MSGMNB 16384
#define DDIPC_MSGMNI_ADV 32000
// Table capacities we actually allocate + enforce (all >> the host's 32; well beyond what real software
// needs). shm matches the advertised limit; sem/msg are capped lower than the advertised (Linux sizes them
// dynamically -- impractical in a fixed shared block) but far above any realistic use.
#define DDIPC_SHMMNI 4096            // shm segment descriptors (metadata only; data in a per-segment object)
#define DDIPC_SEMMNI 512             // semaphore SETS
#define DDIPC_SEMMSL 256             // semaphores per set (inline values)
#define DDIPC_MSGMNI 512             // message queues (metadata; data in a per-queue object)
#define DDMSG_SLOTS 512              // messages a single queue can hold
#define DDMSG_MAXSZ 8192             // == MSGMAX: largest single message body
#define DDIPC_CTRL_MAGIC 0x44494943u // "DIIC"
#define DDMSG_MAGIC 0x44494d51u      // "DIMQ"

// A cross-process robust spinlock: 0 == free, else == holder host pid. A holder that dies with the lock
// held is detected (kill(pid,0)==ESRCH) and its lock stolen -- macOS has no PTHREAD_MUTEX_ROBUST, and the
// critical sections are short (table edits + a couple of shm_open/ftruncate; never a blocking wait).
struct ddlock {
    atomic_uint owner;
};

// The container-visible permission block (we store the GUEST identity natively -- no host<->guest map).
struct ddperm {
    int32_t key;
    uint32_t uid, gid, cuid, cgid, mode, seq;
};

struct ddshm {
    uint32_t inuse, removed;
    struct ddperm perm;
    uint64_t segsz; // caller-requested size (reported by IPC_STAT, Linux-faithful)
    int32_t cpid, lpid;
    int64_t atime, dtime, ctime;
    uint32_t nattch; // authoritative attach count across all processes
};

struct ddsem {
    uint32_t inuse;
    struct ddperm perm;
    uint32_t nsems;
    int64_t otime, ctime;
    uint16_t val[DDIPC_SEMMSL];
    int32_t pid[DDIPC_SEMMSL];  // last process to op each sem (GETPID)
    int32_t ncnt[DDIPC_SEMMSL]; // processes waiting for the sem to rise (GETNCNT)
    int32_t zcnt[DDIPC_SEMMSL]; // processes waiting for the sem to reach 0 (GETZCNT)
};

struct ddmsgq {
    uint32_t inuse, removed;
    struct ddperm perm;
    int64_t stime, rtime, ctime;
    int32_t lspid, lrpid;
    uint64_t qnum, cbytes, qbytes;
};

struct ddipc_ctrl {
    atomic_uint magic;
    struct ddlock lock;
    struct ddshm shm[DDIPC_SHMMNI];
    struct ddsem sem[DDIPC_SEMMNI];
    struct ddmsgq msg[DDIPC_MSGMNI];
};

// A message queue's backing object: a slot ring + free list. head/tail are the FIFO order; msgrcv may
// unlink any matching slot from the middle.
struct ddmsg_slot {
    long mtype;
    uint32_t size;
    int32_t next;
    uint8_t data[DDMSG_MAXSZ];
};

struct ddmsg_store {
    atomic_uint magic;
    int32_t head, tail, freehead;
    struct ddmsg_slot slots[DDMSG_SLOTS];
};

// ---- in-process (COW-inherited across fork) state ------------------------------------------------
static struct ddipc_ctrl *g_ctrl; // this process's mapping of the control block
static uint32_t g_ns_hash;        // namespace id (0 == not yet computed)
static int g_ipc_creator;         // did THIS process create the control block?
static int g_ipc_atexit_armed;
static int g_ipc_ctor_pid;                                        // engine-root pid (constructor; COW-inherited)
static pthread_mutex_t g_ipc_local_m = PTHREAD_MUTEX_INITIALIZER; // guards the in-process caches below

#define DD_SHMAT_MAX 256

static struct {
    int used;
    void *addr;
    uint32_t idx;
    size_t len;
} g_shmat[DD_SHMAT_MAX];

#define DD_MSGCACHE_MAX 256

static struct {
    int used;
    uint32_t idx;
    uint32_t seq;
    struct ddmsg_store *p;
} g_msgcache[DD_MSGCACHE_MAX];

#define DD_UNDO_MAX 256

static struct {
    int used;
    uint32_t idx; // sem set slot
    uint32_t seq; // set's seq (guard against slot reuse)
    uint16_t semnum;
    int adj; // accumulated undo adjustment (subtract on process exit)
} g_undo[DD_UNDO_MAX];

__attribute__((constructor)) static void ipc_ctor(void) {
    g_ipc_ctor_pid = (int)getpid();
}

static int64_t dd_now(void) {
    return (int64_t)time(NULL);
}

static size_t dd_pground(size_t n) {
    size_t pg = (size_t)sysconf(_SC_PAGESIZE);
    if (pg == 0) pg = 16384;
    return (n + pg - 1) & ~(pg - 1);
}

// ---- namespace + object names --------------------------------------------------------------------
// Key the namespace by DD_NETNS (per-IPC-ns isolation; --network host leaves it unset -> shared). When
// unset we fall back to the container init pid (daemon path) or the engine-root pid (single-binary/test
// path, captured by the constructor and COW-inherited by every child) -- unique per run, shared by the
// whole process tree, so a leak is per-run and cross-run runs never collide.
static uint32_t ipc_ns(void) {
    if (g_ns_hash) return g_ns_hash;
    char buf[80];
    const char *ns = getenv("DD_NETNS");
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

static void dd_ctrl_name(char *out, size_t n) {
    snprintf(out, n, "/di%08xC", ipc_ns());
}

static void dd_shm_name(char *out, size_t n, uint32_t idx) {
    snprintf(out, n, "/di%08xs%x", ipc_ns(), idx);
}

static void dd_msg_name(char *out, size_t n, uint32_t idx) {
    snprintf(out, n, "/di%08xm%x", ipc_ns(), idx);
}

// ---- robust spinlock -----------------------------------------------------------------------------
static void dd_lock(struct ddlock *L) {
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

static void dd_unlock(struct ddlock *L) {
    atomic_store(&L->owner, 0);
}

// ---- control block attach ------------------------------------------------------------------------
static void sysv_on_exit(void);

static struct ddipc_ctrl *dd_ctrl(void) {
    if (g_ctrl) return g_ctrl;
    char nm[40];
    dd_ctrl_name(nm, sizeof nm);
    int created = 0, fd = shm_open(nm, O_CREAT | O_EXCL | O_RDWR, 0600);
    if (fd >= 0) {
        created = 1;
        if (ftruncate(fd, (off_t)sizeof(struct ddipc_ctrl)) < 0) {
            close(fd);
            shm_unlink(nm);
            return NULL;
        }
    } else if (errno == EEXIST) {
        fd = shm_open(nm, O_RDWR, 0600);
    }
    if (fd < 0) return NULL;
    void *p = mmap(NULL, sizeof(struct ddipc_ctrl), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (p == MAP_FAILED) return NULL;
    struct ddipc_ctrl *c = (struct ddipc_ctrl *)p;
    if (created) {
        // A fresh POSIX shm object is zero-filled, which is our valid empty state; publish magic last.
        atomic_store(&c->magic, DDIPC_CTRL_MAGIC);
        g_ipc_creator = 1;
    } else {
        for (int i = 0; i < 200000 && atomic_load(&c->magic) != DDIPC_CTRL_MAGIC; i++) {
            struct timespec ts = {0, 20000};
            nanosleep(&ts, NULL);
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
static struct ddmsg_store *dd_msg_store(uint32_t idx, uint32_t seq, int create) {
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < DD_MSGCACHE_MAX; i++)
        if (g_msgcache[i].used && g_msgcache[i].idx == idx && g_msgcache[i].seq == seq) {
            struct ddmsg_store *r = g_msgcache[i].p;
            pthread_mutex_unlock(&g_ipc_local_m);
            return r;
        }
    pthread_mutex_unlock(&g_ipc_local_m);

    char nm[40];
    dd_msg_name(nm, sizeof nm, idx);
    int fd;
    if (create) {
        shm_unlink(nm); // clear any stale object at this (ns,idx) before (re)creating
        fd = shm_open(nm, O_CREAT | O_EXCL | O_RDWR, 0600);
        if (fd < 0)
            fd = shm_open(nm, O_RDWR, 0600); // lost a create race -> open the winner's
        else if (ftruncate(fd, (off_t)sizeof(struct ddmsg_store)) < 0) {
            close(fd);
            shm_unlink(nm);
            return NULL;
        }
    } else {
        fd = shm_open(nm, O_RDWR, 0600);
    }
    if (fd < 0) return NULL;
    void *p = mmap(NULL, sizeof(struct ddmsg_store), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (p == MAP_FAILED) return NULL;
    struct ddmsg_store *s = (struct ddmsg_store *)p;
    if (create && atomic_load(&s->magic) != DDMSG_MAGIC) {
        s->head = s->tail = -1;
        for (int i = 0; i < DDMSG_SLOTS; i++)
            s->slots[i].next = (i + 1 < DDMSG_SLOTS) ? i + 1 : -1;
        s->freehead = 0;
        atomic_store(&s->magic, DDMSG_MAGIC);
    } else {
        for (int i = 0; i < 200000 && atomic_load(&s->magic) != DDMSG_MAGIC; i++) {
            struct timespec ts = {0, 20000};
            nanosleep(&ts, NULL);
        }
    }
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < DD_MSGCACHE_MAX; i++)
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

static void dd_msg_uncache(uint32_t idx) {
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < DD_MSGCACHE_MAX; i++)
        if (g_msgcache[i].used && g_msgcache[i].idx == idx) {
            munmap(g_msgcache[i].p, sizeof(struct ddmsg_store));
            g_msgcache[i].used = 0;
        }
    pthread_mutex_unlock(&g_ipc_local_m);
}

// ---- permission checks (against the stored container identity) -----------------------------------
static int dd_access(const struct ddperm *p, int want) {
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

static int dd_owner(const struct ddperm *p) {
    cred_init();
    if (cred_euid() == 0) return 0;
    return ((uint32_t)cred_euid() == p->uid || (uint32_t)cred_euid() == p->cuid) ? 0 : -EPERM;
}

static void ddperm_to_guest(struct ipc64_perm_guest *g, const struct ddperm *p) {
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

static void ddperm_init(struct ddperm *p, int32_t key, int flag) {
    p->key = key;
    p->uid = p->cuid = (uint32_t)cuid();
    p->gid = p->cgid = (uint32_t)cgid();
    p->mode = (uint32_t)(flag & 0777);
    // seq is preserved across free/realloc by the caller.
}

// ---- id build / decode ---------------------------------------------------------------------------
static uint64_t dd_id(int mni, uint32_t idx, uint32_t seq) {
    return (uint64_t)seq * (uint32_t)mni + idx;
}

// ============================================================================================
//  SHARED MEMORY
// ============================================================================================
static struct ddshm *shm_by_id(struct ddipc_ctrl *C, int id) {
    if (id < 0) return NULL;
    uint32_t idx = (uint32_t)id % DDIPC_SHMMNI, seq = (uint32_t)id / DDIPC_SHMMNI;
    struct ddshm *s = &C->shm[idx];
    if (!s->inuse || s->removed || s->perm.seq != seq) return NULL;
    return s;
}

static uint32_t shm_idx_of(struct ddipc_ctrl *C, const struct ddshm *s) {
    return (uint32_t)(s - C->shm);
}

static void shm_free(struct ddipc_ctrl *C, uint32_t idx) {
    char nm[40];
    dd_shm_name(nm, sizeof nm, idx);
    shm_unlink(nm);
    uint32_t seq = C->shm[idx].perm.seq + 1;
    memset(&C->shm[idx], 0, sizeof C->shm[idx]);
    C->shm[idx].perm.seq = seq;
}

// Marshal descriptor idx -> the guest shmid64_ds at gbuf (already access-checked). Returns 0 or -errno.
static uint64_t shm_stat_to_guest(struct ddipc_ctrl *C, uint32_t idx, uint64_t gbuf) {
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct shmid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct ddshm *s = &C->shm[idx];
    struct shmid64_ds_guest *g = (struct shmid64_ds_guest *)gbuf;
    memset(g, 0, sizeof *g);
    ddperm_to_guest(&g->shm_perm, &s->perm);
    g->shm_segsz = s->segsz;
    g->shm_atime = s->atime;
    g->shm_dtime = s->dtime;
    g->shm_ctime = s->ctime;
    g->shm_cpid = s->cpid;
    g->shm_lpid = s->lpid;
    g->shm_nattch = s->nattch;
    return 0;
}

// ============================================================================================
//  SEMAPHORES
// ============================================================================================
static struct ddsem *sem_by_id(struct ddipc_ctrl *C, int id) {
    if (id < 0) return NULL;
    uint32_t idx = (uint32_t)id % DDIPC_SEMMNI, seq = (uint32_t)id / DDIPC_SEMMNI;
    struct ddsem *s = &C->sem[idx];
    if (!s->inuse || s->perm.seq != seq) return NULL;
    return s;
}

static uint32_t sem_idx_of(struct ddipc_ctrl *C, const struct ddsem *s) {
    return (uint32_t)(s - C->sem);
}

static void sem_free(struct ddipc_ctrl *C, uint32_t idx) {
    uint32_t seq = C->sem[idx].perm.seq + 1;
    memset(&C->sem[idx], 0, sizeof C->sem[idx]);
    C->sem[idx].perm.seq = seq;
}

static uint64_t sem_stat_to_guest(struct ddipc_ctrl *C, uint32_t idx, uint64_t gbuf) {
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct semid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct ddsem *s = &C->sem[idx];
    struct semid64_ds_guest *g = (struct semid64_ds_guest *)gbuf;
    memset(g, 0, sizeof *g);
    ddperm_to_guest(&g->sem_perm, &s->perm);
    g->sem_otime = s->otime;
    g->sem_ctime = s->ctime;
    g->sem_nsems = s->nsems;
    return 0;
}

// Drop this process's undo record for (idx,semnum) -- SETVAL/SETALL clear the semadj (Linux semantics).
static void sem_undo_clear(uint32_t idx, uint32_t seq, int semnum /* -1 == whole set */) {
    for (int i = 0; i < DD_UNDO_MAX; i++)
        if (g_undo[i].used && g_undo[i].idx == idx && g_undo[i].seq == seq &&
            (semnum < 0 || g_undo[i].semnum == (uint16_t)semnum))
            g_undo[i].used = 0;
}

static void sem_undo_add(uint32_t idx, uint32_t seq, uint16_t semnum, int adj) {
    if (adj == 0) return;
    for (int i = 0; i < DD_UNDO_MAX; i++)
        if (g_undo[i].used && g_undo[i].idx == idx && g_undo[i].seq == seq && g_undo[i].semnum == semnum) {
            g_undo[i].adj += adj;
            return;
        }
    for (int i = 0; i < DD_UNDO_MAX; i++)
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
static struct ddmsgq *msg_by_id(struct ddipc_ctrl *C, int id) {
    if (id < 0) return NULL;
    uint32_t idx = (uint32_t)id % DDIPC_MSGMNI, seq = (uint32_t)id / DDIPC_MSGMNI;
    struct ddmsgq *q = &C->msg[idx];
    if (!q->inuse || q->removed || q->perm.seq != seq) return NULL;
    return q;
}

static uint32_t msg_idx_of(struct ddipc_ctrl *C, const struct ddmsgq *q) {
    return (uint32_t)(q - C->msg);
}

static void msg_free(struct ddipc_ctrl *C, uint32_t idx) {
    dd_msg_uncache(idx);
    char nm[40];
    dd_msg_name(nm, sizeof nm, idx);
    shm_unlink(nm);
    uint32_t seq = C->msg[idx].perm.seq + 1;
    memset(&C->msg[idx], 0, sizeof C->msg[idx]);
    C->msg[idx].perm.seq = seq;
}

static uint64_t msg_stat_to_guest(struct ddipc_ctrl *C, uint32_t idx, uint64_t gbuf) {
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct msqid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct ddmsgq *q = &C->msg[idx];
    struct msqid64_ds_guest *g = (struct msqid64_ds_guest *)gbuf;
    memset(g, 0, sizeof *g);
    ddperm_to_guest(&g->msg_perm, &q->perm);
    g->msg_stime = q->stime;
    g->msg_rtime = q->rtime;
    g->msg_ctime = q->ctime;
    g->msg_cbytes = q->cbytes;
    g->msg_qnum = q->qnum;
    g->msg_qbytes = q->qbytes;
    g->msg_lspid = q->lspid;
    g->msg_lrpid = q->lrpid;
    return 0;
}

// ---- IPC_INFO / *_INFO fill (Linux-like limits + live counts) ------------------------------------
static int shm_count(struct ddipc_ctrl *C, int *maxid) {
    int n = 0, m = -1;
    for (int i = 0; i < DDIPC_SHMMNI; i++)
        if (C->shm[i].inuse) {
            n++;
            m = i;
        }
    if (maxid) *maxid = m;
    return n;
}

static int sem_count(struct ddipc_ctrl *C, int *maxid) {
    int n = 0, m = -1;
    for (int i = 0; i < DDIPC_SEMMNI; i++)
        if (C->sem[i].inuse) {
            n++;
            m = i;
        }
    if (maxid) *maxid = m;
    return n;
}

static int msg_count(struct ddipc_ctrl *C, int *maxid) {
    int n = 0, m = -1;
    for (int i = 0; i < DDIPC_MSGMNI; i++)
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
// dead holder is recovered by dd_lock's steal). Called from proc.c after fork.
static void sysv_on_exit(void);

static void sysv_after_fork(void) {
    pthread_mutex_init(&g_ipc_local_m, NULL);
    g_ipc_creator = 0;                // only the parent owns the atexit GC
    memset(g_undo, 0, sizeof g_undo); // semadj is not inherited across fork
    g_ipc_did_exit = 0;               // the child gets its own exit pass
    if (g_ctrl) {                     // inherited attachments bump nattch (Linux VM_SHM fork)
        dd_lock(&g_ctrl->lock);
        for (int i = 0; i < DD_SHMAT_MAX; i++)
            if (g_shmat[i].used) {
                struct ddshm *s = &g_ctrl->shm[g_shmat[i].idx];
                if (s->inuse) s->nattch++;
            }
        dd_unlock(&g_ctrl->lock);
        if (!g_ipc_atexit_armed) {  // the child short-circuits dd_ctrl() (g_ctrl inherited), so
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
    struct ddipc_ctrl *C = g_ctrl;
    if (C) {
        dd_lock(&C->lock);
        for (int i = 0; i < DD_SHMAT_MAX; i++)
            if (g_shmat[i].used) {
                struct ddshm *s = &C->shm[g_shmat[i].idx];
                munmap(g_shmat[i].addr, g_shmat[i].len);
                if (s->inuse) {
                    if (s->nattch) s->nattch--;
                    if (s->removed && s->nattch == 0) shm_free(C, g_shmat[i].idx);
                }
                g_shmat[i].used = 0;
            }
        dd_unlock(&C->lock);
    }
    memset(g_undo, 0, sizeof g_undo); // semadj is cleared across execve
}

// Apply this process's outstanding SEM_UNDO adjustments (process exit undoes them, Linux semantics) and, if
// we created the namespace (or we are the container init), unlink every live object + the control block.
static void sysv_on_exit(void) {
    if (g_ipc_did_exit) return;
    g_ipc_did_exit = 1;
    struct ddipc_ctrl *C = g_ctrl;
    if (!C) return;
    dd_lock(&C->lock);
    // Process exit detaches every segment this process still holds (Linux: shm_nattch drops, and a segment
    // already marked for deletion is destroyed once nattch hits 0).
    for (int i = 0; i < DD_SHMAT_MAX; i++)
        if (g_shmat[i].used) {
            struct ddshm *s = &C->shm[g_shmat[i].idx];
            if (s->inuse) {
                if (s->nattch) s->nattch--;
                if (s->removed && s->nattch == 0) shm_free(C, g_shmat[i].idx);
            }
            g_shmat[i].used = 0;
        }
    for (int i = 0; i < DD_UNDO_MAX; i++)
        if (g_undo[i].used) {
            uint32_t idx = g_undo[i].idx;
            if (idx < DDIPC_SEMMNI && C->sem[idx].inuse && C->sem[idx].perm.seq == g_undo[i].seq &&
                g_undo[i].semnum < C->sem[idx].nsems) {
                int v = (int)C->sem[idx].val[g_undo[i].semnum] - g_undo[i].adj;
                if (v < 0) v = 0;
                if (v > DDIPC_SEMVMX) v = DDIPC_SEMVMX;
                C->sem[idx].val[g_undo[i].semnum] = (uint16_t)v;
            }
            g_undo[i].used = 0;
        }
    int gc = g_ipc_creator || (g_init_hostpid && (int)getpid() == g_init_hostpid);
    if (gc) {
        for (int i = 0; i < DDIPC_SHMMNI; i++)
            if (C->shm[i].inuse) {
                char nm[40];
                dd_shm_name(nm, sizeof nm, (uint32_t)i);
                shm_unlink(nm);
            }
        for (int i = 0; i < DDIPC_MSGMNI; i++)
            if (C->msg[i].inuse) {
                char nm[40];
                dd_msg_name(nm, sizeof nm, (uint32_t)i);
                shm_unlink(nm);
            }
    }
    dd_unlock(&C->lock);
    if (gc) {
        char nm[40];
        dd_ctrl_name(nm, sizeof nm);
        shm_unlink(nm);
    }
}

static int svc_sysv(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    (void)a5;
    struct ddipc_ctrl *C;
    switch (nr) {
    // ===================== SysV shared memory =====================
    case 194: { // shmget(key, size, shmflg)
        int32_t key = (int32_t)a0;
        size_t size = (size_t)a1;
        int flag = (int)a2;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        dd_lock(&C->lock);
        struct ddshm *found = NULL;
        if (key != L_IPC_PRIVATE)
            for (int i = 0; i < DDIPC_SHMMNI; i++)
                if (C->shm[i].inuse && !C->shm[i].removed && C->shm[i].perm.key == key) {
                    found = &C->shm[i];
                    break;
                }
        if (found) {
            if ((flag & L_IPC_CREAT) && (flag & L_IPC_EXCL)) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            }
            if (size && found->segsz < size) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            int perr = dd_access(&found->perm, 4);
            if (perr) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            uint64_t id = dd_id(DDIPC_SHMMNI, shm_idx_of(C, found), found->perm.seq);
            dd_unlock(&C->lock);
            G_RET(c) = id;
            break;
        }
        if (key != L_IPC_PRIVATE && !(flag & L_IPC_CREAT)) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOENT);
            break;
        }
        if (size == 0 || size > DDIPC_SHMMAX) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int idx = -1;
        for (int i = 0; i < DDIPC_SHMMNI; i++)
            if (!C->shm[i].inuse) {
                idx = i;
                break;
            }
        if (idx < 0) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        char nm[40];
        dd_shm_name(nm, sizeof nm, (uint32_t)idx);
        shm_unlink(nm);
        int fd = shm_open(nm, O_CREAT | O_EXCL | O_RDWR, 0600);
        if (fd < 0) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        if (ftruncate(fd, (off_t)dd_pground(size)) < 0) {
            close(fd);
            shm_unlink(nm);
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        close(fd);
        struct ddshm *s = &C->shm[idx];
        uint32_t seq = s->perm.seq;
        memset(s, 0, sizeof *s);
        s->perm.seq = seq;
        ddperm_init(&s->perm, key, flag);
        s->segsz = size;
        s->cpid = container_pid();
        s->ctime = dd_now();
        s->inuse = 1;
        uint64_t id = dd_id(DDIPC_SHMMNI, (uint32_t)idx, seq);
        dd_unlock(&C->lock);
        G_RET(c) = id;
        break;
    }
    case 196: { // shmat(shmid, shmaddr, shmflg)
        int id = (int)a0, flag = (int)a2;
        void *shmaddr = (void *)a1;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        dd_lock(&C->lock);
        struct ddshm *s = shm_by_id(C, id);
        if (!s) { // a removed-but-attached id resolves nowhere new; report EIDRM if it exists-but-removed
            uint32_t idx = (uint32_t)id % DDIPC_SHMMNI;
            int eid = (id >= 0 && C->shm[idx].inuse && C->shm[idx].removed &&
                       C->shm[idx].perm.seq == (uint32_t)id / DDIPC_SHMMNI);
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(eid ? -EIDRM : -EINVAL);
            break;
        }
        int want = (flag & L_SHM_RDONLY) ? 4 : 6;
        int perr = dd_access(&s->perm, want);
        if (perr) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)perr;
            break;
        }
        uint32_t idx = shm_idx_of(C, s);
        size_t len = dd_pground(s->segsz);
        char nm[40];
        dd_shm_name(nm, sizeof nm, idx);
        dd_unlock(&C->lock); // shm_open/mmap can be slow -- don't hold the lock across them
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
            size_t pg = (size_t)sysconf(_SC_PAGESIZE);
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
        for (int i = 0; i < DD_SHMAT_MAX; i++)
            if (!g_shmat[i].used) {
                g_shmat[i].used = 1;
                g_shmat[i].addr = p;
                g_shmat[i].idx = idx;
                g_shmat[i].len = len;
                break;
            }
        pthread_mutex_unlock(&g_ipc_local_m);
        dd_lock(&C->lock);
        s = shm_by_id(C, id);
        if (s) {
            s->nattch++;
            s->lpid = container_pid();
            s->atime = dd_now();
        }
        dd_unlock(&C->lock);
        G_RET(c) = (uint64_t)p;
        break;
    }
    case 197: { // shmdt(shmaddr)
        void *addr = (void *)a0;
        C = dd_ctrl();
        pthread_mutex_lock(&g_ipc_local_m);
        int slot = -1;
        for (int i = 0; i < DD_SHMAT_MAX; i++)
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
            dd_lock(&C->lock);
            struct ddshm *s = &C->shm[idx];
            if (s->inuse) {
                if (s->nattch) s->nattch--;
                s->lpid = container_pid();
                s->dtime = dd_now();
                if (s->removed && s->nattch == 0) shm_free(C, idx);
            }
            dd_unlock(&C->lock);
        }
        G_RET(c) = 0;
        break;
    }
    case 195: { // shmctl(shmid, cmd, buf)
        int id = (int)a0, cmd = (int)a1;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (cmd == L_IPC_INFO || cmd == L_SHM_INFO) {
            dd_lock(&C->lock);
            int maxid = -1, n = shm_count(C, &maxid);
            uint64_t rc = 0;
            if (cmd == L_IPC_INFO) {
                if (!host_range_mapped((uintptr_t)a2, sizeof(struct shminfo_guest)))
                    rc = (uint64_t)(-EFAULT);
                else {
                    struct shminfo_guest *g = (struct shminfo_guest *)a2;
                    memset(g, 0, sizeof *g);
                    g->shmmax = DDIPC_SHMMAX;
                    g->shmmin = 1;
                    g->shmmni = DDIPC_SHMMNI_ADV;
                    g->shmseg = DDIPC_SHMMNI_ADV;
                    g->shmall = DDIPC_SHMMAX / 4096;
                }
            } else {
                if (!host_range_mapped((uintptr_t)a2, sizeof(struct shm_info_guest)))
                    rc = (uint64_t)(-EFAULT);
                else {
                    struct shm_info_guest *g = (struct shm_info_guest *)a2;
                    memset(g, 0, sizeof *g);
                    g->used_ids = n;
                }
            }
            dd_unlock(&C->lock);
            G_RET(c) = rc ? rc : (uint64_t)(maxid < 0 ? 0 : maxid);
            break;
        }
        if (cmd == L_SHM_STAT || cmd == L_SHM_STAT_ANY) {
            dd_lock(&C->lock);
            if (id < 0 || id >= DDIPC_SHMMNI || !C->shm[id].inuse) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (cmd == L_SHM_STAT) {
                int perr = dd_access(&C->shm[id].perm, 4);
                if (perr) {
                    dd_unlock(&C->lock);
                    G_RET(c) = (uint64_t)perr;
                    break;
                }
            }
            uint64_t retid = dd_id(DDIPC_SHMMNI, (uint32_t)id, C->shm[id].perm.seq);
            uint64_t rc = shm_stat_to_guest(C, (uint32_t)id, a2);
            dd_unlock(&C->lock);
            G_RET(c) = rc ? rc : retid;
            break;
        }
        dd_lock(&C->lock);
        struct ddshm *s = shm_by_id(C, id);
        if (!s) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t idx = shm_idx_of(C, s);
        uint64_t rc;
        switch (cmd) {
        case L_IPC_STAT: {
            int perr = dd_access(&s->perm, 4);
            rc = perr ? (uint64_t)perr : shm_stat_to_guest(C, idx, a2);
            break;
        }
        case L_IPC_SET: {
            int perr = dd_owner(&s->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            if (!host_range_mapped((uintptr_t)a2, sizeof(struct shmid64_ds_guest))) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            struct shmid64_ds_guest *g = (struct shmid64_ds_guest *)a2;
            s->perm.uid = g->shm_perm.uid;
            s->perm.gid = g->shm_perm.gid;
            s->perm.mode = (s->perm.mode & ~0777u) | (g->shm_perm.mode & 0777);
            s->ctime = dd_now();
            rc = 0;
            break;
        }
        case L_IPC_RMID: {
            int perr = dd_owner(&s->perm);
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
            rc = (uint64_t)dd_owner(&s->perm);
            break;
        default: rc = (uint64_t)(-EINVAL); break;
        }
        dd_unlock(&C->lock);
        G_RET(c) = rc;
        break;
    }

    // ===================== SysV semaphores =====================
    case 190: { // semget(key, nsems, semflg)
        int32_t key = (int32_t)a0;
        int nsems = (int)a1, flag = (int)a2;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        dd_lock(&C->lock);
        struct ddsem *found = NULL;
        if (key != L_IPC_PRIVATE)
            for (int i = 0; i < DDIPC_SEMMNI; i++)
                if (C->sem[i].inuse && C->sem[i].perm.key == key) {
                    found = &C->sem[i];
                    break;
                }
        if (found) {
            if ((flag & L_IPC_CREAT) && (flag & L_IPC_EXCL)) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            }
            if (nsems > 0 && (uint32_t)nsems > found->nsems) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            int perr = dd_access(&found->perm, 4);
            if (perr) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            uint64_t id = dd_id(DDIPC_SEMMNI, sem_idx_of(C, found), found->perm.seq);
            dd_unlock(&C->lock);
            G_RET(c) = id;
            break;
        }
        if (key != L_IPC_PRIVATE && !(flag & L_IPC_CREAT)) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOENT);
            break;
        }
        if (nsems <= 0 || nsems > DDIPC_SEMMSL) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int idx = -1;
        for (int i = 0; i < DDIPC_SEMMNI; i++)
            if (!C->sem[i].inuse) {
                idx = i;
                break;
            }
        if (idx < 0) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        struct ddsem *s = &C->sem[idx];
        uint32_t seq = s->perm.seq;
        memset(s, 0, sizeof *s);
        s->perm.seq = seq;
        ddperm_init(&s->perm, key, flag);
        s->nsems = (uint32_t)nsems;
        s->ctime = dd_now();
        s->inuse = 1;
        uint64_t id = dd_id(DDIPC_SEMMNI, (uint32_t)idx, seq);
        dd_unlock(&C->lock);
        G_RET(c) = id;
        break;
    }
    case 192:   // semtimedop(semid, sops, nsops, timeout)
    case 193: { // semop(semid, sops, nsops)
        int id = (int)a0;
        size_t nsops = (size_t)a2;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (nsops == 0 || nsops > DDIPC_SEMOPM_ADV) {
            G_RET(c) = (uint64_t)(nsops == 0 ? -EINVAL : -E2BIG);
            break;
        }
        if (!host_range_mapped((uintptr_t)a1, nsops * sizeof(struct sembuf_guest))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        struct sembuf_guest *sops = (struct sembuf_guest *)a1;
        // Optional relative timeout (semtimedop): compute an absolute monotonic deadline.
        struct timespec deadline;
        int have_deadline = 0;
        if (nr == 192 && a3) {
            if (!host_range_mapped((uintptr_t)a3, sizeof(struct timespec))) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            struct timespec *to = (struct timespec *)a3;
            if (to->tv_nsec < 0 || to->tv_nsec >= 1000000000L || to->tv_sec < 0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            clock_gettime(CLOCK_MONOTONIC, &deadline);
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
            dd_lock(&C->lock);
            struct ddsem *s = sem_by_id(C, id);
            if (!s) {
                if (waited_marked) waited_marked = 0; // set gone while blocking -> EIDRM
                dd_unlock(&C->lock);
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
                dd_unlock(&C->lock);
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
                } else if (cur + op > DDIPC_SEMVMX) {
                    dd_unlock(&C->lock);
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
                s->otime = dd_now();
                dd_unlock(&C->lock);
                G_RET(c) = 0;
                break;
            }
            // Cannot proceed: NOWAIT -> EAGAIN; else register as a waiter and poll.
            if (sops[block_on].sem_flg & L_IPC_NOWAIT) {
                dd_unlock(&C->lock);
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
            dd_unlock(&C->lock);
            did_wait = 1;
            if (have_deadline) {
                struct timespec now;
                clock_gettime(CLOCK_MONOTONIC, &now);
                if (now.tv_sec > deadline.tv_sec ||
                    (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)) {
                    dd_lock(&C->lock);
                    struct ddsem *s2 = sem_by_id(C, id);
                    if (s2)
                        for (size_t i = 0; i < nsops; i++) {
                            if (sops[i].sem_op < 0 && s2->ncnt[sops[i].sem_num] > 0)
                                s2->ncnt[sops[i].sem_num]--;
                            else if (sops[i].sem_op == 0 && s2->zcnt[sops[i].sem_num] > 0)
                                s2->zcnt[sops[i].sem_num]--;
                        }
                    dd_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
            }
            struct timespec ts = {0, 200000}; // 200us poll
            if (nanosleep(&ts, NULL) < 0 && errno == EINTR) {
                dd_lock(&C->lock);
                struct ddsem *s2 = sem_by_id(C, id);
                if (s2)
                    for (size_t i = 0; i < nsops; i++) {
                        if (sops[i].sem_op < 0 && s2->ncnt[sops[i].sem_num] > 0)
                            s2->ncnt[sops[i].sem_num]--;
                        else if (sops[i].sem_op == 0 && s2->zcnt[sops[i].sem_num] > 0)
                            s2->zcnt[sops[i].sem_num]--;
                    }
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINTR);
                break;
            }
        }
    sem_done:
        break;
    }
    case 191: { // semctl(semid, semnum, cmd, arg)
        int id = (int)a0, semnum = (int)a1, cmd = (int)a2;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (cmd == L_IPC_INFO || cmd == L_SEM_INFO) {
            dd_lock(&C->lock);
            int maxid = -1, n = sem_count(C, &maxid);
            uint64_t rc = 0;
            if (!host_range_mapped((uintptr_t)a3, sizeof(struct seminfo_guest)))
                rc = (uint64_t)(-EFAULT);
            else {
                struct seminfo_guest *g = (struct seminfo_guest *)a3;
                memset(g, 0, sizeof *g);
                g->semmni = DDIPC_SEMMNI_ADV;
                g->semmsl = DDIPC_SEMMSL_ADV;
                g->semmns = DDIPC_SEMMNS_ADV;
                g->semopm = DDIPC_SEMOPM_ADV;
                g->semvmx = DDIPC_SEMVMX;
                g->semaem = DDIPC_SEMVMX;
                g->semmnu = 2147483647;
                g->semume = DDIPC_SEMOPM_ADV;
                if (cmd == L_SEM_INFO) {
                    g->semusz = n;
                    g->semaem = n;
                }
            }
            dd_unlock(&C->lock);
            G_RET(c) = rc ? rc : (uint64_t)(maxid < 0 ? 0 : maxid);
            break;
        }
        if (cmd == L_SEM_STAT || cmd == L_SEM_STAT_ANY) {
            dd_lock(&C->lock);
            if (id < 0 || id >= DDIPC_SEMMNI || !C->sem[id].inuse) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (cmd == L_SEM_STAT) {
                int perr = dd_access(&C->sem[id].perm, 4);
                if (perr) {
                    dd_unlock(&C->lock);
                    G_RET(c) = (uint64_t)perr;
                    break;
                }
            }
            uint64_t retid = dd_id(DDIPC_SEMMNI, (uint32_t)id, C->sem[id].perm.seq);
            uint64_t rc = sem_stat_to_guest(C, (uint32_t)id, a3);
            dd_unlock(&C->lock);
            G_RET(c) = rc ? rc : retid;
            break;
        }
        dd_lock(&C->lock);
        struct ddsem *s = sem_by_id(C, id);
        if (!s) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t idx = sem_idx_of(C, s);
        uint64_t rc;
        switch (cmd) {
        case L_IPC_STAT: {
            int perr = dd_access(&s->perm, 4);
            rc = perr ? (uint64_t)perr : sem_stat_to_guest(C, idx, a3);
            break;
        }
        case L_IPC_SET: {
            int perr = dd_owner(&s->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            if (!host_range_mapped((uintptr_t)a3, sizeof(struct semid64_ds_guest))) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            struct semid64_ds_guest *g = (struct semid64_ds_guest *)a3;
            s->perm.uid = g->sem_perm.uid;
            s->perm.gid = g->sem_perm.gid;
            s->perm.mode = (s->perm.mode & ~0777u) | (g->sem_perm.mode & 0777);
            s->ctime = dd_now();
            rc = 0;
            break;
        }
        case L_IPC_RMID: {
            int perr = dd_owner(&s->perm);
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
            int perr = dd_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)s->val[semnum];
            break;
        }
        case L_GETPID: {
            int perr = dd_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)(uint32_t)s->pid[semnum];
            break;
        }
        case L_GETNCNT: {
            int perr = dd_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)(uint32_t)s->ncnt[semnum];
            break;
        }
        case L_GETZCNT: {
            int perr = dd_access(&s->perm, 4);
            if (perr)
                rc = (uint64_t)perr;
            else if (semnum < 0 || (uint32_t)semnum >= s->nsems)
                rc = (uint64_t)(-EINVAL);
            else
                rc = (uint64_t)(uint32_t)s->zcnt[semnum];
            break;
        }
        case L_SETVAL: {
            int perr = dd_access(&s->perm, 2);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            if (semnum < 0 || (uint32_t)semnum >= s->nsems) {
                rc = (uint64_t)(-EINVAL);
                break;
            }
            int v = (int)a3;
            if (v < 0 || v > DDIPC_SEMVMX) {
                rc = (uint64_t)(-ERANGE);
                break;
            }
            s->val[semnum] = (uint16_t)v;
            s->pid[semnum] = container_pid();
            s->ctime = dd_now();
            sem_undo_clear(idx, s->perm.seq, semnum);
            rc = 0;
            break;
        }
        case L_GETALL: {
            int perr = dd_access(&s->perm, 4);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            if (!host_range_mapped((uintptr_t)a3, s->nsems * sizeof(uint16_t))) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            uint16_t *arr = (uint16_t *)a3;
            for (uint32_t i = 0; i < s->nsems; i++)
                arr[i] = s->val[i];
            rc = 0;
            break;
        }
        case L_SETALL: {
            int perr = dd_access(&s->perm, 2);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            if (!host_range_mapped((uintptr_t)a3, s->nsems * sizeof(uint16_t))) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            uint16_t *arr = (uint16_t *)a3;
            for (uint32_t i = 0; i < s->nsems; i++) {
                if (arr[i] > DDIPC_SEMVMX) {
                    rc = (uint64_t)(-ERANGE);
                    goto sem_setall_out;
                }
            }
            for (uint32_t i = 0; i < s->nsems; i++)
                s->val[i] = arr[i];
            s->ctime = dd_now();
            sem_undo_clear(idx, s->perm.seq, -1);
            rc = 0;
        sem_setall_out:
            break;
        }
        default: rc = (uint64_t)(-EINVAL); break;
        }
        dd_unlock(&C->lock);
        G_RET(c) = rc;
        break;
    }

    // ===================== SysV message queues =====================
    case 186: { // msgget(key, msgflg)
        int32_t key = (int32_t)a0;
        int flag = (int)a1;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        dd_lock(&C->lock);
        struct ddmsgq *found = NULL;
        if (key != L_IPC_PRIVATE)
            for (int i = 0; i < DDIPC_MSGMNI; i++)
                if (C->msg[i].inuse && !C->msg[i].removed && C->msg[i].perm.key == key) {
                    found = &C->msg[i];
                    break;
                }
        if (found) {
            if ((flag & L_IPC_CREAT) && (flag & L_IPC_EXCL)) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            }
            int perr = dd_access(&found->perm, 4);
            if (perr) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            uint64_t id = dd_id(DDIPC_MSGMNI, msg_idx_of(C, found), found->perm.seq);
            dd_unlock(&C->lock);
            G_RET(c) = id;
            break;
        }
        if (key != L_IPC_PRIVATE && !(flag & L_IPC_CREAT)) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOENT);
            break;
        }
        int idx = -1;
        for (int i = 0; i < DDIPC_MSGMNI; i++)
            if (!C->msg[i].inuse) {
                idx = i;
                break;
            }
        if (idx < 0) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        struct ddmsgq *q = &C->msg[idx];
        uint32_t seq = q->perm.seq;
        memset(q, 0, sizeof *q);
        q->perm.seq = seq;
        ddperm_init(&q->perm, key, flag);
        q->qbytes = DDIPC_MSGMNB;
        q->ctime = dd_now();
        q->inuse = 1;
        dd_unlock(&C->lock);
        // Create the backing store OUTSIDE the lock (shm_open/ftruncate can be slow).
        if (!dd_msg_store((uint32_t)idx, seq, 1)) {
            dd_lock(&C->lock);
            if (q->inuse && q->perm.seq == seq) msg_free(C, (uint32_t)idx);
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-ENOSPC);
            break;
        }
        G_RET(c) = dd_id(DDIPC_MSGMNI, (uint32_t)idx, seq);
        break;
    }
    case 189: { // msgsnd(msqid, msgp, msgsz, msgflg)
        int id = (int)a0;
        size_t msgsz = (size_t)a2;
        int flag = (int)a3;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (msgsz > DDMSG_MAXSZ) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (!host_range_mapped((uintptr_t)a1, sizeof(long) + msgsz)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        long mtype = *(long *)a1;
        if (mtype < 1) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        const uint8_t *body = (const uint8_t *)(a1 + sizeof(long));
        int did_wait = 0;
        for (;;) {
            dd_lock(&C->lock);
            struct ddmsgq *q = msg_by_id(C, id);
            if (!q) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(did_wait ? -EIDRM : -EINVAL);
                break;
            }
            uint32_t idx = msg_idx_of(C, q);
            uint32_t qseq = q->perm.seq;
            int perr = dd_access(&q->perm, 2);
            if (perr) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            int full = (q->cbytes + msgsz > q->qbytes) || (q->qnum >= DDMSG_SLOTS);
            if (full) {
                if (flag & L_IPC_NOWAIT) {
                    dd_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
                dd_unlock(&C->lock);
                did_wait = 1;
                struct timespec ts = {0, 200000};
                if (nanosleep(&ts, NULL) < 0 && errno == EINTR) {
                    G_RET(c) = (uint64_t)(-EINTR);
                    break;
                }
                continue;
            }
            struct ddmsg_store *st = dd_msg_store(idx, qseq, 0);
            if (!st || st->freehead < 0) {
                dd_unlock(&C->lock);
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
            q->stime = dd_now();
            q->lspid = container_pid();
            dd_unlock(&C->lock);
            G_RET(c) = 0;
            break;
        }
        break;
    }
    case 188: { // msgrcv(msqid, msgp, msgsz, msgtyp, msgflg)
        int id = (int)a0;
        size_t msgsz = (size_t)a2;
        long msgtyp = (long)a3;
        int flag = (int)a4;
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (!host_range_mapped((uintptr_t)a1, sizeof(long) + msgsz)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int did_wait = 0;
        for (;;) {
            dd_lock(&C->lock);
            struct ddmsgq *q = msg_by_id(C, id);
            if (!q) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(did_wait ? -EIDRM : -EINVAL);
                break;
            }
            uint32_t idx = msg_idx_of(C, q);
            uint32_t qseq = q->perm.seq;
            int perr = dd_access(&q->perm, 4);
            if (perr) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)perr;
                break;
            }
            struct ddmsg_store *st = dd_msg_store(idx, qseq, 0);
            if (!st) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Select a message: type 0 = first; >0 = first of that type (or first NOT of it w/ MSG_EXCEPT);
            // <0 = the message with the lowest mtype that is <= |msgtyp|.
            int prev = -1, cur = st->head, best = -1, bestprev = -1;
            while (cur != -1) {
                struct ddmsg_slot *sl = &st->slots[cur];
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
                struct ddmsg_slot *sl = &st->slots[best];
                if (sl->size > msgsz && !(flag & L_MSG_NOERROR)) {
                    dd_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-E2BIG);
                    break;
                }
                size_t copy = sl->size > msgsz ? msgsz : sl->size;
                *(long *)a1 = sl->mtype;
                if (copy) memcpy((void *)(a1 + sizeof(long)), sl->data, copy);
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
                q->rtime = dd_now();
                q->lrpid = container_pid();
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)copy;
                break;
            }
            if (flag & L_IPC_NOWAIT) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-ENOMSG);
                break;
            }
            dd_unlock(&C->lock);
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
        C = dd_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (cmd == L_IPC_INFO || cmd == L_MSG_INFO) {
            dd_lock(&C->lock);
            int maxid = -1, n = msg_count(C, &maxid);
            uint64_t rc = 0;
            if (!host_range_mapped((uintptr_t)a2, sizeof(struct msginfo_guest)))
                rc = (uint64_t)(-EFAULT);
            else {
                struct msginfo_guest *g = (struct msginfo_guest *)a2;
                memset(g, 0, sizeof *g);
                g->msgmax = DDIPC_MSGMAX;
                g->msgmni = DDIPC_MSGMNI_ADV;
                g->msgmnb = DDIPC_MSGMNB;
                g->msgssz = 16;
                g->msgtql = DDIPC_MSGMNI_ADV;
                g->msgseg = 0xffff;
                if (cmd == L_MSG_INFO) {
                    g->msgpool = n;
                    g->msgtql = n;
                }
            }
            dd_unlock(&C->lock);
            G_RET(c) = rc ? rc : (uint64_t)(maxid < 0 ? 0 : maxid);
            break;
        }
        if (cmd == L_MSG_STAT || cmd == L_MSG_STAT_ANY) {
            dd_lock(&C->lock);
            if (id < 0 || id >= DDIPC_MSGMNI || !C->msg[id].inuse) {
                dd_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (cmd == L_MSG_STAT) {
                int perr = dd_access(&C->msg[id].perm, 4);
                if (perr) {
                    dd_unlock(&C->lock);
                    G_RET(c) = (uint64_t)perr;
                    break;
                }
            }
            uint64_t retid = dd_id(DDIPC_MSGMNI, (uint32_t)id, C->msg[id].perm.seq);
            uint64_t rc = msg_stat_to_guest(C, (uint32_t)id, a2);
            dd_unlock(&C->lock);
            G_RET(c) = rc ? rc : retid;
            break;
        }
        dd_lock(&C->lock);
        struct ddmsgq *q = msg_by_id(C, id);
        if (!q) {
            dd_unlock(&C->lock);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint32_t idx = msg_idx_of(C, q);
        uint64_t rc;
        switch (cmd) {
        case L_IPC_STAT: {
            int perr = dd_access(&q->perm, 4);
            rc = perr ? (uint64_t)perr : msg_stat_to_guest(C, idx, a2);
            break;
        }
        case L_IPC_SET: {
            int perr = dd_owner(&q->perm);
            if (perr) {
                rc = (uint64_t)perr;
                break;
            }
            if (!host_range_mapped((uintptr_t)a2, sizeof(struct msqid64_ds_guest))) {
                rc = (uint64_t)(-EFAULT);
                break;
            }
            struct msqid64_ds_guest *g = (struct msqid64_ds_guest *)a2;
            // Raising qbytes above the default ceiling needs privilege (CAP_SYS_RESOURCE); lowering is free.
            if (g->msg_qbytes > DDIPC_MSGMNB && cred_euid() != 0) {
                rc = (uint64_t)(-EPERM);
                break;
            }
            q->perm.uid = g->msg_perm.uid;
            q->perm.gid = g->msg_perm.gid;
            q->perm.mode = (q->perm.mode & ~0777u) | (g->msg_perm.mode & 0777);
            if (g->msg_qbytes) q->qbytes = g->msg_qbytes;
            q->ctime = dd_now();
            rc = 0;
            break;
        }
        case L_IPC_RMID: {
            int perr = dd_owner(&q->perm);
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
        dd_unlock(&C->lock);
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
