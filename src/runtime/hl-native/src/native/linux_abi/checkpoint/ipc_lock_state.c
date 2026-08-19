// The two IPC state domains that live outside the guest descriptor table: SysV
// IPC (shm segments, semaphore sets, message queues), which this file CAPTURES
// and RESTORES, and fcntl record locks / flock(2) leases, which the image format
// still does not carry and which therefore still fail closed.
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
//     attached to nothing. Captured below.
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

// ============================================================================================
//  SysV IPC capture and restore
// ============================================================================================
// The registry is container-scoped, not descriptor-scoped, so it is captured as one
// self-contained object per process image ("<procdir>/sysv"):
//
//   header | live shm records | live sem records | live msg records |
//   this process's shmat attachments | this process's SEM_UNDO adjustments |
//   each live segment's payload bytes | each live queue's slot ring
//
// The tables are identical in every process's image (they describe one shared
// control block); the attachment and undo lists are per process, which is exactly
// the split in syscall/sysv_state.h between the MAP_SHARED control block and the
// COW-inherited g_shmat/g_undo caches. g_msgcache is NOT captured: it holds only
// mappings of the per-queue objects and hl_ipc_msg_store() rebuilds it on demand.
//
// ATTACH-ADDRESS FIDELITY. shmat() returns the host mmap result directly to the
// guest (syscall/sysv.c:svc_shmat), so a guest shared-memory address IS a host
// address. PostgreSQL stores absolute pointers into its shared buffers, so an
// attachment restored anywhere else is a corrupt cluster. Restore therefore maps
// each segment with MAP_FIXED at the captured address, AFTER ckpt_restore_mem_dir
// has laid out guest RAM, and verifies mmap returned exactly that address --
// MAP_FIXED can only fail to honour the request by failing outright, so a
// mismatch is treated as unrecoverable and refuses the restore rather than
// resuming a guest whose pointers are wrong.

#define CKPT_SYSV_MAGIC UINT32_C(0x56535948) // "HYSV" (LE)
#define CKPT_SYSV_VERSION UINT32_C(1)
// A capture larger than this cannot be a legitimate container registry and is refused
// rather than allocated: the tables are guest-controlled through shmget/msgsnd.
#define CKPT_SYSV_MAX_IMAGE ((uint64_t)1 << 32)

struct ckpt_sysv_header {
    uint32_t magic, version;
    uint32_t shm_count, sem_count, msg_count;
    uint32_t attach_count, undo_count;
    uint32_t reserved;
};

struct ckpt_sysv_shm_record {
    uint32_t idx, reserved;
    uint64_t payload_bytes; // hl_ipc_pground(segsz); the mapped length
    struct hl_shm_entry entry;
};

struct ckpt_sysv_sem_record {
    uint32_t idx, reserved;
    struct hl_sem_entry entry;
};

struct ckpt_sysv_msg_record {
    uint32_t idx, reserved;
    struct hl_msg_queue entry;
};

struct ckpt_sysv_attach_record {
    uint64_t address;
    uint64_t length;
    uint32_t idx, reserved;
};

struct ckpt_sysv_undo_record {
    uint32_t idx, seq;
    uint32_t semnum;
    int32_t adjustment;
};

// Map one named per-object backing (a segment or a queue store) without creating it.
static void *ckpt_sysv_map_object(const char *name, size_t bytes, int writable) {
    int fd = shm_open(name, writable ? O_RDWR : O_RDONLY, 0600);
    if (fd < 0) return NULL;
    void *mapped = mmap(NULL, bytes, writable ? (PROT_READ | PROT_WRITE) : PROT_READ, MAP_SHARED, fd, 0);
    close(fd);
    return mapped == MAP_FAILED ? NULL : mapped;
}

