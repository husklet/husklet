// Extracted from service(): SysV IPC syscalls (shm/sem/msg). Returns 1 if nr was handled (G_RET set), 0 otherwise.
// Included by service.c after service/helpers.c, before service(); sees the same TU scope (globals + helpers).

// ---- Linux control-command numbers (task #418) -----------------------------------------------------
// macOS shmctl/semctl/msgctl only know IPC_RMID(0)/IPC_SET(1)/IPC_STAT(2); the Linux-specific INFO/STAT/
// LOCK forms below are emulated in-engine (macOS has no kernel support for them). Values are the Linux
// asm-generic ABI the guest passes. IPC_RMID/SET/STAT already coincide with the macOS SDK constants.
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

// The guest's `struct ipc64_perm` (aarch64 asm-generic, 48 bytes) — the leading member of every *id64_ds.
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
// struct semid64_ds — the ONE SysV struct whose 64-bit layout is arch-specific (shmid64_ds/msqid64_ds are
// identical across x86-64 and aarch64). x86-64's `struct semid64_ds` carries a reserved slot after each
// time field (otime_high/ctime_high, an old x86 quirk), pushing sem_nsems to offset 80 in a 104-byte
// struct; the aarch64 asm-generic form has neither, with sem_nsems at 64 in an 88-byte struct. Verified by
// raw-syscall probe on both arches. CANON_X86ONLY is defined only in the x86_64 engine (translate/x86_64).
#ifdef CANON_X86ONLY
struct semid64_ds_guest {
    struct ipc64_perm_guest sem_perm;         // 0   (48)
    int64_t sem_otime, sem_otime_high;        // 48, 56
    int64_t sem_ctime, sem_ctime_high;        // 64, 72
    uint64_t sem_nsems, unused3, unused4;      // 80, 88, 96 -> 104
};
#else
struct semid64_ds_guest {
    struct ipc64_perm_guest sem_perm;         // 0   (48)
    int64_t sem_otime, sem_ctime;             // 48, 56
    uint64_t sem_nsems, unused3, unused4;      // 64, 72, 80 -> 88
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

// macOS shmget rounds the segment size up to a page, and its IPC_STAT reports that rounded size; Linux
// reports the size the caller originally requested. Remember each segment's requested size (keyed by the
// host shmid) so IPC_STAT can report it faithfully -- otherwise the guest's `shm_segsz >= requested`
// check can fail.
#define SHM_SEGSZ_MAX 256
static struct {
    int used, id;
    size_t segsz;
} g_shm_segsz[SHM_SEGSZ_MAX];
static pthread_mutex_t g_shm_segsz_m = PTHREAD_MUTEX_INITIALIZER;

// In-process registry of live SysV ids (type: 0=shm 1=sem 2=msg), so the Linux-only index-based *_STAT /
// resource-counting *_INFO commands (which macOS cannot enumerate) can be emulated over the objects this
// container created. Inherited across fork (COW); reinit the lock in the fork child.
#define IPC_REG_MAX 256
static struct {
    int used, type, id;
} g_ipc_reg[IPC_REG_MAX];
static pthread_mutex_t g_ipc_reg_m = PTHREAD_MUTEX_INITIALIZER;

// fork() only clones the calling thread: a peer that held these locks at fork time leaves them inherited-
// locked with no owner, deadlocking the single-threaded child on its next SysV op. Reinit to unlocked in
// the child (always safe: no peer survives). Same fork-unsafe-mutex class as g_jit_lock. Called from proc.c.
static void sysv_after_fork(void) {
    pthread_mutex_init(&g_shm_segsz_m, NULL);
    pthread_mutex_init(&g_ipc_reg_m, NULL);
}

static void shm_segsz_remember(int id, size_t segsz) {
    pthread_mutex_lock(&g_shm_segsz_m);
    int slot = -1;
    for (int i = 0; i < SHM_SEGSZ_MAX; i++) {
        if (g_shm_segsz[i].used && g_shm_segsz[i].id == id) {
            slot = i;
            break;
        }
        if (slot < 0 && !g_shm_segsz[i].used) slot = i;
    }
    if (slot >= 0) {
        g_shm_segsz[slot].used = 1;
        g_shm_segsz[slot].id = id;
        g_shm_segsz[slot].segsz = segsz;
    }
    pthread_mutex_unlock(&g_shm_segsz_m);
}

static size_t shm_segsz_lookup(int id) {
    size_t r = 0;
    pthread_mutex_lock(&g_shm_segsz_m);
    for (int i = 0; i < SHM_SEGSZ_MAX; i++)
        if (g_shm_segsz[i].used && g_shm_segsz[i].id == id) {
            r = g_shm_segsz[i].segsz;
            break;
        }
    pthread_mutex_unlock(&g_shm_segsz_m);
    return r;
}

static void shm_segsz_forget(int id) {
    pthread_mutex_lock(&g_shm_segsz_m);
    for (int i = 0; i < SHM_SEGSZ_MAX; i++)
        if (g_shm_segsz[i].used && g_shm_segsz[i].id == id) {
            g_shm_segsz[i].used = 0;
            break;
        }
    pthread_mutex_unlock(&g_shm_segsz_m);
}

// Register a newly-created id (dedup: a get() on an existing key returns the same id). No-op if full.
static void ipc_reg_add(int type, int id) {
    pthread_mutex_lock(&g_ipc_reg_m);
    int slot = -1;
    for (int i = 0; i < IPC_REG_MAX; i++) {
        if (g_ipc_reg[i].used && g_ipc_reg[i].type == type && g_ipc_reg[i].id == id) {
            slot = -2;
            break;
        }
        if (slot < 0 && !g_ipc_reg[i].used) slot = i;
    }
    if (slot >= 0) {
        g_ipc_reg[slot].used = 1;
        g_ipc_reg[slot].type = type;
        g_ipc_reg[slot].id = id;
    }
    pthread_mutex_unlock(&g_ipc_reg_m);
}
static void ipc_reg_del(int type, int id) {
    pthread_mutex_lock(&g_ipc_reg_m);
    for (int i = 0; i < IPC_REG_MAX; i++)
        if (g_ipc_reg[i].used && g_ipc_reg[i].type == type && g_ipc_reg[i].id == id) {
            g_ipc_reg[i].used = 0;
            break;
        }
    pthread_mutex_unlock(&g_ipc_reg_m);
}
// Number of live registered ids of `type` (== the highest used index + 1, the Linux IPC_INFO return).
static int ipc_reg_count(int type) {
    int n = 0;
    pthread_mutex_lock(&g_ipc_reg_m);
    for (int i = 0; i < IPC_REG_MAX; i++)
        if (g_ipc_reg[i].used && g_ipc_reg[i].type == type) n++;
    pthread_mutex_unlock(&g_ipc_reg_m);
    return n;
}
// The `idx`-th live id of `type` (registry order), or -1 if idx is out of range — the id an index-based
// *_STAT resolves to.
static int ipc_reg_at(int type, int idx) {
    int n = 0, r = -1;
    if (idx < 0) return -1;
    pthread_mutex_lock(&g_ipc_reg_m);
    for (int i = 0; i < IPC_REG_MAX; i++)
        if (g_ipc_reg[i].used && g_ipc_reg[i].type == type) {
            if (n == idx) {
                r = g_ipc_reg[i].id;
                break;
            }
            n++;
        }
    pthread_mutex_unlock(&g_ipc_reg_m);
    return r;
}

// ---- container uid/gid virtualization -------------------------------------------------------------
// macOS records the real host uid/gid (== getuid()/getgid()) as an IPC object's owner/creator; the guest
// must see the container identity instead (cuid()/cgid(), default 0=root). Map host<->guest by identity.
static uint32_t ipc_uid_h2g(uid_t hu) { return hu == (uid_t)getuid() ? (uint32_t)cuid() : (uint32_t)hu; }
static uint32_t ipc_gid_h2g(gid_t hg) { return hg == (gid_t)getgid() ? (uint32_t)cgid() : (uint32_t)hg; }
static uid_t ipc_uid_g2h(uint32_t gu) { return gu == (uint32_t)cuid() ? (uid_t)getuid() : (uid_t)gu; }
static gid_t ipc_gid_g2h(uint32_t gg) { return gg == (uint32_t)cgid() ? (gid_t)getgid() : (gid_t)gg; }

// Reverse ipc_ns_key(): the host object's key is the per-container-namespaced key we created it with, but
// the guest must see the ORIGINAL key it passed. ipc_ns_key XORs by a DD_NETNS-derived salt, so re-XORing
// by the same salt recovers it; IPC_PRIVATE(0) always maps to 0 (it is never namespaced). (When DD_NETNS is
// unset this is the identity.) The rare forward IPC_PRIVATE-collision bump (+1) is not reversed.
static int32_t ipc_key_h2g(int32_t hk) {
    if (hk == IPC_PRIVATE) return IPC_PRIVATE;
    const char *ns = getenv("DD_NETNS");
    if (!ns || !ns[0]) return hk;
    uint32_t salt = 2166136261u;
    for (const char *p = ns; *p; p++) {
        salt ^= (uint8_t)*p;
        salt = salt * 16777619u;
    }
    return (int32_t)((uint32_t)hk ^ (salt & 0x7fffffffu));
}

static void ipc_perm_to_guest(struct ipc64_perm_guest *g, const struct ipc_perm *h) {
    g->key = ipc_key_h2g(h->_key);
    g->uid = ipc_uid_h2g(h->uid);
    g->gid = ipc_gid_h2g(h->gid);
    g->cuid = ipc_uid_h2g(h->cuid);
    g->cgid = ipc_gid_h2g(h->cgid);
    g->mode = h->mode;
    g->seq = h->_seq;
    g->pad2 = 0;
    g->unused1 = g->unused2 = 0;
}

// Emulate Linux ipcperms(): the macOS kernel checks against the real host uid (unchanged by the guest's
// virtual setuid), so we must enforce the container-visible mode ourselves. `want` is a rwx bitmask (4=read
// for IPC_STAT, 2=write). Returns 0 if allowed, -EACCES otherwise.
static int ipc_check_access(const struct ipc_perm *p, int want) {
    cred_init();
    if (cred_euid() == 0) return 0; // root bypasses the mode check
    uint32_t ouid = ipc_uid_h2g(p->uid), ccuid = ipc_uid_h2g(p->cuid);
    uint32_t ogid = ipc_gid_h2g(p->gid), ccgid = ipc_gid_h2g(p->cgid);
    int granted, eu = cred_euid(), eg = cred_egid();
    if ((uint32_t)eu == ouid || (uint32_t)eu == ccuid) granted = (p->mode >> 6) & 7;
    else if ((uint32_t)eg == ogid || (uint32_t)eg == ccgid) granted = (p->mode >> 3) & 7;
    else granted = p->mode & 7;
    return (granted & want) == want ? 0 : -EACCES;
}
// IPC_SET / IPC_RMID require the caller be owner, creator, or privileged (Linux). 0 ok, -EPERM otherwise.
static int ipc_check_owner(const struct ipc_perm *p) {
    cred_init();
    if (cred_euid() == 0) return 0;
    uint32_t ouid = ipc_uid_h2g(p->uid), ccuid = ipc_uid_h2g(p->cuid);
    return ((uint32_t)cred_euid() == ouid || (uint32_t)cred_euid() == ccuid) ? 0 : -EPERM;
}

// ---- IPC_STAT marshaling (host struct -> guest *id64_ds) -------------------------------------------
// shmctl(IPC_STAT/SHM_STAT): query the host, then marshal its `struct shmid_ds` into the guest layout at
// `gbuf`. Reports the guest-requested segment size when we have it. Returns 0 or -errno.
static uint64_t shm_stat_to_guest(int id, uint64_t gbuf) {
    struct shmid_ds h;
    if (shmctl(id, IPC_STAT, &h) < 0) return (uint64_t)(-errno);
    int perr = ipc_check_access(&h.shm_perm, 4);
    if (perr) return (uint64_t)perr;
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct shmid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct shmid64_ds_guest *g = (struct shmid64_ds_guest *)gbuf;
    memset(g, 0, sizeof *g);
    ipc_perm_to_guest(&g->shm_perm, &h.shm_perm);
    size_t req = shm_segsz_lookup(id);
    g->shm_segsz = req ? req : (uint64_t)h.shm_segsz;
    g->shm_atime = h.shm_atime;
    g->shm_dtime = h.shm_dtime;
    g->shm_ctime = h.shm_ctime;
    g->shm_cpid = h.shm_cpid;
    g->shm_lpid = h.shm_lpid;
    g->shm_nattch = (uint64_t)h.shm_nattch;
    return 0;
}
static uint64_t sem_stat_to_guest(int id, uint64_t gbuf) {
    struct semid_ds h;
    union semun_s {
        int val;
        struct semid_ds *buf;
        unsigned short *array;
    } arg;
    arg.buf = &h;
    if (semctl(id, 0, IPC_STAT, arg) < 0) return (uint64_t)(-errno);
    int perr = ipc_check_access(&h.sem_perm, 4);
    if (perr) return (uint64_t)perr;
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct semid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct semid64_ds_guest *g = (struct semid64_ds_guest *)gbuf;
    memset(g, 0, sizeof *g);
    ipc_perm_to_guest(&g->sem_perm, &h.sem_perm);
    g->sem_otime = h.sem_otime;
    g->sem_ctime = h.sem_ctime;
    g->sem_nsems = h.sem_nsems;
    return 0;
}
static uint64_t msg_stat_to_guest(int id, uint64_t gbuf) {
    struct msqid_ds h;
    if (msgctl(id, IPC_STAT, &h) < 0) return (uint64_t)(-errno);
    int perr = ipc_check_access(&h.msg_perm, 4);
    if (perr) return (uint64_t)perr;
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct msqid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct msqid64_ds_guest *g = (struct msqid64_ds_guest *)gbuf;
    memset(g, 0, sizeof *g);
    ipc_perm_to_guest(&g->msg_perm, &h.msg_perm);
    g->msg_stime = h.msg_stime;
    g->msg_rtime = h.msg_rtime;
    g->msg_ctime = h.msg_ctime;
    g->msg_cbytes = h.msg_cbytes;
    g->msg_qnum = h.msg_qnum;
    g->msg_qbytes = h.msg_qbytes;
    g->msg_lspid = h.msg_lspid;
    g->msg_lrpid = h.msg_lrpid;
    return 0;
}

// ---- IPC_INFO / *_INFO fill (macOS has no equivalent -> plausible Linux limits + live counts) -------
static uint64_t shm_info_fill(int cmd, uint64_t gbuf) {
    if (cmd == L_IPC_INFO) {
        if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct shminfo_guest))) return (uint64_t)(-EFAULT);
        struct shminfo_guest *g = (struct shminfo_guest *)gbuf;
        memset(g, 0, sizeof *g);
        g->shmmax = 0xffffffffffffffffULL;
        g->shmmin = 1;
        g->shmmni = 4096;
        g->shmseg = 4096;
        g->shmall = 0xffffffffffffffffULL / 4096;
    } else { // SHM_INFO
        if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct shm_info_guest))) return (uint64_t)(-EFAULT);
        struct shm_info_guest *g = (struct shm_info_guest *)gbuf;
        memset(g, 0, sizeof *g);
        g->used_ids = ipc_reg_count(0);
    }
    int n = ipc_reg_count(0);
    return (uint64_t)(n > 0 ? n - 1 : 0);
}
static uint64_t sem_info_fill(int cmd, uint64_t gbuf) {
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct seminfo_guest))) return (uint64_t)(-EFAULT);
    struct seminfo_guest *g = (struct seminfo_guest *)gbuf;
    memset(g, 0, sizeof *g);
    g->semmni = 32000;
    g->semmsl = 32000;
    g->semmns = 1024000000;
    g->semopm = 500;
    g->semvmx = 32767;
    g->semaem = 32767;
    g->semmnu = 2147483647;
    g->semume = 500;
    if (cmd == L_SEM_INFO) { // resource form: semusz=#sets, semaem=#semaphores in use
        g->semusz = ipc_reg_count(1);
        g->semaem = ipc_reg_count(1);
    }
    int n = ipc_reg_count(1);
    return (uint64_t)(n > 0 ? n - 1 : 0);
}
static uint64_t msg_info_fill(int cmd, uint64_t gbuf) {
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct msginfo_guest))) return (uint64_t)(-EFAULT);
    struct msginfo_guest *g = (struct msginfo_guest *)gbuf;
    memset(g, 0, sizeof *g);
    g->msgmax = 8192;
    g->msgmni = 32000;
    g->msgmnb = 16384;
    g->msgssz = 16;
    g->msgtql = 32000;
    g->msgseg = 0xffff;
    if (cmd == L_MSG_INFO) { // resource form: msgpool/msgtql become live counts
        g->msgpool = ipc_reg_count(2);
        g->msgtql = ipc_reg_count(2);
    }
    int n = ipc_reg_count(2);
    return (uint64_t)(n > 0 ? n - 1 : 0);
}

