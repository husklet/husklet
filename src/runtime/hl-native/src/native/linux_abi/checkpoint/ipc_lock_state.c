// Checkpoint admission for the two IPC state domains the image format does not
// yet carry: SysV IPC (shm segments, semaphore sets, message queues) and fcntl
// record locks / flock(2) leases.
//
// Neither domain lives in a guest descriptor, so the descriptor scan in image.c
// cannot see them and every refusal path there steps straight past them. Before
// this gate a container holding either produced a checkpoint that committed
// cleanly and restored WITHOUT them:
//
//   * SysV -- the emulation in syscall/sysv_state.h keeps a per-container control
//     block (a named POSIX shm object keyed by the IPC namespace hash) holding the
//     shm/sem/msg descriptor tables, with segment and queue payloads in their own
//     named objects. None of it is reachable from a guest fd. PostgreSQL selects
//     the `sysv` dynamic-shared-memory implementation on this platform, so a live
//     cluster always holds segments; restoring without them yields backends
//     attached to nothing.
//   * File locks -- the POSIX record-lock table in syscall/emulation_state.c
//     (`struct poslk_shm`) is a MAP_SHARED region owned per host pid, holding both
//     fcntl record locks and the flock broker leases. PostgreSQL interlocks its
//     data directory here. Dropping the interlock lets two restored clusters open
//     one data directory at once, which is a corruption class, not a cosmetic gap.
//
// So capture fails closed and names the domain and the offending object. This is
// deliberately a refusal and not a capture: a refused checkpoint costs a run, a
// silently incomplete one costs the cluster. `ckpt_recovery_permissive_requested()`
// downgrades it to a warning for the same reason the socket path does -- an
// operator who has asked for a degraded image gets one, loudly.