// Build the whole image in one buffer. Returns 0 and a malloc'd buffer the caller frees,
// or -1 with a named refusal -- the capture never degrades silently.
static int ckpt_sysv_image_build(void **out_image, size_t *out_size) {
    *out_image = NULL;
    *out_size = 0;
    struct hl_ipc_ctrl *control = ckpt_sysv_registry_existing();
    if (control == NULL) return 0; // no registry object == no SysV state
    uint32_t shm_count = 0, sem_count = 0, msg_count = 0, attach_count = 0, undo_count = 0;
    uint64_t payload = 0;
    hl_ipc_lock(&control->lock);
    for (uint32_t i = 0; i < HL_IPC_SHMMNI; i++)
        if (control->shm[i].inuse) {
            shm_count++;
            payload += hl_ipc_pground(control->shm[i].segsz);
        }
    for (uint32_t i = 0; i < HL_IPC_SEMMNI; i++)
        if (control->sem[i].inuse) sem_count++;
    for (uint32_t i = 0; i < HL_IPC_MSGMNI; i++)
        if (control->msg[i].inuse) {
            msg_count++;
            payload += sizeof(struct hl_ipc_msg_store);
        }
    hl_ipc_unlock(&control->lock);
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < HL_SHMAT_MAX; i++)
        if (g_shmat[i].used) attach_count++;
    for (int i = 0; i < HL_UNDO_MAX; i++)
        if (g_undo[i].used) undo_count++;
    pthread_mutex_unlock(&g_ipc_local_m);
    if (shm_count == 0 && sem_count == 0 && msg_count == 0 && attach_count == 0 && undo_count == 0) {
        ckpt_sysv_release(control);
        return 0;
    }
    uint64_t total = sizeof(struct ckpt_sysv_header) + (uint64_t)shm_count * sizeof(struct ckpt_sysv_shm_record) +
                     (uint64_t)sem_count * sizeof(struct ckpt_sysv_sem_record) +
                     (uint64_t)msg_count * sizeof(struct ckpt_sysv_msg_record) +
                     (uint64_t)attach_count * sizeof(struct ckpt_sysv_attach_record) +
                     (uint64_t)undo_count * sizeof(struct ckpt_sysv_undo_record) + payload;
    if (total > CKPT_SYSV_MAX_IMAGE) {
        fprintf(stderr, "[ckpt] refuse: SysV domain -- registry image would be %llu bytes\n",
                (unsigned long long)total);
        ckpt_sysv_release(control);
        return -1;
    }
    uint8_t *image = malloc((size_t)total);
    if (image == NULL) {
        ckpt_sysv_release(control);
        return -1;
    }
    struct ckpt_sysv_header *header = (struct ckpt_sysv_header *)image;
    *header = (struct ckpt_sysv_header){CKPT_SYSV_MAGIC, CKPT_SYSV_VERSION, shm_count, sem_count,
                                        msg_count,       attach_count,      undo_count, 0};
    size_t offset = sizeof *header;
    // The tables are copied under the registry lock; the payload objects are copied after it is
    // released, exactly as svc_shmat does -- every guest thread is stopped by the checkpoint STW,
    // so no guest write can race the copy, and holding the spinlock across a megabyte memcpy would
    // stall any sibling engine sharing the namespace.
    hl_ipc_lock(&control->lock);
    for (uint32_t i = 0; i < HL_IPC_SHMMNI; i++) {
        if (!control->shm[i].inuse) continue;
        struct ckpt_sysv_shm_record record = {i, 0, hl_ipc_pground(control->shm[i].segsz), control->shm[i]};
        memcpy(image + offset, &record, sizeof record);
        offset += sizeof record;
    }
    for (uint32_t i = 0; i < HL_IPC_SEMMNI; i++) {
        if (!control->sem[i].inuse) continue;
        struct ckpt_sysv_sem_record record = {i, 0, control->sem[i]};
        memcpy(image + offset, &record, sizeof record);
        offset += sizeof record;
    }
    for (uint32_t i = 0; i < HL_IPC_MSGMNI; i++) {
        if (!control->msg[i].inuse) continue;
        struct ckpt_sysv_msg_record record = {i, 0, control->msg[i]};
        memcpy(image + offset, &record, sizeof record);
        offset += sizeof record;
    }
    hl_ipc_unlock(&control->lock);
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < HL_SHMAT_MAX; i++) {
        if (!g_shmat[i].used) continue;
        struct ckpt_sysv_attach_record record = {(uint64_t)(uintptr_t)g_shmat[i].addr, (uint64_t)g_shmat[i].len,
                                                 g_shmat[i].idx, 0};
        memcpy(image + offset, &record, sizeof record);
        offset += sizeof record;
    }
    for (int i = 0; i < HL_UNDO_MAX; i++) {
        if (!g_undo[i].used) continue;
        struct ckpt_sysv_undo_record record = {g_undo[i].idx, g_undo[i].seq, g_undo[i].semnum, g_undo[i].adj};
        memcpy(image + offset, &record, sizeof record);
        offset += sizeof record;
    }
    pthread_mutex_unlock(&g_ipc_local_m);
    const struct ckpt_sysv_shm_record *shm_records = (const struct ckpt_sysv_shm_record *)(image + sizeof *header);
    for (uint32_t i = 0; i < shm_count; i++) {
        char name[40];
        hl_ipc_shm_name(name, sizeof name, shm_records[i].idx);
        size_t bytes = (size_t)shm_records[i].payload_bytes;
        void *segment = bytes ? ckpt_sysv_map_object(name, bytes, 0) : NULL;
        if (bytes && segment == NULL) {
            fprintf(stderr, "[ckpt] refuse: SysV domain -- cannot read shared-memory segment %s (%zu bytes): %s\n",
                    name, bytes, strerror(errno));
            free(image);
            ckpt_sysv_release(control);
            return -1;
        }
        if (bytes) {
            memcpy(image + offset, segment, bytes);
            munmap(segment, bytes);
        }
        offset += bytes;
    }
    const struct ckpt_sysv_msg_record *msg_records =
        (const struct ckpt_sysv_msg_record *)(image + sizeof *header +
                                              (size_t)shm_count * sizeof(struct ckpt_sysv_shm_record) +
                                              (size_t)sem_count * sizeof(struct ckpt_sysv_sem_record));
    for (uint32_t i = 0; i < msg_count; i++) {
        char name[40];
        hl_ipc_message_name(name, sizeof name, msg_records[i].idx);
        void *store = ckpt_sysv_map_object(name, sizeof(struct hl_ipc_msg_store), 0);
        if (store == NULL) {
            fprintf(stderr, "[ckpt] refuse: SysV domain -- cannot read message queue %s: %s\n", name,
                    strerror(errno));
            free(image);
            ckpt_sysv_release(control);
            return -1;
        }
        memcpy(image + offset, store, sizeof(struct hl_ipc_msg_store));
        munmap(store, sizeof(struct hl_ipc_msg_store));
        offset += sizeof(struct hl_ipc_msg_store);
    }
    ckpt_sysv_release(control);
    *out_image = image;
    *out_size = (size_t)total;
    return 0;
}