// ---- IPC_SET writeback (mutable perm fields from the guest *id64_ds) -------------------------------
static uint64_t shm_set_from_guest(int id, uint64_t gbuf) {
    struct shmid_ds h;
    if (shmctl(id, IPC_STAT, &h) < 0) return (uint64_t)(-errno);
    int perr = ipc_check_owner(&h.shm_perm);
    if (perr) return (uint64_t)perr;
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct shmid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct shmid64_ds_guest *g = (struct shmid64_ds_guest *)gbuf;
    h.shm_perm.uid = ipc_uid_g2h(g->shm_perm.uid);
    h.shm_perm.gid = ipc_gid_g2h(g->shm_perm.gid);
    h.shm_perm.mode = (h.shm_perm.mode & ~0777) | (g->shm_perm.mode & 0777);
    return shmctl(id, IPC_SET, &h) < 0 ? (uint64_t)(-errno) : 0;
}
static uint64_t sem_set_from_guest(int id, uint64_t gbuf) {
    struct semid_ds h;
    union semun_s {
        int val;
        struct semid_ds *buf;
        unsigned short *array;
    } arg;
    arg.buf = &h;
    if (semctl(id, 0, IPC_STAT, arg) < 0) return (uint64_t)(-errno);
    int perr = ipc_check_owner(&h.sem_perm);
    if (perr) return (uint64_t)perr;
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct semid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct semid64_ds_guest *g = (struct semid64_ds_guest *)gbuf;
    h.sem_perm.uid = ipc_uid_g2h(g->sem_perm.uid);
    h.sem_perm.gid = ipc_gid_g2h(g->sem_perm.gid);
    h.sem_perm.mode = (h.sem_perm.mode & ~0777) | (g->sem_perm.mode & 0777);
    arg.buf = &h;
    return semctl(id, 0, IPC_SET, arg) < 0 ? (uint64_t)(-errno) : 0;
}
static uint64_t msg_set_from_guest(int id, uint64_t gbuf) {
    struct msqid_ds h;
    if (msgctl(id, IPC_STAT, &h) < 0) return (uint64_t)(-errno);
    int perr = ipc_check_owner(&h.msg_perm);
    if (perr) return (uint64_t)perr;
    if (!host_range_mapped((uintptr_t)gbuf, sizeof(struct msqid64_ds_guest))) return (uint64_t)(-EFAULT);
    struct msqid64_ds_guest *g = (struct msqid64_ds_guest *)gbuf;
    h.msg_perm.uid = ipc_uid_g2h(g->msg_perm.uid);
    h.msg_perm.gid = ipc_gid_g2h(g->msg_perm.gid);
    h.msg_perm.mode = (h.msg_perm.mode & ~0777) | (g->msg_perm.mode & 0777);
    h.msg_qbytes = g->msg_qbytes; // raising above the prior max needs privilege — the host enforces that
    return msgctl(id, IPC_SET, &h) < 0 ? (uint64_t)(-errno) : 0;
}

