#include "sysv_state.h"

static void svc_shmget(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static void svc_shmat(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static void svc_shmdt(struct cpu *c, uint64_t a0) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static void svc_shmctl(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static void svc_semget(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static int sem_ops_valid(const struct hl_sem_entry *sem, const struct sembuf_guest *ops, size_t count) {
    for (size_t i = 0; i < count; i++)
        if (ops[i].sem_num >= sem->nsems) return 0;
    return 1;
}

static int sem_blocking_op(const struct hl_sem_entry *sem, const struct sembuf_guest *ops, size_t count) {
    for (size_t i = 0; i < count; i++) {
        int current = (int)sem->val[ops[i].sem_num];
        int operation = ops[i].sem_op;
        if ((operation == 0 && current != 0) || (operation < 0 && current + operation < 0)) return (int)i;
        if (operation > 0 && current + operation > HL_IPC_SEMVMX) return -2;
    }
    return -1;
}

static void sem_waiters_adjust(struct hl_sem_entry *sem, const struct sembuf_guest *ops, size_t count, int delta) {
    for (size_t i = 0; i < count; i++) {
        int32_t *waiters = NULL;
        if (ops[i].sem_op < 0)
            waiters = &sem->ncnt[ops[i].sem_num];
        else if (ops[i].sem_op == 0)
            waiters = &sem->zcnt[ops[i].sem_num];
        if (waiters && (delta > 0 || *waiters > 0)) *waiters += delta;
    }
}

static void svc_semop(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3) {
    struct hl_ipc_ctrl *C;
    do {
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
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(did_wait ? -EIDRM : -EINVAL);
                break;
            }
            if (!sem_ops_valid(s, sops, nsops)) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EFBIG); // EFBIG(27 mac==linux) -- Linux returns EFBIG for sem_num OOR
                break;
            }
            int block_on = sem_blocking_op(s, sops, nsops);
            if (block_on == -2) {
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-ERANGE);
                goto sem_done;
            }
            if (block_on == -1) {
                if (waited_marked) { // leaving the wait: drop our ncnt/zcnt bookkeeping
                    sem_waiters_adjust(s, sops, nsops, -1);
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
                sem_waiters_adjust(s, sops, nsops, 1);
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
                    if (s2) sem_waiters_adjust(s2, sops, nsops, -1);
                    hl_ipc_unlock(&C->lock);
                    G_RET(c) = (uint64_t)(-EAGAIN);
                    break;
                }
            }
            struct timespec ts = {0, 200000}; // 200us poll
            if (nanosleep(&ts, NULL) < 0 && errno == EINTR) {
                hl_ipc_lock(&C->lock);
                struct hl_sem_entry *s2 = sem_by_id(C, id);
                if (s2) sem_waiters_adjust(s2, sops, nsops, -1);
                hl_ipc_unlock(&C->lock);
                G_RET(c) = (uint64_t)(-EINTR);
                break;
            }
        }
    sem_done:
        free(sops);
        break;
    } while (0);
}

static void svc_semctl_info(struct cpu *c, struct hl_ipc_ctrl *C, int cmd, uint64_t output) {
    hl_ipc_lock(&C->lock);
    int maxid = -1, count = sem_count(C, &maxid);
    uint64_t result = 0;
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
        info.semusz = count;
        info.semaem = count;
    }
    if (guest_copy_to(output, &info, sizeof(info)) != sizeof(info)) result = (uint64_t)(-EFAULT);
    hl_ipc_unlock(&C->lock);
    G_RET(c) = result ? result : (uint64_t)(maxid < 0 ? 0 : maxid);
}

static void svc_semctl_stat(struct cpu *c, struct hl_ipc_ctrl *C, int id, int cmd, uint64_t output) {
    hl_ipc_lock(&C->lock);
    if (id < 0 || id >= HL_IPC_SEMMNI || !C->sem[id].inuse) {
        hl_ipc_unlock(&C->lock);
        G_RET(c) = (uint64_t)(-EINVAL);
        return;
    }
    if (cmd == L_SEM_STAT) {
        int error = hl_ipc_access(&C->sem[id].perm, 4);
        if (error) {
            hl_ipc_unlock(&C->lock);
            G_RET(c) = (uint64_t)error;
            return;
        }
    }
    uint64_t result_id = hl_ipc_id(HL_IPC_SEMMNI, (uint32_t)id, C->sem[id].perm.seq);
    uint64_t result = sem_stat_to_guest(C, (uint32_t)id, output);
    hl_ipc_unlock(&C->lock);
    G_RET(c) = result ? result : result_id;
}

static void svc_semctl(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3) {
    struct hl_ipc_ctrl *C;
    do {
        int id = (int)a0, semnum = (int)a1, cmd = (int)a2;
        C = hl_ipc_ctrl();
        if (!C) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (cmd == L_IPC_INFO || cmd == L_SEM_INFO) {
            svc_semctl_info(c, C, cmd, a3);
            break;
        }
        if (cmd == L_SEM_STAT || cmd == L_SEM_STAT_ANY) {
            svc_semctl_stat(c, C, id, cmd, a3);
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
    } while (0);
}

static void svc_msgget(struct cpu *c, uint64_t a0, uint64_t a1) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static void svc_msgsnd(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static void svc_msgrcv(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static void svc_msgctl(struct cpu *c, uint64_t a0, uint64_t a1, uint64_t a2) {
    struct hl_ipc_ctrl *C;
    do {
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
    } while (0);
}

static int svc_sysv(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    (void)a5;
    switch (nr) {
    case 194: svc_shmget(c, a0, a1, a2); break;
    case 196: svc_shmat(c, a0, a1, a2); break;
    case 197: svc_shmdt(c, a0); break;
    case 195: svc_shmctl(c, a0, a1, a2); break;
    case 190: svc_semget(c, a0, a1, a2); break;
    case 192:
    case 193: svc_semop(c, nr, a0, a1, a2, a3); break;
    case 191: svc_semctl(c, a0, a1, a2, a3); break;
    case 186: svc_msgget(c, a0, a1); break;
    case 189: svc_msgsnd(c, a0, a1, a2, a3); break;
    case 188: svc_msgrcv(c, a0, a1, a2, a3, a4); break;
    case 187: svc_msgctl(c, a0, a1, a2); break;
    default: return 0;
    }
    // Map the host(macOS) errno left in G_RET to the Linux errno the guest expects (e.g. ENOMSG 91->42,
    // EIDRM 90->43, EAGAIN 35->11). Like every other svc_<family>() tail, sysv early-returns from
    // service_local before its trailing m2l boundary, so it must translate here.
    return svc_done_host(c);
}