// Drop every attachment and undo record this process inherited. A restored child is forked
// from the restored init, so it inherits the init's g_shmat table and sysv_after_fork() has
// already counted those attachments in nattch; both are undone here before the child installs
// its own captured set, which carries the authoritative nattch in the control tables.
//
// MUST RUN BEFORE ckpt_restore_mem_dir, alongside the other releases of COW-inherited parent
// state (bound_mapping_reset / hl_gmap_reset). The addresses in g_shmat describe the RESTORED
// INIT's address space, not this process's: a member whose own image records no attachment --
// every container exec session, e.g. the psql client holding the far end of a postgres backend
// socket -- still inherits the init's two SysV ranges, and munmapping them AFTER the memory
// restore deletes whatever that member's own captured image legitimately mapped at those VAs.
// Observed on the postgres acceptance fixture: four members reported inherited_attach=2 with
// image_attach=0, and the restored cluster then lost a backend ("exited with exit code 0" /
// SIGSEGV) followed by a full postmaster crash-recovery cycle.
static void ckpt_sysv_detach_inherited(void) {
    pthread_mutex_lock(&g_ipc_local_m);
    for (int i = 0; i < HL_SHMAT_MAX; i++) {
        if (!g_shmat[i].used) continue;
        if (g_ctrl != NULL) {
            hl_ipc_lock(&g_ctrl->lock);
            struct hl_shm_entry *entry = &g_ctrl->shm[g_shmat[i].idx];
            if (entry->inuse && entry->nattch) entry->nattch--;
            hl_ipc_unlock(&g_ctrl->lock);
        }
        munmap(g_shmat[i].addr, g_shmat[i].len);
        g_shmat[i].used = 0;
    }
    memset(g_undo, 0, sizeof g_undo);
    pthread_mutex_unlock(&g_ipc_local_m);
    for (int i = 0; i < HL_MSGCACHE_MAX; i++)
        if (g_msgcache[i].used) {
            munmap(g_msgcache[i].p, sizeof(struct hl_ipc_msg_store));
            g_msgcache[i].used = 0;
        }
}