static int svc_sysv(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    switch (nr) {
    // ===================== SysV shared memory (per-container key namespace) =====================
    case 194: { // shmget(key, size, shmflg)
        int r = shmget(ipc_ns_key((key_t)a0), (size_t)a1, (int)a2);
        if (r >= 0) {
            if (a1) shm_segsz_remember(r, (size_t)a1); // remember requested size for IPC_STAT (skip size-0)
            ipc_reg_add(0, r);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 196: { // shmat(shmid, shmaddr, shmflg) -- the guest runs in-process so the host map is usable
        void *p = shmat((int)a0, (const void *)a1, (int)a2);
        G_RET(c) = (p == (void *)-1) ? (uint64_t)(-errno) : (uint64_t)p;
        break;
    }
    case 197: { // shmdt(shmaddr)
        int r = shmdt((const void *)a0);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 195: { // shmctl(shmid, cmd, buf)
        int id = (int)a0, cmd = (int)a1;
        switch (cmd) {
        case L_IPC_RMID: {
            struct shmid_ds h;
            if (shmctl(id, IPC_STAT, &h) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int perr = ipc_check_owner(&h.shm_perm);
            if (perr) {
                G_RET(c) = (uint64_t)perr;
                break;
            }
            int r = shmctl(id, IPC_RMID, NULL);
            if (r == 0) {
                shm_segsz_forget(id);
                ipc_reg_del(0, id);
            }
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        case L_IPC_STAT:
            G_RET(c) = shm_stat_to_guest(id, a2);
            break;
        case L_SHM_STAT:
        case L_SHM_STAT_ANY: {
            int rid = ipc_reg_at(0, id); // shmid arg is an index for SHM_STAT
            if (rid < 0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            uint64_t rc = shm_stat_to_guest(rid, a2);
            G_RET(c) = rc ? rc : (uint64_t)rid;
            break;
        }
        case L_IPC_SET:
            G_RET(c) = shm_set_from_guest(id, a2);
            break;
        case L_IPC_INFO:
        case L_SHM_INFO:
            G_RET(c) = shm_info_fill(cmd, a2);
            break;
        case L_SHM_LOCK:
        case L_SHM_UNLOCK: {
            struct shmid_ds h; // validate the id + owner/root (Linux gates SHM_LOCK on ownership/CAP_IPC_LOCK)
            if (shmctl(id, IPC_STAT, &h) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            G_RET(c) = (uint64_t)ipc_check_owner(&h.shm_perm); // 0 on success; macOS has no wired pages to (un)lock
            break;
        }
        default:
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        break;
    }

    // ===================== SysV semaphores =====================
    case 190: { // semget(key, nsems, semflg)
        int r = semget(ipc_ns_key((key_t)a0), (int)a1, (int)a2);
        if (r >= 0) ipc_reg_add(1, r);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 192:   // semtimedop -> semop (glibc routes semop() through it; macOS has no timed variant)
    case 193: { // semop(semid, sops, nsops) -- struct sembuf is layout-compatible with the guest's
        int r = semop((int)a0, (struct sembuf *)a1, (size_t)a2);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 191: { // semctl(semid, semnum, cmd, arg)
        int id = (int)a0, semnum = (int)a1, lc = (int)a2, r;
        union semun_ {
            int val;
            struct semid_ds *buf;
            unsigned short *array;
        } arg;
        // Linux-specific control forms first (macOS can't express them / needs a virtualized check).
        if (lc == L_IPC_STAT) {
            G_RET(c) = sem_stat_to_guest(id, a3);
            break;
        }
        if (lc == L_SEM_STAT || lc == L_SEM_STAT_ANY) {
            int rid = ipc_reg_at(1, id); // semid arg is an index for SEM_STAT
            if (rid < 0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            uint64_t rc = sem_stat_to_guest(rid, a3);
            G_RET(c) = rc ? rc : (uint64_t)rid;
            break;
        }
        if (lc == L_IPC_SET) {
            G_RET(c) = sem_set_from_guest(id, a3);
            break;
        }
        if (lc == L_IPC_INFO || lc == L_SEM_INFO) {
            G_RET(c) = sem_info_fill(lc, a3);
            break;
        }
        if (lc == L_IPC_RMID) {
            struct semid_ds h;
            arg.buf = &h;
            if (semctl(id, 0, IPC_STAT, arg) == 0) {
                int perr = ipc_check_owner(&h.sem_perm);
                if (perr) {
                    G_RET(c) = (uint64_t)perr;
                    break;
                }
            }
            r = semctl(id, 0, sem_cmd_l2m(lc));
            if (r == 0) ipc_reg_del(1, id);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
            break;
        }
        int mc = sem_cmd_l2m(lc);
        if (lc == 16) {
            arg.val = (int)a3;
            r = semctl(id, semnum, mc, arg);
        } // SETVAL
        else if (lc == 13 || lc == 17) {
            arg.array = (unsigned short *)a3;
            r = semctl(id, semnum, mc, arg);
        } // GET/SETALL
        else if (lc == 11 || lc == 12 || lc == 14 || lc == 15) {
            r = semctl(id, semnum, mc);
        } // GETPID/GETVAL/GETNCNT/GETZCNT
        else {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // unknown cmd
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }

    // ===================== SysV message queues =====================
    case 186: { // msgget(key, msgflg)
        int r = msgget(ipc_ns_key((key_t)a0), (int)a1);
        if (r >= 0) ipc_reg_add(2, r);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 189: { // msgsnd(msqid, msgp, msgsz, msgflg) -- msgbuf {long mtype; char mtext[]} is compatible
        int r = msgsnd((int)a0, (const void *)a1, (size_t)a2, (int)a3);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 188: { // msgrcv(msqid, msgp, msgsz, msgtyp, msgflg)
        ssize_t r = msgrcv((int)a0, (void *)a1, (size_t)a2, (long)a3, (int)a4);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 187: { // msgctl(msqid, cmd, buf)
        int id = (int)a0, cmd = (int)a1;
        switch (cmd) {
        case L_IPC_RMID: {
            struct msqid_ds h;
            if (msgctl(id, IPC_STAT, &h) < 0) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int perr = ipc_check_owner(&h.msg_perm);
            if (perr) {
                G_RET(c) = (uint64_t)perr;
                break;
            }
            int r = msgctl(id, IPC_RMID, NULL);
            if (r == 0) ipc_reg_del(2, id);
            G_RET(c) = r < 0 ? (uint64_t)(-errno) : 0;
            break;
        }
        case L_IPC_STAT:
            G_RET(c) = msg_stat_to_guest(id, a2);
            break;
        case L_MSG_STAT:
        case L_MSG_STAT_ANY: {
            int rid = ipc_reg_at(2, id); // msqid arg is an index for MSG_STAT
            if (rid < 0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            uint64_t rc = msg_stat_to_guest(rid, a2);
            G_RET(c) = rc ? rc : (uint64_t)rid;
            break;
        }
        case L_IPC_SET:
            G_RET(c) = msg_set_from_guest(id, a2);
            break;
        case L_IPC_INFO:
        case L_MSG_INFO:
            G_RET(c) = msg_info_fill(cmd, a2);
            break;
        default:
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        break;
    }
    default: return 0;
    }
    // Map the host(macOS) errno left in G_RET to the Linux errno the guest expects (e.g. SysV msgrcv
    // IPC_NOWAIT on an empty queue -> macOS ENOMSG=91, Linux ENOMSG=42). Like every other svc_<family>()
    // tail, sysv early-returns from service_local before its trailing m2l boundary, so it must translate
    // here — otherwise the raw macOS errno (identical only for the low 1..34 codes) leaks to the guest.
    return svc_done(c);
}