// Map the container's SysV control block WITHOUT creating it. hl_ipc_ctrl() has
// O_CREAT|O_EXCL in its first attempt, so calling it here would materialise an
// empty registry (and arm its atexit unlink) for every container that never used
// SysV at all. Absent object == no SysV state, which is the answer we want.
static struct hl_ipc_ctrl *ckpt_sysv_registry_existing(void) {
    if (g_ctrl) return g_ctrl;
    char name[40];
    hl_ipc_control_name(name, sizeof name);
    int fd = shm_open(name, O_RDWR, 0600);
    if (fd < 0) return NULL;
    void *mapped = mmap(NULL, sizeof(struct hl_ipc_ctrl), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (mapped == MAP_FAILED) return NULL;
    struct hl_ipc_ctrl *control = (struct hl_ipc_ctrl *)mapped;
    if (atomic_load(&control->magic) != HL_IPC_CTRL_MAGIC) {
        munmap(control, sizeof *control);
        return NULL;
    }
    return control;
}

static void ckpt_sysv_release(struct hl_ipc_ctrl *control) {
    if (control && control != g_ctrl) munmap(control, sizeof *control);
}

// Count live descriptors in each SysV table and report the first of each kind by
// its guest-visible id, which is what an operator sees in `ipcs`.
static int ckpt_refuse_uncaptured_sysv_state(int permissive) {
    struct hl_ipc_ctrl *control = ckpt_sysv_registry_existing();
    if (control == NULL) return 0;
    unsigned shm_live = 0, sem_live = 0, msg_live = 0;
    int shm_first = -1, sem_first = -1, msg_first = -1;
    uint64_t shm_bytes = 0;
    int32_t shm_first_key = 0, sem_first_key = 0, msg_first_key = 0;
    unsigned sem_first_count = 0;
    for (uint32_t i = 0; i < HL_IPC_SHMMNI; i++) {
        const struct hl_shm_entry *entry = &control->shm[i];
        if (!entry->inuse) continue;
        shm_live++;
        shm_bytes += entry->segsz;
        if (shm_first < 0) {
            shm_first = (int)hl_ipc_id(HL_IPC_SHMMNI, i, entry->perm.seq);
            shm_first_key = entry->perm.key;
        }
    }
    for (uint32_t i = 0; i < HL_IPC_SEMMNI; i++) {
        const struct hl_sem_entry *entry = &control->sem[i];
        if (!entry->inuse) continue;
        sem_live++;
        if (sem_first < 0) {
            sem_first = (int)hl_ipc_id(HL_IPC_SEMMNI, i, entry->perm.seq);
            sem_first_key = entry->perm.key;
            sem_first_count = entry->nsems;
        }
    }
    for (uint32_t i = 0; i < HL_IPC_MSGMNI; i++) {
        const struct hl_msg_queue *entry = &control->msg[i];
        if (!entry->inuse) continue;
        msg_live++;
        if (msg_first < 0) {
            msg_first = (int)hl_ipc_id(HL_IPC_MSGMNI, i, entry->perm.seq);
            msg_first_key = entry->perm.key;
        }
    }
    ckpt_sysv_release(control);
    if (shm_live == 0 && sem_live == 0 && msg_live == 0) return 0;
    const char *verdict = permissive ? "degraded" : "refuse";
    if (shm_live)
        fprintf(stderr,
                "[ckpt] %s: SysV domain -- %u shared-memory segment(s) totalling %llu bytes are live "
                "(first id %d key 0x%08x); the checkpoint image carries no SysV section\n",
                verdict, shm_live, (unsigned long long)shm_bytes, shm_first, (unsigned)shm_first_key);
    if (sem_live)
        fprintf(stderr,
                "[ckpt] %s: SysV domain -- %u semaphore set(s) are live (first id %d key 0x%08x, %u semaphores); "
                "the checkpoint image carries no SysV section\n",
                verdict, sem_live, sem_first, (unsigned)sem_first_key, sem_first_count);
    if (msg_live)
        fprintf(stderr,
                "[ckpt] %s: SysV domain -- %u message queue(s) are live (first id %d key 0x%08x); "
                "the checkpoint image carries no SysV section\n",
                verdict, msg_live, msg_first, (unsigned)msg_first_key);
    return permissive ? 0 : -1;
}

// Record locks are owned per host pid, and the poslk table is shared by every
// engine on this uid -- so filter to THIS process. Each engine process dumps its
// own image and any one refusal aborts the whole group, which gives exact
// per-process attribution without refusing on a sibling container's locks.
static int ckpt_refuse_uncaptured_file_locks(int permissive) {
    if (g_poslk == NULL) return 0;
    int32_t self = poslk_mypid();
    if (self == 0) return 0;
    unsigned record_locks = 0, flock_leases = 0;
    const struct poslk_rec *record_first = NULL;
    const struct flock_broker_record *flock_first = NULL;
    int high_water = g_poslk->hi;
    if (high_water < 0) high_water = 0;
    if (high_water > POSLK_MAX) high_water = POSLK_MAX;
    for (int i = 0; i < high_water; i++) {
        const struct poslk_rec *record = &g_poslk->rec[i];
        if (record->owner != self) continue;
        record_locks++;
        if (record_first == NULL) record_first = record;
    }
    for (int i = 0; i < FLOCK_BROKER_MAX; i++) {
        const struct flock_broker_record *lease = &g_poslk->flock[i];
        if (!lease->active) continue;
        int mine = 0;
        for (int h = 0; h < FLOCK_HOLDERS_MAX; h++)
            if (lease->holders[h] == self) mine = 1;
        if (!mine) continue;
        flock_leases++;
        if (flock_first == NULL) flock_first = lease;
    }
    if (record_locks == 0 && flock_leases == 0) return 0;
    const char *verdict = permissive ? "degraded" : "refuse";
    if (record_locks)
        fprintf(stderr,
                "[ckpt] %s: file-lock domain -- this process holds %u fcntl record lock(s) (first: dev %llu ino %llu "
                "range [%lld,%lld) type %d); the checkpoint image carries no lock section, so a restore would drop "
                "the interlock\n",
                verdict, record_locks, (unsigned long long)record_first->device, (unsigned long long)record_first->object,
                (long long)record_first->lo, (long long)record_first->hi, record_first->type);
    if (flock_leases)
        fprintf(stderr,
                "[ckpt] %s: file-lock domain -- this process holds %u flock(2) lease(s) (first: dev %llu ino %llu "
                "mode %u); the checkpoint image carries no lock section, so a restore would drop the interlock\n",
                verdict, flock_leases, (unsigned long long)flock_first->device, (unsigned long long)flock_first->object,
                (unsigned)flock_first->mode);
    return permissive ? 0 : -1;
}

// One admission gate for both domains. Evaluate BOTH before returning so a single
// refused run reports everything an operator must fix, rather than one domain per
// attempt.
static int ckpt_admit_ipc_and_lock_state(void) {
    int permissive = ckpt_recovery_permissive_requested();
    int sysv = ckpt_refuse_uncaptured_sysv_state(permissive);
    int locks = ckpt_refuse_uncaptured_file_locks(permissive);
    return (sysv != 0 || locks != 0) ? -1 : 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)
// Drive the admission gate against each domain in turn. Every scenario installs
// exactly one live object into the real registry it is testing, runs the gate,
// and restores the prior contents, so the hook leaves no state behind for the
// next scenario or for a concurrent engine sharing the same tables.
HL_API int HL_TARGET_LOCAL(checkpoint_ipc_admission_test)(uint32_t scenario) {
    if (scenario == 0) { // an engine holding neither domain is admitted
        return ckpt_admit_ipc_and_lock_state() == 0 ? 0 : 10;
    }
    if (scenario == 1 || scenario == 2 || scenario == 3) { // SysV shm / sem / msg
        struct hl_ipc_ctrl *control = hl_ipc_ctrl();
        if (control == NULL) return 20;
        if (ckpt_admit_ipc_and_lock_state() != 0) return 21; // must start empty
        int verdict;
        if (scenario == 1) {
            struct hl_shm_entry saved = control->shm[0];
            control->shm[0] = (struct hl_shm_entry){0};
            control->shm[0].inuse = 1;
            control->shm[0].perm.key = 0x52654463;
            control->shm[0].segsz = 4096;
            verdict = ckpt_admit_ipc_and_lock_state();
            control->shm[0] = saved;
        } else if (scenario == 2) {
            struct hl_sem_entry saved = control->sem[0];
            memset(&control->sem[0], 0, sizeof control->sem[0]);
            control->sem[0].inuse = 1;
            control->sem[0].perm.key = 0x52654464;
            control->sem[0].nsems = 17;
            verdict = ckpt_admit_ipc_and_lock_state();
            control->sem[0] = saved;
        } else {
            struct hl_msg_queue saved = control->msg[0];
            control->msg[0] = (struct hl_msg_queue){0};
            control->msg[0].inuse = 1;
            control->msg[0].perm.key = 0x52654465;
            verdict = ckpt_admit_ipc_and_lock_state();
            control->msg[0] = saved;
        }
        if (verdict == 0) return 22;                              // silent drop -- the defect
        return ckpt_admit_ipc_and_lock_state() == 0 ? 0 : 23;     // and the gate must re-open
    }
    if (scenario == 4) { // an fcntl record lock held by this process
        if (poslk_init() != 0) return 30;
        if (ckpt_admit_ipc_and_lock_state() != 0) return 31;
        poslk_lock();
        struct poslk_rec *record = poslk_slot();
        int high_water = g_poslk->hi;
        if (record == NULL) {
            poslk_unlock();
            return 32;
        }
        *record = (struct poslk_rec){.device = 7, .object = 99, .lo = 0, .hi = INT64_MAX, .type = F_WRLCK,
                                     .owner = poslk_mypid()};
        poslk_unlock();
        int verdict = ckpt_admit_ipc_and_lock_state();
        poslk_lock();
        *record = (struct poslk_rec){0};
        g_poslk->hi = high_water;
        poslk_unlock();
        if (verdict == 0) return 33;
        return ckpt_admit_ipc_and_lock_state() == 0 ? 0 : 34;
    }
    if (scenario == 5) { // a flock(2) lease held by this process
        if (poslk_init() != 0) return 40;
        if (ckpt_admit_ipc_and_lock_state() != 0) return 41;
        poslk_lock();
        struct flock_broker_record *lease = NULL;
        for (int i = 0; i < FLOCK_BROKER_MAX && lease == NULL; i++)
            if (!g_poslk->flock[i].active) lease = &g_poslk->flock[i];
        if (lease == NULL) {
            poslk_unlock();
            return 42;
        }
        struct flock_broker_record saved = *lease;
        memset(lease, 0, sizeof *lease);
        lease->active = 1;
        lease->device = 7;
        lease->object = 100;
        lease->mode = 2;
        lease->holders[0] = poslk_mypid();
        poslk_unlock();
        int verdict = ckpt_admit_ipc_and_lock_state();
        poslk_lock();
        *lease = saved;
        poslk_unlock();
        if (verdict == 0) return 43;
        return ckpt_admit_ipc_and_lock_state() == 0 ? 0 : 44;
    }
    return 99;
}
#endif