// Create one per-object backing under the CURRENT namespace and seed it with captured bytes.
static int ckpt_sysv_publish_object(const char *name, const void *bytes, size_t size) {
    shm_unlink(name); // a stale object from an aborted restore must not be adopted
    int fd = shm_open(name, O_CREAT | O_EXCL | O_RDWR, 0600);
    if (fd < 0) return -1;
    if (ftruncate(fd, (off_t)size) < 0) {
        close(fd);
        shm_unlink(name);
        return -1;
    }
    void *mapped = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (mapped == MAP_FAILED) {
        shm_unlink(name);
        return -1;
    }
    memcpy(mapped, bytes, size);
    munmap(mapped, size);
    return 0;
}

static int ckpt_sysv_image_apply(const void *image_bytes, size_t size) {
    const uint8_t *image = (const uint8_t *)image_bytes;
    if (size < sizeof(struct ckpt_sysv_header)) return -1;
    struct ckpt_sysv_header header;
    memcpy(&header, image, sizeof header);
    if (header.magic != CKPT_SYSV_MAGIC || header.version != CKPT_SYSV_VERSION) {
        fprintf(stderr, "[restore] SysV image header is not recognised (magic %08x version %u)\n", header.magic,
                header.version);
        return -1;
    }
    if (header.shm_count > HL_IPC_SHMMNI || header.sem_count > HL_IPC_SEMMNI || header.msg_count > HL_IPC_MSGMNI ||
        header.attach_count > HL_SHMAT_MAX || header.undo_count > HL_UNDO_MAX) {
        fprintf(stderr, "[restore] SysV image exceeds the registry capacities\n");
        return -1;
    }
    size_t shm_off = sizeof header;
    size_t sem_off = shm_off + (size_t)header.shm_count * sizeof(struct ckpt_sysv_shm_record);
    size_t msg_off = sem_off + (size_t)header.sem_count * sizeof(struct ckpt_sysv_sem_record);
    size_t attach_off = msg_off + (size_t)header.msg_count * sizeof(struct ckpt_sysv_msg_record);
    size_t undo_off = attach_off + (size_t)header.attach_count * sizeof(struct ckpt_sysv_attach_record);
    size_t payload_off = undo_off + (size_t)header.undo_count * sizeof(struct ckpt_sysv_undo_record);
    if (payload_off > size) {
        fprintf(stderr, "[restore] SysV image is truncated\n");
        return -1;
    }
    const struct ckpt_sysv_shm_record *shm_records = (const struct ckpt_sysv_shm_record *)(image + shm_off);
    const struct ckpt_sysv_sem_record *sem_records = (const struct ckpt_sysv_sem_record *)(image + sem_off);
    const struct ckpt_sysv_msg_record *msg_records = (const struct ckpt_sysv_msg_record *)(image + msg_off);
    const struct ckpt_sysv_attach_record *attach_records =
        (const struct ckpt_sysv_attach_record *)(image + attach_off);
    const struct ckpt_sysv_undo_record *undo_records = (const struct ckpt_sysv_undo_record *)(image + undo_off);
    uint64_t payload = 0;
    for (uint32_t i = 0; i < header.shm_count; i++) {
        if (shm_records[i].idx >= HL_IPC_SHMMNI) return -1;
        payload += shm_records[i].payload_bytes;
    }
    for (uint32_t i = 0; i < header.sem_count; i++)
        if (sem_records[i].idx >= HL_IPC_SEMMNI || sem_records[i].entry.nsems > HL_IPC_SEMMSL) return -1;
    for (uint32_t i = 0; i < header.msg_count; i++) {
        if (msg_records[i].idx >= HL_IPC_MSGMNI) return -1;
        payload += sizeof(struct hl_ipc_msg_store);
    }
    if (payload > size - payload_off) {
        fprintf(stderr, "[restore] SysV image payload is truncated\n");
        return -1;
    }

    // The inherited attachments were already released before the memory restore
    // (ckpt_sysv_detach_inherited at the COW-inheritance reset); this is idempotent for the init,
    // which inherits none, and must never run after the memory image has been laid down.
    ckpt_sysv_detach_inherited();
    struct hl_ipc_ctrl *control = hl_ipc_ctrl(); // creates it under the NEW namespace hash when absent
    if (control == NULL) {
        fprintf(stderr, "[restore] cannot open the restored SysV control block\n");
        return -1;
    }
    // Only the process that created the namespace object republishes the tables and the
    // per-object backings; every other restored process inherits that one mapping and restores
    // just its own attachments and undo list.
    size_t offset = payload_off;
    if (g_ipc_creator) {
        hl_ipc_lock(&control->lock);
        memset(control->shm, 0, sizeof control->shm);
        memset(control->sem, 0, sizeof control->sem);
        memset(control->msg, 0, sizeof control->msg);
        for (uint32_t i = 0; i < header.shm_count; i++) control->shm[shm_records[i].idx] = shm_records[i].entry;
        for (uint32_t i = 0; i < header.sem_count; i++) control->sem[sem_records[i].idx] = sem_records[i].entry;
        for (uint32_t i = 0; i < header.msg_count; i++) control->msg[msg_records[i].idx] = msg_records[i].entry;
        hl_ipc_unlock(&control->lock);
        for (uint32_t i = 0; i < header.shm_count; i++) {
            char name[40];
            hl_ipc_shm_name(name, sizeof name, shm_records[i].idx);
            size_t bytes = (size_t)shm_records[i].payload_bytes;
            if (bytes && ckpt_sysv_publish_object(name, image + offset, bytes) != 0) {
                fprintf(stderr, "[restore] cannot republish shared-memory segment %s: %s\n", name, strerror(errno));
                return -1;
            }
            offset += bytes;
        }
        for (uint32_t i = 0; i < header.msg_count; i++) {
            char name[40];
            hl_ipc_message_name(name, sizeof name, msg_records[i].idx);
            if (ckpt_sysv_publish_object(name, image + offset, sizeof(struct hl_ipc_msg_store)) != 0) {
                fprintf(stderr, "[restore] cannot republish message queue %s: %s\n", name, strerror(errno));
                return -1;
            }
            offset += sizeof(struct hl_ipc_msg_store);
        }
    }
    for (uint32_t i = 0; i < header.attach_count; i++) {
        const struct ckpt_sysv_attach_record *record = &attach_records[i];
        if (record->idx >= HL_IPC_SHMMNI || record->length == 0) return -1;
        char name[40];
        hl_ipc_shm_name(name, sizeof name, record->idx);
        int fd = shm_open(name, O_RDWR, 0600);
        if (fd < 0) {
            fprintf(stderr, "[restore] SysV segment %s is missing for an attachment at 0x%llx: %s\n", name,
                    (unsigned long long)record->address, strerror(errno));
            return -1;
        }
        void *mapped = mmap((void *)(uintptr_t)record->address, (size_t)record->length, PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_FIXED, fd, 0);
        close(fd);
        if (mapped != (void *)(uintptr_t)record->address) {
            // Guests store absolute pointers into shared memory; a different address is a corrupt
            // guest, not a degraded one, so this refuses instead of relocating the attachment.
            fprintf(stderr, "[restore] SysV segment %s could not be re-attached at its captured address 0x%llx\n",
                    name, (unsigned long long)record->address);
            if (mapped != MAP_FAILED) munmap(mapped, (size_t)record->length);
            return -1;
        }
        pthread_mutex_lock(&g_ipc_local_m);
        int slot = -1;
        for (int s = 0; s < HL_SHMAT_MAX && slot < 0; s++)
            if (!g_shmat[s].used) slot = s;
        if (slot >= 0) {
            g_shmat[slot].used = 1;
            g_shmat[slot].addr = mapped;
            g_shmat[slot].idx = record->idx;
            g_shmat[slot].len = (size_t)record->length;
        }
        pthread_mutex_unlock(&g_ipc_local_m);
        if (slot < 0) {
            fprintf(stderr, "[restore] SysV attachment table overflow restoring segment %s\n", name);
            return -1;
        }
    }
    pthread_mutex_lock(&g_ipc_local_m);
    for (uint32_t i = 0; i < header.undo_count && i < HL_UNDO_MAX; i++) {
        g_undo[i].used = 1;
        g_undo[i].idx = undo_records[i].idx;
        g_undo[i].seq = undo_records[i].seq;
        g_undo[i].semnum = (uint16_t)undo_records[i].semnum;
        g_undo[i].adj = undo_records[i].adjustment;
    }
    pthread_mutex_unlock(&g_ipc_local_m);
    return 0;
}

// Capture hook for ckpt_dump_self_locked: writes "<group>/sysv" when the container holds
// any SysV state, and refuses the dump when the registry cannot be read.
static int ckpt_sysv_capture(struct ckpt_sink *sink, const char *group) {
    void *image = NULL;
    size_t size = 0;
    if (ckpt_sysv_image_build(&image, &size) != 0) return -1;
    if (image == NULL) return 0;
    int rc = ckpt_sink_put(sink, group, "sysv", 0, image, size);
    free(image);
    return rc;
}

// Restore hook: absent object == the container held no SysV state.
static int ckpt_restore_sysv_state(const char *procdir) {
    char path[1300];
    snprintf(path, sizeof path, "%s/sysv", procdir);
    int64_t stored = ckpt_source_object_size(path);
    if (stored <= 0) return 0; // absent or empty == the container held no SysV state
    if ((uint64_t)stored > CKPT_SYSV_MAX_IMAGE) return -1;
    void *image = malloc((size_t)stored);
    if (image == NULL) return -1;
    if (ckpt_source_load(path, image, (size_t)stored) != 0) {
        free(image);
        return -1;
    }
    int rc = ckpt_sysv_image_apply(image, (size_t)stored);
    free(image);
    return rc;
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

// The remaining admission gate. SysV is captured, so only the lock domain can
// refuse; the gate keeps its name and its call site so the next domain to be
// captured is removed from here rather than from ckpt_dump_self_locked.
static int ckpt_admit_ipc_and_lock_state(void) {
    int permissive = ckpt_recovery_permissive_requested();
    return ckpt_refuse_uncaptured_file_locks(permissive) != 0 ? -1 : 0;
}

#if defined(HL_NATIVE_TEST_HOOKS)

// Capture and restore the SysV registry for real: install one live object, capture the image,
// tear the namespace down, then re-create it under a DIFFERENT namespace hash (which is what a
// restore into a fresh container does) and apply the image. Scenario 1 additionally checks the
// property PostgreSQL depends on -- the segment comes back at its ORIGINAL attach address with
// its original bytes. Every scenario restores the process's prior namespace bindings.
static int ckpt_sysv_roundtrip_test(uint32_t scenario) {
    struct hl_ipc_ctrl *control = hl_ipc_ctrl();
    if (control == NULL) return 20;
    for (int i = 0; i < HL_SHMAT_MAX; i++)
        if (g_shmat[i].used) return 21; // the hook owns the attach table for the duration
    uint32_t idx = UINT32_MAX;
    uint32_t limit = scenario == 1 ? HL_IPC_SHMMNI : (scenario == 2 ? HL_IPC_SEMMNI : HL_IPC_MSGMNI);
    for (uint32_t i = 0; i < limit && idx == UINT32_MAX; i++) {
        int used = scenario == 1 ? control->shm[i].inuse : (scenario == 2 ? control->sem[i].inuse : control->msg[i].inuse);
        if (!used) idx = i;
    }
    if (idx == UINT32_MAX) return 22;

    size_t length = hl_ipc_pground(1);
    void *address = NULL;
    char name[40];
    if (scenario == 1) {
        hl_ipc_shm_name(name, sizeof name, idx);
        uint8_t *seed = calloc(1, length);
        if (seed == NULL) return 23;
        int published = ckpt_sysv_publish_object(name, seed, length);
        free(seed);
        if (published != 0) return 24;
        int fd = shm_open(name, O_RDWR, 0600);
        if (fd < 0) {
            shm_unlink(name);
            return 25;
        }
        address = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        close(fd);
        if (address == MAP_FAILED) {
            shm_unlink(name);
            return 26;
        }
        memset(address, 0xa5, length);
        memcpy(address, "HLSYSVPATTERN", 13);
        hl_ipc_lock(&control->lock);
        control->shm[idx] = (struct hl_shm_entry){0};
        control->shm[idx].inuse = 1;
        control->shm[idx].segsz = (uint64_t)length;
        control->shm[idx].nattch = 1;
        control->shm[idx].perm.key = 0x52654463;
        control->shm[idx].perm.mode = 0600;
        hl_ipc_unlock(&control->lock);
        g_shmat[0].used = 1;
        g_shmat[0].addr = address;
        g_shmat[0].idx = idx;
        g_shmat[0].len = length;
    } else if (scenario == 2) {
        hl_ipc_lock(&control->lock);
        memset(&control->sem[idx], 0, sizeof control->sem[idx]);
        control->sem[idx].inuse = 1;
        control->sem[idx].nsems = 3;
        control->sem[idx].perm.key = 0x52654464;
        control->sem[idx].val[0] = 7;
        control->sem[idx].val[1] = 8;
        control->sem[idx].val[2] = 9;
        hl_ipc_unlock(&control->lock);
        g_undo[0].used = 1;
        g_undo[0].idx = idx;
        g_undo[0].seq = control->sem[idx].perm.seq;
        g_undo[0].semnum = 1;
        g_undo[0].adj = 5;
    } else {
        hl_ipc_message_name(name, sizeof name, idx);
        struct hl_ipc_msg_store *seed = calloc(1, sizeof *seed);
        if (seed == NULL) return 23;
        atomic_store(&seed->magic, HL_MSG_MAGIC);
        seed->head = 0;
        seed->tail = 0;
        seed->freehead = 1;
        seed->slots[0].mtype = 42;
        seed->slots[0].size = 5;
        seed->slots[0].next = -1;
        memcpy(seed->slots[0].data, "queue", 5);
        int published = ckpt_sysv_publish_object(name, seed, sizeof *seed);
        free(seed);
        if (published != 0) return 24;
        hl_ipc_lock(&control->lock);
        control->msg[idx] = (struct hl_msg_queue){0};
        control->msg[idx].inuse = 1;
        control->msg[idx].perm.key = 0x52654465;
        control->msg[idx].qnum = 1;
        control->msg[idx].cbytes = 5;
        hl_ipc_unlock(&control->lock);
    }

    void *image = NULL;
    size_t size = 0;
    int built = ckpt_sysv_image_build(&image, &size);

    // Tear the captured namespace down exactly as an exiting container would.
    if (scenario == 1) {
        g_shmat[0].used = 0;
        munmap(address, length);
    }
    memset(g_undo, 0, sizeof g_undo);
    hl_ipc_lock(&control->lock);
    if (scenario == 1) control->shm[idx] = (struct hl_shm_entry){0};
    else if (scenario == 2) memset(&control->sem[idx], 0, sizeof control->sem[idx]);
    else control->msg[idx] = (struct hl_msg_queue){0};
    hl_ipc_unlock(&control->lock);
    if (scenario != 2) shm_unlink(name);

    uint32_t saved_hash = g_ns_hash;
    struct hl_ipc_ctrl *saved_control = g_ctrl;
    int saved_creator = g_ipc_creator;
    g_ns_hash = saved_hash ^ 0x5a5a5a5au;
    if (g_ns_hash == 0 || g_ns_hash == saved_hash) g_ns_hash = saved_hash + 1u;
    g_ctrl = NULL;
    g_ipc_creator = 0;

    int verdict = 0;
    if (built != 0 || image == NULL) verdict = 30;
    else if (ckpt_sysv_image_apply(image, size) != 0) verdict = 31;
    else if (g_ctrl == NULL) verdict = 32;
    else if (scenario == 1) {
        if (!g_shmat[0].used || g_shmat[0].addr != address) verdict = 33; // attach-address fidelity
        else if (memcmp(address, "HLSYSVPATTERN", 13) != 0 || ((const uint8_t *)address)[length - 1] != 0xa5)
            verdict = 34;
        else if (!g_ctrl->shm[idx].inuse || g_ctrl->shm[idx].segsz != (uint64_t)length ||
                 g_ctrl->shm[idx].perm.key != 0x52654463)
            verdict = 35;
    } else if (scenario == 2) {
        if (!g_ctrl->sem[idx].inuse || g_ctrl->sem[idx].nsems != 3 || g_ctrl->sem[idx].val[0] != 7 ||
            g_ctrl->sem[idx].val[2] != 9)
            verdict = 36;
        else if (!g_undo[0].used || g_undo[0].idx != idx || g_undo[0].semnum != 1 || g_undo[0].adj != 5)
            verdict = 37;
    } else {
        char restored_name[40];
        hl_ipc_message_name(restored_name, sizeof restored_name, idx);
        struct hl_ipc_msg_store *store =
            (struct hl_ipc_msg_store *)ckpt_sysv_map_object(restored_name, sizeof *store, 0);
        if (!g_ctrl->msg[idx].inuse || g_ctrl->msg[idx].qnum != 1) verdict = 38;
        else if (store == NULL) verdict = 39;
        else if (store->slots[0].mtype != 42 || memcmp(store->slots[0].data, "queue", 5) != 0) verdict = 40;
        if (store != NULL) munmap(store, sizeof *store);
    }

    // Drop the restored namespace: its objects live under the synthetic hash and nothing else uses them.
    for (int i = 0; i < HL_SHMAT_MAX; i++)
        if (g_shmat[i].used) {
            munmap(g_shmat[i].addr, g_shmat[i].len);
            g_shmat[i].used = 0;
        }
    if (g_ctrl != NULL) {
        char victim[40];
        for (uint32_t i = 0; i < HL_IPC_SHMMNI; i++)
            if (g_ctrl->shm[i].inuse) {
                hl_ipc_shm_name(victim, sizeof victim, i);
                shm_unlink(victim);
            }
        for (uint32_t i = 0; i < HL_IPC_MSGMNI; i++)
            if (g_ctrl->msg[i].inuse) {
                hl_ipc_message_name(victim, sizeof victim, i);
                shm_unlink(victim);
            }
        if (g_ctrl != saved_control) munmap(g_ctrl, sizeof *g_ctrl);
    }
    char control_name[40];
    hl_ipc_control_name(control_name, sizeof control_name);
    shm_unlink(control_name);
    memset(g_undo, 0, sizeof g_undo);
    g_ns_hash = saved_hash;
    g_ctrl = saved_control;
    g_ipc_creator = saved_creator;
    free(image);
    return verdict;
}

// Drive the admission gate against each domain in turn. Every scenario installs
// exactly one live object into the real registry it is testing, runs the gate,
// and restores the prior contents, so the hook leaves no state behind for the
// next scenario or for a concurrent engine sharing the same tables.
HL_API int HL_TARGET_LOCAL(checkpoint_ipc_admission_test)(uint32_t scenario) {
    if (scenario == 0) { // an engine holding neither domain is admitted
        return ckpt_admit_ipc_and_lock_state() == 0 ? 0 : 10;
    }
    if (scenario == 1 || scenario == 2 || scenario == 3) return ckpt_sysv_roundtrip_test(scenario);
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
