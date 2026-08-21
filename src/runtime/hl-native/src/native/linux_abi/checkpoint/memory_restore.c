#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_vm.h>
#endif

static int ckpt_read_manifest(struct ckpt_manifest *man) {
    if (ckpt_source_load("MANIFEST", man, sizeof *man) != 0) {
        fprintf(stderr, "[restore] the store has no MANIFEST (not a complete checkpoint)\n");
        return -1;
    }
    if (man->magic != CKPT_MANIFEST_MAGIC) {
        fprintf(stderr, "[restore] bad manifest magic\n");
        return -1;
    }
    if (man->version != CKPT_VERSION || man->arch != G_CKPT_ARCH) {
        fprintf(stderr, "[restore] manifest version/arch mismatch\n");
        return -1;
    }
    uint64_t image_hash, image_files, image_bytes;
    if (ckpt_source_digest(&image_hash, &image_files, &image_bytes) != 0 || image_hash != man->image_hash ||
        image_files != man->image_files || image_bytes != man->image_bytes) {
        fprintf(stderr, "[restore] checkpoint image integrity mismatch\n");
        return -1;
    }
    if (man->n_procs == 0 || man->n_procs > 512 || man->root_gpid != 1) {
        fprintf(stderr, "[restore] invalid manifest process count/root\n");
        return -1;
    }
    return 0;
}

static int ckpt_read_meta_dir(const char *procdir, struct ckpt_meta *m) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/meta", procdir);
    if (ckpt_source_load(pf, m, sizeof *m) != 0) {
        // Every failure path of ckpt_source_load is an image-protocol answer -- no source installed, a
        // size the host would not give, or a short read -- and none of them sets errno.
        fprintf(stderr, "[restore] image object %s is absent or shorter than the %zu bytes it must carry\n", pf,
                sizeof *m);
        return -1;
    }
    if (m->magic != CKPT_MAGIC) {
        fprintf(stderr, "[restore] %s is not a checkpoint (bad magic/short read)\n", procdir);
        return -1;
    }
    if (m->version != CKPT_VERSION || m->arch != G_CKPT_ARCH) {
        fprintf(stderr, "[restore] version/arch mismatch (file v%llu arch %llu)\n", (unsigned long long)m->version,
                (unsigned long long)m->arch);
        return -1;
    }
    hl_identity_digest expected_engine = pcache_translator_identity();
    if (!hl_identity_digest_equal(&m->engine_identity, &expected_engine)) {
        fprintf(stderr, "[restore] translator identity mismatch\n");
        return -1;
    }
    if (m->cpu_sz != sizeof(struct cpu)) {
        fprintf(stderr, "[restore] cpu-struct size mismatch (file %llu, expected %zu)\n", (unsigned long long)m->cpu_sz,
                sizeof(struct cpu));
        return -1;
    }
    if (m->n_threads < 1 || m->n_threads > THREAD_REG_MAX) {
        fprintf(stderr, "[restore] invalid checkpoint thread count %llu\n", (unsigned long long)m->n_threads);
        return -1;
    }
    if (memchr(m->exe_path, 0, sizeof m->exe_path) == NULL) {
        fprintf(stderr, "[restore] invalid process executable path\n");
        return -1;
    }
    return 0;
}

struct ckpt_restore_backing {
    uint64_t object_id;
    int fd;
    int expandable;
};
static struct ckpt_restore_backing *g_restore_backings;
static int g_nrestore_backings;
static int g_restore_backings_capacity;

static int ckpt_vector_reserve(void **items, int *capacity, size_t item_size, int needed) {
    if (needed <= *capacity) return 0;
    int expanded = *capacity > 0 ? *capacity : 64;
    while (expanded < needed) {
        if (expanded > INT_MAX / 2) return -1;
        expanded *= 2;
    }
    if ((size_t)expanded > SIZE_MAX / item_size) return -1;
    void *replacement = realloc(*items, (size_t)expanded * item_size);
    if (replacement == NULL) return -1;
    *items = replacement;
    *capacity = expanded;
    return 0;
}

// Materialize an image object into `destination`. The blob and memfd seeds need a real descriptor, and the
// object only exists in the embedder's store.
static int ckpt_source_copy_to_fd(const char *name, int destination) {
    FILE *source = ckpt_source_fopen(name);
    unsigned char buffer[65536];
    size_t count;
    int failed = 0;
    if (source == NULL) return -1;
    while (!failed && (count = fread(buffer, 1, sizeof buffer, source)) != 0) {
        size_t offset = 0;
        while (offset < count) {
            ssize_t written = write(destination, buffer + offset, count - offset);
            if (written > 0) {
                offset += (size_t)written;
                continue;
            }
            if (written < 0 && errno == EINTR) continue;
            failed = 1;
            break;
        }
    }
    if (ferror(source)) failed = 1;
    ckpt_source_fclose(source);
    return failed ? -1 : 0;
}

static int ckpt_restore_backing_seed(const char *procdir, uint64_t object_id, uint64_t minimum_size) {
    for (int i = 0; i < g_nrestore_backings; i++)
        if (g_restore_backings[i].object_id == object_id) {
            if (g_restore_backings[i].expandable) {
                struct stat status;
                if (minimum_size > (uint64_t)INT64_MAX || fstat(g_restore_backings[i].fd, &status) != 0 ||
                    ((uint64_t)status.st_size < minimum_size &&
                     ftruncate(g_restore_backings[i].fd, (off_t)minimum_size) != 0))
                    return -1;
            }
            return g_restore_backings[i].fd;
        }
    if (ckpt_vector_reserve((void **)&g_restore_backings, &g_restore_backings_capacity, sizeof *g_restore_backings,
                            g_nrestore_backings + 1) != 0)
        return -1;
    char records_path[1300];
    snprintf(records_path, sizeof records_path, "%s/fds", procdir);
    FILE *records = ckpt_source_fopen(records_path);
    if (!records) return -1;
    struct ckpt_fd record;
    int found = 0;
    int expandable = 0;
    while (ckpt_rd_fd(records, &record) == 0)
        if (record.object_id == object_id &&
            (record.kind == CKF_FILE || record.kind == CKF_BLOB || record.kind == CKF_MEMFD)) {
            found = 1;
            break;
        }
    ckpt_source_fclose(records);
    int fd = -1;
    if (!found) {
        /*
         * mmap keeps a vnode alive after its guest descriptor is closed.
         * Such a backing has no fd record, but the sparse page stream still
         * contains every mapped byte needed for restoration.  Recreate a
         * private anonymous seed now; later regions with the same object id
         * reuse it and therefore recover alias topology.
         */
        char temporary[] = "/tmp/.hl-restore-mapXXXXXX";
        fd = mkstemp(temporary);
        if (fd >= 0) unlink(temporary);
        if (fd < 0 || minimum_size > (uint64_t)INT64_MAX || ftruncate(fd, (off_t)minimum_size) != 0) {
            if (fd >= 0) close(fd);
            return -1;
        }
        expandable = 1;
    } else if (record.kind == CKF_FILE) {
        fd = open(record.path, O_RDWR);
        if (fd < 0) fd = open(record.path, O_RDONLY);
    } else {
        char temporary[] = "/tmp/.hl-restore-mapXXXXXX";
        fd = mkstemp(temporary);
        if (fd >= 0) unlink(temporary);
        if (fd < 0 || ckpt_source_copy_to_fd(record.path, fd) != 0 || lseek(fd, 0, SEEK_SET) < 0) {
            if (fd >= 0) close(fd);
            return -1;
        }
    }
    int private_fd = fd >= 0 ? hl_host_process_fd_private_adopt(fd) : -1;
    if (private_fd < 0) {
        if (fd >= 0) close(fd);
        return -1;
    }
    fd = private_fd;
    g_restore_backings[g_nrestore_backings++] = (struct ckpt_restore_backing){object_id, fd, expandable};
    return fd;
}

// Materialise an ANONYMOUS MAP_SHARED region's object for the restored generation.
//
// It cannot use ckpt_restore_backing_seed's fallback (an mkstemp file, recorded in this process's
// g_restore_backings): that table is per process, and a member is forked BEFORE its parent restores
// its own memory (ckpt_restore_proc_run calls ckpt_fork_children ahead of ckpt_restore_mem_dir), so
// the descriptor cannot be relied on to reach the other sharers. Each member would create its own
// file and get a private copy -- the same defect, moved.
//
// So the object is republished by NAME under the CURRENT restore generation, exactly as SysV segments
// are (ipc_state.c): whichever member arrives first creates it, every other member opens the same
// name, and all of them map it MAP_SHARED at their captured address. Fork order stops mattering.
//
// THE NAME IS A PURE FUNCTION OF (restore generation, object id), AND THE GENERATION IS THE POINT.
// The old name was (ipc_ns(), object_id) and BOTH halves recycle: ipc_ns() hashes a host pid, and a
// kernel object id -- a Darwin vm_object id, a Linux shmem inode number -- is handed out again freely
// once the object is gone. A segment left behind by a crashed earlier restore could therefore be
// opened by a later one under the very same name. When the leftover was too small the restore failed
// (`ftruncate` EINVAL on Darwin, where a POSIX shm object may be sized only once and only by its
// creator); when it was large enough the restore MAPPED IT INSTEAD OF THE CAPTURED BYTES and the guest
// silently resumed on stale memory. That second outcome is the one this discipline exists to make
// unreachable: a restore may only ever open a name its OWN generation minted, so adoption of a
// foreign segment is not a race that is won but a name that does not exist.
#define CKPT_ANON_SHARED_UNLINK_MAX 64
#define CKPT_ANON_SHARED_NAME_MAX 26
static char g_anon_shared_unlink[CKPT_ANON_SHARED_UNLINK_MAX][CKPT_ANON_SHARED_NAME_MAX];
static int g_nanon_shared_unlink;
static uint64_t g_anon_shared_generation;

// Mint the generation ONCE, EAGERLY, BEFORE any member is forked; every member inherits it across
// fork. Minting it lazily would be a sharing bug rather than a naming one: a member whose first
// anonymous-shared object is needed after the fork would mint a generation of its own, derive a
// different name from the same object id, and quietly stop sharing with the members it is supposed to
// share with -- the private-copy defect this whole path exists to prevent, reintroduced by the fix.
static void ckpt_anon_shared_generation_init(void) {
    if (g_anon_shared_generation != 0) return;
    uint64_t minted = 0;
    arc4random_buf(&minted, sizeof minted);
    g_anon_shared_generation = minted ? minted : 1;
}

// Darwin caps a POSIX shm name at 31 bytes (PSHMNAMLEN), so the 128 bits of identity are emitted in a
// 64-character alphabet (22 characters) rather than as hex (32, which does not fit).
static void ckpt_anon_shared_name(uint64_t object_id, char out[CKPT_ANON_SHARED_NAME_MAX]) {
    static const char alphabet[] = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz_-";
    out[0] = '/';
    out[1] = 'h';
    out[2] = 'l';
    for (int i = 0; i < 11; i++) {
        out[3 + i] = alphabet[(g_anon_shared_generation >> (i * 6)) & 63u];
        out[14 + i] = alphabet[(object_id >> (i * 6)) & 63u];
    }
    out[25] = '\0';
}

// Every process that CREATES OR OPENS a name registers it, not only the creator. The name is derivable
// by any member, so unlinking it needs no privileged owner -- and that is what closes the `_exit` hole:
// the member that created a segment may leave through `_exit` (a restore-commit failure does exactly
// that, and so does the fork-round-trip fixture), which runs no `atexit` handler, but every other
// sharer of that object holds the same name and unlinks it on its own way out. Ownership lives in the
// name, which survives an `_exit` because it never lived in the exiting process to begin with.
static void ckpt_anon_shared_unlink_all(void) {
    for (int index = 0; index < g_nanon_shared_unlink; index++) shm_unlink(g_anon_shared_unlink[index]);
    g_nanon_shared_unlink = 0;
}

static void ckpt_anon_shared_unlink_register(const char *name) {
    for (int index = 0; index < g_nanon_shared_unlink; index++)
        if (strcmp(g_anon_shared_unlink[index], name) == 0) return;
    if (g_nanon_shared_unlink >= CKPT_ANON_SHARED_UNLINK_MAX) return;
    if (g_nanon_shared_unlink == 0) (void)atexit(ckpt_anon_shared_unlink_all);
    snprintf(g_anon_shared_unlink[g_nanon_shared_unlink++], CKPT_ANON_SHARED_NAME_MAX, "%s", name);
}

// How long an opener will wait for the creator to publish the object's size. A restore that cannot see
// the agreed size fails; it never proceeds on a short object, and never resizes one it did not create.
#define CKPT_ANON_SHARED_SIZE_WAIT_US 2000000

static int ckpt_restore_anon_shared_seed(uint64_t object_id, uint64_t minimum_size) {
    for (int i = 0; i < g_nrestore_backings; i++)
        if (g_restore_backings[i].object_id == object_id) {
            struct stat status;
            if (minimum_size > (uint64_t)INT64_MAX || fstat(g_restore_backings[i].fd, &status) != 0 ||
                ((uint64_t)status.st_size < minimum_size &&
                 ftruncate(g_restore_backings[i].fd, (off_t)minimum_size) != 0))
                return -1;
            return g_restore_backings[i].fd;
        }
    if (minimum_size > (uint64_t)INT64_MAX) return -1;
    if (ckpt_vector_reserve((void **)&g_restore_backings, &g_restore_backings_capacity, sizeof *g_restore_backings,
                            g_nrestore_backings + 1) != 0)
        return -1;
    // A generation that is still zero here means no pre-fork mint ran. Refuse rather than mint one
    // now: a name minted after the fork is a name no sibling can derive, and the restore would come up
    // with private copies of memory the guest believes is shared.
    if (g_anon_shared_generation == 0) {
        fprintf(stderr, "[restore] refuse: anonymous shared object %llx has no restore generation\n",
                (unsigned long long)object_id);
        errno = EINVAL;
        return -1;
    }
    char name[CKPT_ANON_SHARED_NAME_MAX];
    ckpt_anon_shared_name(object_id, name);
    int created = 0;
    int fd = shm_open(name, O_CREAT | O_EXCL | O_RDWR, 0600);
    if (fd >= 0)
        created = 1;
    else if (errno == EEXIST)
        fd = shm_open(name, O_RDWR, 0600);
    if (fd < 0) return -1;
    ckpt_anon_shared_unlink_register(name);
    // ONLY THE CREATOR SIZES THE OBJECT. Darwin permits `ftruncate` on a POSIX shm object exactly once,
    // by its creator; a second call returns EINVAL. An opener therefore waits for the creator's size
    // instead of setting it, and a wait that times out is a restore failure, not a short mapping.
    struct stat status;
    if (created) {
        if (fstat(fd, &status) != 0 || ((uint64_t)status.st_size < minimum_size &&
                                        ftruncate(fd, (off_t)minimum_size) != 0)) {
            int failure = errno;
            close(fd);
            shm_unlink(name);
            errno = failure;
            return -1;
        }
    } else {
        int sized = 0;
        for (unsigned waited = 0; waited <= CKPT_ANON_SHARED_SIZE_WAIT_US; waited += 200) {
            if (fstat(fd, &status) != 0) break;
            if ((uint64_t)status.st_size >= minimum_size) {
                sized = 1;
                break;
            }
            usleep(200);
        }
        if (!sized) {
            fprintf(stderr, "[restore] anonymous shared object %llx never reached %llu bytes\n",
                    (unsigned long long)object_id, (unsigned long long)minimum_size);
            close(fd);
            errno = EINVAL;
            return -1;
        }
    }
    int private_fd = hl_host_process_fd_private_adopt(fd);
    if (private_fd < 0) {
        close(fd);
        return -1;
    }
    g_restore_backings[g_nrestore_backings++] = (struct ckpt_restore_backing){object_id, private_fd, 1};
    return private_fd;
}

static int ckpt_restore_backing_find(uint64_t object_id) {
    for (int i = 0; i < g_nrestore_backings; i++)
        if (g_restore_backings[i].object_id == object_id) return g_restore_backings[i].fd;
    return -1;
}

static void ckpt_restore_backings_close(void) {
    for (int i = 0; i < g_nrestore_backings; i++) {
        hl_host_process_fd_private_remove(g_restore_backings[i].fd);
        close(g_restore_backings[i].fd);
    }
    g_nrestore_backings = 0;
}

// Name whatever already holds [lo, hi). Only reached from the collision path below, where "what is in the
// way" is the whole question and a bare address answers none of it.
static void ckpt_report_overlap(uint64_t lo, uint64_t hi) {
#if defined(__APPLE__)
    /* Darwin has no /proc/self/maps, so the portable reader below names nothing here -- the collision
     * diagnostic was silent on exactly the host whose restores collide. Walk the Mach VM map instead. */
    mach_vm_address_t address = (mach_vm_address_t)lo;
    while (address < (mach_vm_address_t)hi) {
        mach_vm_size_t size = 0;
        vm_region_basic_info_data_64_t info;
        mach_msg_type_number_t count = VM_REGION_BASIC_INFO_COUNT_64;
        mach_port_t object = MACH_PORT_NULL;
        if (mach_vm_region(mach_task_self(), &address, &size, VM_REGION_BASIC_INFO_64, (vm_region_info_t)&info, &count,
                           &object) != KERN_SUCCESS)
            break;
        if (address >= (mach_vm_address_t)hi) break;
        fprintf(stderr, "[restore]   in the way: %llx-%llx prot=%x/%x shared=%d reserved=%d\n",
                (unsigned long long)address, (unsigned long long)(address + size), info.protection,
                info.max_protection, (int)info.shared, (int)info.reserved);
        if (size == 0) break;
        address += size;
    }
    return;
#else
    FILE *maps = fopen("/proc/self/maps", "r");
    char line[512];
    if (maps == NULL) return;
    while (fgets(line, sizeof line, maps) != NULL) {
        unsigned long long start = 0, end = 0;
        if (sscanf(line, "%llx-%llx", &start, &end) != 2) continue;
        if (end <= lo || start >= hi) continue;
        fprintf(stderr, "[restore]   in the way: %s", line);
    }
    fclose(maps);
#endif
}

static void *ckpt_map_exact_nonreplacing(uint64_t address, size_t length, int protection, int flags, int fd,
                                         off_t offset) {
#if defined(__APPLE__)
    /* Darwin has no MAP_FIXED_NOREPLACE. Claim the destination in one Mach operation first:
     * VM_FLAGS_FIXED without VM_FLAGS_OVERWRITE fails if any byte is occupied. The reservation
     * remains continuously present until MAP_FIXED atomically replaces our own pages, so this is
     * not the unsafe inspect-then-map sequence that would let another mapping enter the range. */
    mach_vm_address_t reserved = (mach_vm_address_t)address;
    kern_return_t status = mach_vm_allocate(mach_task_self(), &reserved, (mach_vm_size_t)length, VM_FLAGS_FIXED);
    if (status != KERN_SUCCESS) {
        if (status == KERN_NO_SPACE || status == KERN_MEMORY_PRESENT)
            errno = EEXIST;
        else if (status == KERN_INVALID_ADDRESS || status == KERN_INVALID_ARGUMENT)
            errno = EINVAL;
        else if (status == KERN_RESOURCE_SHORTAGE)
            errno = ENOMEM;
        else
            errno = EIO;
        return MAP_FAILED;
    }
    void *mapping = mmap((void *)(uintptr_t)address, length, protection, flags | MAP_FIXED, fd, offset);
    if (mapping == MAP_FAILED) {
        int map_errno = errno;
        (void)mach_vm_deallocate(mach_task_self(), reserved, (mach_vm_size_t)length);
        errno = map_errno;
    }
    return mapping;
#else
    int claim_flags = (flags & ~MAP_FIXED) | MAP_FIXED_NOREPLACE;
    return mmap((void *)(uintptr_t)address, length, protection, claim_flags, fd, offset);
#endif
}

typedef void *(*ckpt_exact_mapper)(uint64_t, size_t, int, int, int, off_t);
typedef int (*ckpt_exact_unmapper)(void *, size_t);

static int ckpt_claim_exact_with(ckpt_exact_mapper mapper, ckpt_exact_unmapper unmapper, uint64_t address,
                                 size_t length, int protection, int flags, int fd, off_t offset, void **claimed) {
    void *mapping = mapper(address, length, protection, flags, fd, offset);
    if (mapping == MAP_FAILED || (uint64_t)(uintptr_t)mapping != address) {
        int claim_errno = mapping == MAP_FAILED ? errno : EEXIST;
        if (mapping != MAP_FAILED) (void)unmapper(mapping, length);
        errno = claim_errno;
        *claimed = MAP_FAILED;
        return -1;
    }
    *claimed = mapping;
    return 0;
}

static int ckpt_unmap_exact(void *address, size_t length) {
    return munmap(address, length);
}

static int ckpt_claim_exact(uint64_t address, size_t length, int protection, int flags, int fd, off_t offset,
                            void **claimed) {
    return ckpt_claim_exact_with(ckpt_map_exact_nonreplacing, ckpt_unmap_exact, address, length, protection, flags, fd,
                                 offset, claimed);
}

// ---- guest-address reservation ---------------------------------------------------------------------
//
// A restored guest address is not negotiable: the image names it, the guest's pointers are
// unrelocatable, and a member that cannot claim its own address fails the whole tree's commit barrier.
// The init already protects ITS OWN addresses by restoring its RAM before the engine allocates
// anything (ckpt_restore_tree_body). Every other member's addresses were protected by nothing.
//
// That gap is invisible on a host whose layout is randomized per process and certain on one whose is
// not. Measured on x86-64 Linux under the pinned dev shell, which runs with ADDR_NO_RANDOMIZE: the
// restoring init's own host storage -- the process-tree commit barrier at the head of it -- is placed
// by the kernel's top-down allocator immediately below the init's restored guest mappings, which is
// exactly where the SIBLING members' captured mappings live. gpid 2 then asked for
// 7ffff6e1a000+b0000 and found the commit barrier in it, `[restore] exact guest-address claim failed:
// File exists`, and the tree never reached the barrier that mapping IS. Every host allocation the
// restore makes is in this population, including the ones libc makes on its behalf, so relocating any
// single one of them only moves the boundary.
//
// So the image's addresses are reserved BEFORE the restore allocates anything of its own, from the
// region walk `ckpt_validate_process_image` already performs over every member. A reservation is
// PROT_NONE and is released immediately before the member that owns the address claims it, so the
// claim itself stays MAP_FIXED_NOREPLACE and still fails closed against a mapping this restore does
// not own. Whatever a member never claims it drops once its own memory restore is complete, so no
// resumed guest carries a hole reserved for somebody else's image.
struct ckpt_restore_reservation {
    uint64_t lo;
    uint64_t hi;
};

static struct ckpt_restore_reservation *g_restore_reserved;
static int g_restore_reserved_capacity;
static int g_nrestore_reserved;
static int g_restore_reserve_applied;

// Round a region out to the host granularity exactly as the claim below does, so a reservation and the
// claim that consumes it name the same interval.
static int ckpt_restore_reserve_window(uint64_t address, uint64_t length, uint64_t *lo, uint64_t *hi) {
    uint64_t granularity = hl_linux_host_map_granularity();
    if (granularity == 0 || (granularity & (granularity - 1)) != 0) return -1;
    if (length == 0 || address > UINT64_MAX - length) return -1;
    uint64_t low = address & ~(granularity - 1);
    uint64_t high = address + length;
    if (high > UINT64_MAX - (granularity - 1)) return -1;
    high = (high + granularity - 1) & ~(granularity - 1);
    if (high <= low) return -1;
    *lo = low;
    *hi = high;
    return 0;
}

// Record one region of one member's image. Called from the validation walk, which runs before the
// restore has allocated anything; nothing is mapped here, because the walk itself is still holding
// image buffers whose addresses would then be reserved against their own owner.
static void ckpt_restore_reserve_note(uint64_t address, uint64_t length) {
    uint64_t lo, hi;
    if (g_restore_reserve_applied || ckpt_restore_reserve_window(address, length, &lo, &hi) != 0) return;
    if (ckpt_vector_reserve((void **)&g_restore_reserved, &g_restore_reserved_capacity, sizeof *g_restore_reserved,
                            g_nrestore_reserved + 1) != 0)
        return;
    g_restore_reserved[g_nrestore_reserved].lo = lo;
    g_restore_reserved[g_nrestore_reserved].hi = hi;
    g_nrestore_reserved++;
}

static int ckpt_restore_reservation_order(const void *first, const void *second) {
    const struct ckpt_restore_reservation *a = first;
    const struct ckpt_restore_reservation *b = second;
    if (a->lo < b->lo) return -1;
    if (a->lo > b->lo) return 1;
    if (a->hi < b->hi) return -1;
    return a->hi > b->hi;
}

// Take the noted addresses. Merged first, because members share addresses wholesale (a fork child's
// image repeats its parent's text, and every member of a container repeats the loader's), and an
// interval already held by this restore must not be presented to the kernel a second time.
//
// A window the host already occupies is DROPPED rather than refused: the occupant is host storage this
// engine mapped before the restore began, the member that wants the address will fail its own claim
// with the same diagnostic it does today, and refusing here would turn a single unrestorable member
// into an unrestorable tree.
static void ckpt_restore_reserve_apply(void) {
    if (g_restore_reserve_applied) return;
    g_restore_reserve_applied = 1;
    if (g_nrestore_reserved <= 0) return;
    qsort(g_restore_reserved, (size_t)g_nrestore_reserved, sizeof *g_restore_reserved, ckpt_restore_reservation_order);
    int merged = 0;
    for (int index = 1; index < g_nrestore_reserved; ++index) {
        if (g_restore_reserved[index].lo <= g_restore_reserved[merged].hi) {
            if (g_restore_reserved[index].hi > g_restore_reserved[merged].hi)
                g_restore_reserved[merged].hi = g_restore_reserved[index].hi;
            continue;
        }
        g_restore_reserved[++merged] = g_restore_reserved[index];
    }
    int held = 0;
    for (int index = 0; index <= merged; ++index) {
        uint64_t lo = g_restore_reserved[index].lo;
        uint64_t hi = g_restore_reserved[index].hi;
        void *claimed = MAP_FAILED;
        if (hi - lo > SIZE_MAX) continue;
        if (ckpt_claim_exact(lo, (size_t)(hi - lo), PROT_NONE, MAP_FIXED | MAP_PRIVATE | MAP_ANON | MAP_NORESERVE, -1,
                             0, &claimed) != 0)
            continue;
        g_restore_reserved[held].lo = lo;
        g_restore_reserved[held].hi = hi;
        held++;
    }
    g_nrestore_reserved = held;
}

// Hand [lo, hi) back to the kernel so the member that owns it can claim it. Only the intersection with
// an interval this restore actually reserved is unmapped: a rounded claim window can reach into the
// host page a neighbouring region of this same image already claimed, and that page is live restored
// memory, not a reservation.
static void ckpt_restore_reserve_release(uint64_t lo, uint64_t hi) {
    if (hi <= lo) return;
    for (int index = 0; index < g_nrestore_reserved; ++index) {
        uint64_t entry_lo = g_restore_reserved[index].lo;
        uint64_t entry_hi = g_restore_reserved[index].hi;
        uint64_t overlap_lo = lo > entry_lo ? lo : entry_lo;
        uint64_t overlap_hi = hi < entry_hi ? hi : entry_hi;
        if (overlap_hi <= overlap_lo) continue;
        (void)munmap((void *)(uintptr_t)overlap_lo, (size_t)(overlap_hi - overlap_lo));
        if (overlap_lo > entry_lo && overlap_hi < entry_hi) {
            if (ckpt_vector_reserve((void **)&g_restore_reserved, &g_restore_reserved_capacity,
                                    sizeof *g_restore_reserved, g_nrestore_reserved + 1) != 0) {
                // The tail cannot be tracked, so it must not be left mapped: a reservation nobody can
                // release is a hole in every process this one forks.
                (void)munmap((void *)(uintptr_t)overlap_hi, (size_t)(entry_hi - overlap_hi));
                g_restore_reserved[index].hi = overlap_lo;
                continue;
            }
            g_restore_reserved[index].hi = overlap_lo;
            g_restore_reserved[g_nrestore_reserved].lo = overlap_hi;
            g_restore_reserved[g_nrestore_reserved].hi = entry_hi;
            g_nrestore_reserved++;
            continue;
        }
        if (overlap_lo > entry_lo) {
            g_restore_reserved[index].hi = overlap_lo;
            continue;
        }
        if (overlap_hi < entry_hi) {
            g_restore_reserved[index].lo = overlap_hi;
            continue;
        }
        g_restore_reserved[index] = g_restore_reserved[--g_nrestore_reserved];
        index--;
    }
}

// The line between the two address populations a restored region can belong to: the deterministic guest
// arena, which the engine places and a restore can reclaim, and the host kernel's own top-down pool,
// which the engine does not control and a restore must never treat as arena.
//
// Probed rather than fixed for the reason host/linux/memory/mapping.c gives for the host code arena's
// base -- the usable virtual range differs per host and a fixed high constant is unmappable on a
// four-level-paging x86-64 host -- and separated from the pool by the same one terabyte, which that file
// established as out of the kernel allocator's reach until a guest holds a terabyte of mappings. A single
// page probe answers where the allocator is working right now; a gap below it is what makes the answer a
// boundary rather than a sample, because the probe lands in whichever hole the pool happens to have.
#define CKPT_RESTORE_HOST_POOL_GAP UINT64_C(0x10000000000) /* 1 TiB, host/linux/memory/mapping.c */

static uint64_t ckpt_restore_host_pool_edge(void) {
    static uint64_t edge;
    if (edge != 0) return edge;
    // The probe asks where the host's allocator PLACES a mapping, so its unit is the placement
    // granularity that linux_abi/page.h owns -- not the accounting page size beside it, which on a host
    // whose allocation granularity is coarser than its page (Windows: 64 KiB against 4 KiB) is not a
    // length mmap will accept for a fresh reservation at all. The reservation walk above already asks
    // page.h the same question; asking the C library directly made this the one placement decision in
    // the file that did not go through the owner.
    size_t granularity = hl_linux_host_map_granularity();
    if (granularity == 0) return UINT64_MAX;
    void *probe = mmap(NULL, granularity, PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (probe == MAP_FAILED) return UINT64_MAX;
    uint64_t top = (uint64_t)(uintptr_t)probe;
    (void)munmap(probe, granularity);
    if (top <= CKPT_RESTORE_HOST_POOL_GAP) return UINT64_MAX;
    edge = top - CKPT_RESTORE_HOST_POOL_GAP;
    return edge;
}

// Drop everything still reserved. Called once this process has forked every member that inherits the
// reservations and has finished its own memory restore, so what remains belongs to nobody in this
// address space.
static void ckpt_restore_reserve_release_all(void) {
    for (int index = 0; index < g_nrestore_reserved; ++index)
        (void)munmap((void *)(uintptr_t)g_restore_reserved[index].lo,
                     (size_t)(g_restore_reserved[index].hi - g_restore_reserved[index].lo));
    g_nrestore_reserved = 0;
    free(g_restore_reserved);
    g_restore_reserved = NULL;
    g_restore_reserved_capacity = 0;
}

// Resolve the next host sub-range of a region's rounded claim window.
//
// The host granularity can exceed the guest page size -- 16 KiB against the guest's 4 KiB on Apple
// Silicon -- so rounding a region out to whole host pages reaches into the host page a NEIGHBOURING
// guest region of this same image already claimed. That page is this restore's own, already present
// and writable, and re-claiming it would both collide (EEXIST) and, if it succeeded, zero the
// neighbour's already-copied bytes.
//
// Answers, for `cursor`: CKPT_SLICE_CLAIM with [cursor, *chunk_e) to claim, CKPT_SLICE_HELD with
// *chunk_e to resume past a page this restore already claimed, or CKPT_SLICE_REFUSE when the page is
// held by a claim this region may not share. Sharing a host page is only representable while both
// sides are anonymous guest RAM: one page cannot simultaneously be a view of a backing object at one
// offset and something else, so that case fails closed exactly as an occupied foreign page does.
#define CKPT_SLICE_CLAIM 0
#define CKPT_SLICE_HELD 1
#define CKPT_SLICE_REFUSE (-1)
static int ckpt_claim_slice(uint64_t cursor, uint64_t map_e, const uint64_t *mapped_a, const uint64_t *mapped_e,
                            const uint64_t *mapped_anon, size_t nmapped, int shareable, uint64_t *chunk_e) {
    uint64_t limit = map_e;
    for (size_t j = 0; j < nmapped; j++) {
        if (mapped_a[j] <= cursor && cursor < mapped_e[j]) {
            *chunk_e = mapped_e[j] < map_e ? mapped_e[j] : map_e;
            return shareable && mapped_anon[j] != 0 ? CKPT_SLICE_HELD : CKPT_SLICE_REFUSE;
        }
        if (mapped_a[j] > cursor && mapped_a[j] < limit) limit = mapped_a[j];
    }
    *chunk_e = limit;
    return CKPT_SLICE_CLAIM;
}

#if defined(HL_NATIVE_TEST_HOOKS)
struct ckpt_claim_test_state {
    void *mapping;
    int map_errno;
    unsigned unmaps;
};
static struct ckpt_claim_test_state g_ckpt_claim_test;
static size_t g_ckpt_rollback_file_ranges;
static size_t g_ckpt_rollback_logical_ranges;
static size_t g_ckpt_rollback_direct_ranges;

static void *ckpt_claim_test_map(uint64_t address, size_t length, int protection, int flags, int fd, off_t offset) {
    (void)address;
    (void)length;
    (void)protection;
    (void)flags;
    (void)fd;
    (void)offset;
    errno = g_ckpt_claim_test.map_errno;
    return g_ckpt_claim_test.mapping;
}

static int ckpt_claim_test_unmap(void *address, size_t length) {
    (void)address;
    (void)length;
    g_ckpt_claim_test.unmaps++;
    errno = ENOSPC; /* cleanup must not replace the claim verdict */
    return 0;
}

HL_API int hl_checkpoint_restore_claim_test(uint32_t scenario) {
    uint64_t requested = UINT64_C(0x200000);
    void *claimed = NULL;
    g_ckpt_claim_test = (struct ckpt_claim_test_state){0};
    if (scenario == 0) {
        g_ckpt_claim_test.mapping = MAP_FAILED;
        g_ckpt_claim_test.map_errno = EEXIST;
    } else if (scenario == 1) {
        g_ckpt_claim_test.mapping = (void *)(uintptr_t)(requested + UINT64_C(0x10000));
    } else if (scenario == 2) {
        unsigned char *sentinel = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (sentinel == MAP_FAILED) return 20;
        memset(sentinel, 0xa5, 4096);
        int result = ckpt_claim_exact((uint64_t)(uintptr_t)sentinel, 4096, PROT_READ | PROT_WRITE,
                                      MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0, &claimed);
        int claim_errno = errno;
        for (size_t index = 0; index < 4096; ++index) {
            if (sentinel[index] != 0xa5) {
                (void)munmap(sentinel, 4096);
                return 21;
            }
        }
        (void)munmap(sentinel, 4096);
        return result != 0 && claimed == MAP_FAILED && claim_errno == EEXIST ? 0 : 22;
    } else {
        return 10;
    }
    if (ckpt_claim_exact_with(ckpt_claim_test_map, ckpt_claim_test_unmap, requested, 4096, PROT_READ | PROT_WRITE,
                              MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0, &claimed) == 0)
        return 1;
    if (claimed != MAP_FAILED || errno != EEXIST) return 2;
    if (g_ckpt_claim_test.unmaps != (scenario == 1 ? 1u : 0u)) return 3;
    return 0;
}

// The addresses are the ones a real Apple Silicon restore failed on: a 4 KiB-granular guest image whose
// region 50010ee2000+c000 rounds DOWN onto the host page that 50010edd000+5000 rounds UP into. On a
// 4 KiB host these never touch, which is why the defect was macOS-only.
HL_API int HL_TARGET_LOCAL(checkpoint_restore_slice_test)(uint32_t scenario) {
    const uint64_t held_a[1] = {UINT64_C(0x50010ee0000)};
    const uint64_t held_e[1] = {UINT64_C(0x50010ef0000)};
    uint64_t held_anon[1] = {1};
    const uint64_t map_a = UINT64_C(0x50010edc000);
    const uint64_t map_e = UINT64_C(0x50010ee4000);
    uint64_t chunk_e = 0;
    if (scenario == 0) {
        // Anonymous guest RAM sharing a host page with an anonymous neighbour: claim the free head,
        // then step over the page this restore already owns instead of colliding with itself.
        if (ckpt_claim_slice(map_a, map_e, held_a, held_e, held_anon, 1, 1, &chunk_e) != CKPT_SLICE_CLAIM) return 1;
        if (chunk_e != held_a[0]) return 2;
        if (ckpt_claim_slice(chunk_e, map_e, held_a, held_e, held_anon, 1, 1, &chunk_e) != CKPT_SLICE_HELD) return 3;
        if (chunk_e != map_e) return 4;
        return 0;
    }
    if (scenario == 1) {
        // A file-backed region may not share a host page: one page cannot be a view of a backing
        // object at one offset and anonymous RAM at the same time.
        if (ckpt_claim_slice(held_a[0], map_e, held_a, held_e, held_anon, 1, 0, &chunk_e) != CKPT_SLICE_REFUSE)
            return 5;
        return 0;
    }
    if (scenario == 2) {
        // Nor may an anonymous region share the host page of a file-backed claim.
        held_anon[0] = 0;
        if (ckpt_claim_slice(held_a[0], map_e, held_a, held_e, held_anon, 1, 1, &chunk_e) != CKPT_SLICE_REFUSE)
            return 6;
        return 0;
    }
    if (scenario == 3) {
        // An empty claim table claims the whole window in one slice, exactly as before.
        if (ckpt_claim_slice(map_a, map_e, held_a, held_e, held_anon, 0, 1, &chunk_e) != CKPT_SLICE_CLAIM) return 7;
        if (chunk_e != map_e) return 8;
        return 0;
    }
    return 10;
}
#endif

#if defined(HL_NATIVE_TEST_HOOKS)
/* A re-forked restorer drops its parent's inherited address space through hl_gmap_reset() before it claims
 * its own image at exactly the captured guest addresses. That teardown has to reach the HOST pages the guest
 * range occupies: guest ranges are 4 KiB-granular and the deterministic arena places the brk heap one guard
 * page above HL_LINUX_SNAPSHOT_BASE, so on a 16 KiB host the range begins mid-page and Darwin's munmap(2)
 * refuses it outright. Scenario 0 measures the real registry against the real host; scenario 1 pins the
 * rounding itself against an explicit 16 KiB granularity, so it is answerable on a 4 KiB host too. */
HL_API int HL_TARGET_LOCAL(checkpoint_gmap_release_test)(uint32_t scenario) {
    if (scenario == 1) {
        uint64_t start = 0, end = 0;
        if (!hl_gmap_host_release_span(UINT64_C(0x50000001000), UINT64_C(0x4000), UINT64_C(0x4000), &start, &end))
            return 1;
        if (start != UINT64_C(0x50000000000)) return 2;  /* the head page the guest range begins inside */
        if (end != UINT64_C(0x50000008000)) return 3;    /* and the tail page it ends inside */
        if (hl_gmap_host_release_span(UINT64_C(0x1000), 0, UINT64_C(0x4000), &start, &end)) return 4;
        if (hl_gmap_host_release_span(UINT64_C(0x1000), UINT64_C(0x1000), 0, &start, &end)) return 5;
        return 0;
    }
    if (scenario != 0) return 10;
    uint64_t grain = (uint64_t)hl_linux_host_map_granularity();
    if (grain == 0 || (grain & (grain - 1)) != 0) return 20;
    /* Reproduce the arena's shape wherever the host allows it: a guest range that begins one 4 KiB guard
     * page inside a host page and therefore spans two of them. A 4 KiB host cannot express that at all --
     * every guest address is host-aligned there -- so it measures the single-page teardown instead, and
     * scenario 1 carries the rounding itself. */
    uint64_t offset = grain > HL_LINUX_GUEST_PAGE_SIZE ? HL_LINUX_GUEST_PAGE_SIZE : 0;
    size_t span = (size_t)(offset != 0 ? grain * 2u : grain);
    /* Take the probe range from a fixed band, never from the kernel's own choice.
     *
     * The sequence under test opens a hole on purpose -- release the host pages, then claim exactly those
     * pages back -- and a range the allocator picked sits at the top of this process's free area, which is
     * exactly where it places the NEXT mmap(NULL) any thread issues. Measured on x86_64 Linux with a peer
     * thread asking for an address inside the window: it is handed the freed range 197 times in 200 from an
     * allocator-chosen probe and 0 times in 200 from a fixed band the top-down search does not reach. This
     * is a test binary with several threads mapping at once, so the allocator-chosen form failed for a
     * reason that has nothing to do with the teardown it measures -- and retrying could not help, because
     * every attempt re-opened the same hole in the same place. The production sequence runs in a
     * just-forked restorer that has one thread and no peer to lose the range to. */
    static const uint64_t probe_band[] = {UINT64_C(0x5c000000000), UINT64_C(0x5c400000000),
                                          UINT64_C(0x5c800000000), UINT64_C(0x5cc00000000)};
    void *host = MAP_FAILED;
    for (size_t index = 0; index < sizeof probe_band / sizeof probe_band[0]; ++index) {
        void *want = (void *)(uintptr_t)probe_band[index];
        void *attempt = mmap(want, span, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (attempt == want) {
            host = attempt;
            break;
        }
        if (attempt != MAP_FAILED) (void)munmap(attempt, span);
    }
    /* Distinct from 22: no band was free, so this run measured nothing about the teardown and must not be
     * reported as its refusal. */
    if (host == MAP_FAILED) return 24;
    uint64_t base = (uint64_t)(uintptr_t)host;
    hl_gmap_add(base + offset, grain);
    hl_gmap_reset();
    void *claimed = NULL;
    int reclaimed = ckpt_claim_exact(base, span, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS, -1,
                                     0, &claimed);
    int claim_errno = errno;
    (void)munmap((void *)(uintptr_t)base, span);
    if (reclaimed != 0) return claim_errno == EEXIST ? 22 : 23;
    return 0;
}

#endif

static void ckpt_restore_rollback(const struct ckpt_region *topology, size_t processed, size_t registered,
                                  const uint64_t *mapped_a, const uint64_t *mapped_e, size_t nmapped) {
    int restore_errno = errno != 0 ? errno : EIO;
    for (size_t index = 0; index < registered; ++index) {
        const struct ckpt_region *region = &topology[index];
        filemap_unmap(region->addr, region->addr + region->glen);
        futex_shared_unmap(region->addr, region->addr + region->glen);
#if defined(HL_NATIVE_TEST_HOOKS)
        g_ckpt_rollback_file_ranges++;
#endif
    }
    for (size_t index = 0; index < processed; ++index) {
        const struct ckpt_region *region = &topology[index];
        hl_gmap_unmap_range(region->addr, region->addr + region->len);
        anon_split_unmap(region->addr, region->addr + region->len);
        gna_clear(region->addr & ~(uint64_t)0xfff,
                  (region->addr + region->glen + UINT64_C(0xfff)) & ~UINT64_C(0xfff));
        if (region->logical) {
            (void)hl_logical_vma_global_unmap(region->addr, region->glen);
#if defined(HL_NATIVE_TEST_HOOKS)
            g_ckpt_rollback_logical_ranges++;
#endif
        }
    }
    for (size_t index = nmapped; index != 0; --index) {
        (void)munmap((void *)(uintptr_t)mapped_a[index - 1], (size_t)(mapped_e[index - 1] - mapped_a[index - 1]));
#if defined(HL_NATIVE_TEST_HOOKS)
        g_ckpt_rollback_direct_ranges++;
#endif
    }
    errno = restore_errno;
}

#if defined(HL_NATIVE_TEST_HOOKS)
HL_API int HL_TARGET_LOCAL(checkpoint_restore_rollback_test)(void) {
    struct ckpt_region topology[2] = {
        {.addr = UINT64_C(0x100000), .len = 4096, .glen = 4096, .format_version = CKPT_REGION_VERSION},
        {.addr = UINT64_C(0x200000),
         .len = 4096,
         .glen = 4096,
         .backing_object = 1,
         .backing_shared = 1,
         .format_version = CKPT_REGION_VERSION,
         .logical = 1},
    };
    uint64_t mapped_a[1] = {UINT64_C(0x100000)};
    uint64_t mapped_e[1] = {UINT64_C(0x101000)};
    g_ckpt_rollback_file_ranges = 0;
    g_ckpt_rollback_logical_ranges = 0;
    g_ckpt_rollback_direct_ranges = 0;
    errno = EEXIST;
    ckpt_restore_rollback(topology, 2, 2, mapped_a, mapped_e, 1);
    if (errno != EEXIST) return 1;
    if (g_ckpt_rollback_file_ranges != 2) return 2;
    if (g_ckpt_rollback_logical_ranges != 1) return 3;
    if (g_ckpt_rollback_direct_ranges != 1) return 4;
    return 0;
}
#endif

// Rebuild this process's guest memory (MAP_FIXED) + the mapping side-registries from `procdir`. For the init
// this runs BEFORE engine init (so MAP_FIXED lands on free VAs); a re-forked child calls hl_gmap_reset() +
// clears the anon/gna counters FIRST (dropping the COW-inherited init mappings) so its own RAM lands clean.
static int ckpt_restore_mem_dir(const char *procdir, const struct ckpt_meta *m) {
    uint64_t *mapped = NULL;
    struct ckpt_region *topology = NULL;
    uint64_t *mapped_a;
    uint64_t *mapped_e;
    uint64_t *mapped_anon;
    size_t nmapped = 0;
    size_t processed = 0;
    size_t registered = 0;
    jit_guest_soft_restore_deactivate();
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/pages", procdir);
    FILE *f = ckpt_source_fopen(pf);
    if (!f) {
        fprintf(stderr, "[restore] open %s: %s\n", pf, strerror(errno));
        return -1;
    }
    if (ckpt_minimum_counted_object_size(ckpt_source_object_size(pf), m->n_regions, sizeof(struct ckpt_region),
                                         UINT64_C(1048576)) != 0) {
        ckpt_source_fclose(f);
        return -1;
    }
    if (m->n_regions > SIZE_MAX / (3u * sizeof(*mapped))) {
        ckpt_source_fclose(f);
        return -1;
    }
    if (m->n_regions != 0) {
        mapped = calloc((size_t)m->n_regions * 3u, sizeof(*mapped));
        topology = calloc((size_t)m->n_regions, sizeof(*topology));
        if (mapped == NULL || topology == NULL) {
            ckpt_source_fclose(f);
            free(mapped);
            free(topology);
            return -1;
        }
    }
    mapped_a = mapped;
    mapped_e = mapped != NULL ? mapped + (size_t)m->n_regions : NULL;
    mapped_anon = mapped != NULL ? mapped + 2u * (size_t)m->n_regions : NULL;
    for (uint64_t i = 0; i < m->n_regions; i++) {
        struct ckpt_region reg;
        if (ckpt_read_region(f, &reg) != 0) { goto fail; }
        if (!ckpt_region_valid(&reg)) {
            fprintf(stderr, "[restore] invalid region format=%u logical=%u\n", reg.format_version, reg.logical);
            goto fail;
        }
        topology[i] = reg;
        // Hand back the reservation this restore took over the region's claim window, immediately
        // before the claim itself. Nothing allocates between the two, so the window cannot be taken by
        // host storage in between, and the claim below stays MAP_FIXED_NOREPLACE.
        {
            uint64_t reserved_lo, reserved_hi;
            if (ckpt_restore_reserve_window(reg.addr, reg.len, &reserved_lo, &reserved_hi) == 0)
                ckpt_restore_reserve_release(reserved_lo, reserved_hi);
        }
        uint64_t a = reg.addr, e = reg.addr + reg.len;
        int contained = 0;
        for (size_t j = 0; j < nmapped; j++)
            if (mapped_a[j] <= a && e <= mapped_e[j]) {
                contained = 1;
                break;
            }
        if (reg.logical) {
            if (reg.backing_object == 0 || !reg.backing_shared || reg.backing_emulated) {
                fprintf(stderr, "[restore] invalid logical backing metadata\n");
                goto fail;
            }
            jit_guest_soft_restore_activate();
            uint64_t seed_size = reg.backing_offset + reg.glen;
            if (seed_size < reg.backing_offset) goto fail;
            int seed = ckpt_restore_backing_seed(procdir, reg.backing_object, seed_size);
            if (seed < 0 ||
                hl_logical_vma_global_restore_shared(reg.addr, reg.glen, (uint32_t)reg.prot, seed, reg.backing_offset,
                                                     hl_linux_host_map_granularity()) != 0) {
                fprintf(stderr, "[restore] cannot rebuild logical guest region %llx+%llx: %s\n",
                        (unsigned long long)reg.addr, (unsigned long long)reg.glen, strerror(errno));
                goto fail;
            }
        } else if (!contained) {
            uint64_t host_granularity = hl_linux_host_map_granularity();
            if (host_granularity == 0 || (host_granularity & (host_granularity - 1)) != 0) goto fail;
            uint64_t map_a = a & ~(host_granularity - 1);
            uint64_t map_e = (e + host_granularity - 1) & ~(host_granularity - 1);
            if (map_e < e || map_e <= map_a || map_e - map_a > SIZE_MAX) goto fail;
            size_t map_len = (size_t)(map_e - map_a);
            uint64_t prefix = a - map_a;
            int map_flags = MAP_FIXED | MAP_ANON | MAP_PRIVATE;
            int map_fd = -1;
            off_t map_offset = 0;
            if (reg.backing_object != 0 && !reg.backing_emulated) {
                if (reg.backing_offset < prefix) goto fail;
                uint64_t adjusted_offset = reg.backing_offset - prefix;
                if (adjusted_offset > UINT64_MAX - map_len) goto fail;
                map_fd = reg.backing_anon_shared
                             ? ckpt_restore_anon_shared_seed(reg.backing_object, adjusted_offset + map_len)
                             : ckpt_restore_backing_seed(procdir, reg.backing_object, adjusted_offset + map_len);
                if (map_fd < 0) {
                    fprintf(stderr, "[restore] cannot prepare backing object %llx\n",
                            (unsigned long long)reg.backing_object);
                    goto fail;
                }
                map_flags = MAP_FIXED | (reg.backing_shared ? MAP_SHARED : MAP_PRIVATE);
                map_offset = (off_t)adjusted_offset;
            }
            // A saved guest VA can name live restoring-engine state. Claim the exact range without
            // replacement and fail closed when any byte is occupied. host_mman.h implements
            // FIXED_NOREPLACE on every supported host: Linux uses the kernel flag, Darwin first claims an
            // exact Mach reservation without VM_FLAGS_OVERWRITE, and Windows uses placeholders. Never retry
            // with MAP_FIXED here: the guest's pointers are unrelocatable, but overwriting an unowned host
            // mapping corrupts the process that is supposed to report the restore failure.
            uint64_t cursor = map_a;
            while (cursor < map_e) {
                uint64_t chunk_e = map_e;
                int slice =
                    ckpt_claim_slice(cursor, map_e, mapped_a, mapped_e, mapped_anon, nmapped, map_fd < 0, &chunk_e);
                if (slice == CKPT_SLICE_HELD) {
                    cursor = chunk_e;
                    continue;
                }
                void *r = MAP_FAILED;
                if (slice == CKPT_SLICE_REFUSE) errno = EEXIST;
                if (slice == CKPT_SLICE_REFUSE ||
                    ckpt_claim_exact(cursor, (size_t)(chunk_e - cursor), PROT_READ | PROT_WRITE, map_flags, map_fd,
                                     map_offset + (off_t)(cursor - map_a), &r) != 0) {
                    int claim_errno = errno;
                    fprintf(stderr,
                            "[restore] gpid %d cannot claim guest region %llx+%llx (map %llx-%llx fd=%d) without "
                            "replacing a live host mapping\n",
                            g_self_gpid, (unsigned long long)a, (unsigned long long)reg.len,
                            (unsigned long long)map_a, (unsigned long long)map_e, map_fd);
                    ckpt_report_overlap(map_a, map_e);
                    errno = claim_errno;
                    fprintf(stderr, "[restore] exact guest-address claim failed: %s\n", strerror(errno));
                    errno = claim_errno;
                    goto fail;
                }
                cursor = chunk_e;
            }
            mapped_a[nmapped] = map_a;
            mapped_e[nmapped] = map_e;
            mapped_anon[nmapped] = map_fd < 0 ? 1u : 0u;
            nmapped++;
        }
        for (uint64_t p = 0; p < reg.npages; p++) {
            uint64_t va;
            if (ckpt_rd_all(f, &va, sizeof va) != 0) { goto fail; }
            size_t n = (va - reg.addr + m->pagesz > reg.len) ? (size_t)(reg.len - (va - reg.addr)) : (size_t)m->pagesz;
            if (reg.logical) {
                void *page = malloc(n);
                if (page == NULL || ckpt_rd_all(f, page, n) != 0 || hl_logical_vma_global_copy_in(va, page, n) != 0) {
                    fprintf(stderr, "[restore] cannot copy logical guest page %llx+%zx: %s\n", (unsigned long long)va,
                            n, strerror(errno));
                    free(page);
                    goto fail;
                }
                free(page);
            } else if (ckpt_rd_all(f, (void *)va, n) != 0)
                goto fail;
        }
        // ONLY an address the deterministic arena itself could have produced may move its cursor.
        //
        // The arena (linux_abi/container/snapshot.h) exists so that a guest map the kernel would
        // otherwise place lands somewhere a later restore can reclaim, and this call keeps its cursor
        // above memory a restore has just replayed. A region the HOST kernel placed is not in the arena
        // at all -- it is in the kernel's own top-down pool, a hundred terabytes above it -- and letting
        // one of those move the cursor retires the arena outright: every later reserve then answers an
        // address inside the engine's own libraries, the kernel silently relocates the request back into
        // the top-down pool, and the next generation's guest addresses are drawn from the one region
        // where the engine's own host storage also lives.
        //
        // Measured across five Continue-later cycles on x86-64 Linux: the transient `sleep` the fixture's
        // shell re-execs each iteration took its 256 MiB brk heap from the arena on a fresh launch, and
        // from the kernel pool on every generation after a restore -- 7fff94000000, then 7fff98000000,
        // climbing 64 MiB per cycle until it reached a glibc thread arena the restoring worker had mapped
        // before the restore began, which no reservation can move.
        if (reg.addr >= HL_LINUX_SNAPSHOT_BASE && reg.addr < ckpt_restore_host_pool_edge())
            hl_linux_snapshot_advance(&g_ckpt_snapshot, reg.addr + reg.len);
        hl_gmap_add(reg.addr, reg.len);
        hl_gmap_set_guest_length(reg.addr, reg.glen);
        // ONE verdict per region, so PROT_NONE sub-intervals of a piecewise-mprotect'd region are dropped (a
        // restored guard page reads accessible). Do NOT widen the claim back to any-page: whole-region
        // poisoning is far worse.
        if (reg.is_gna)
            gna_add(reg.addr & ~(uint64_t)0xfff, (reg.addr + reg.glen + 0xfff) & ~(uint64_t)0xfff);
        else
            anon_track(reg.addr, reg.len, reg.prot);
        processed++;
    }
    ckpt_source_fclose(f);
    f = NULL;
    for (uint64_t i = 0; i < m->n_regions; i++) {
        struct ckpt_region *reg = &topology[i];
        if (reg->backing_object == 0) continue;
        // An anonymous shared region is not a FILE mapping to the guest: it has no path, no guest
        // descriptor, and /proc/<pid>/maps must keep reporting it anonymous. Its seed was already
        // bound above, and its shared-futex keys stay VA-keyed, which is still correct because the
        // region is re-mapped at exactly its captured address.
        if (reg->backing_anon_shared) continue;
        if (reg->backing_offset > UINT64_MAX - reg->glen) {
            errno = EOVERFLOW;
            goto fail;
        }
        int seed = ckpt_restore_backing_seed(procdir, reg->backing_object, reg->backing_offset + reg->glen);
        if (seed < 0) {
            fprintf(stderr, "[restore] cannot rebuild backing object %llx\n", (unsigned long long)reg->backing_object);
            goto fail;
        }
        filemap_register(reg->addr, reg->glen, seed, reg->backing_offset, reg->backing_shared, reg->backing_emulated);
        if (reg->backing_shared && !reg->backing_emulated)
            futex_shared_register(reg->addr, reg->glen, seed, reg->backing_offset);
        registered = (size_t)i + 1;
    }
    free(mapped);
    free(topology);
    brk_lo = m->brk_lo;
    brk_cur = m->brk_cur;
    brk_hi = m->brk_hi;
    g_nonpie_lo = m->nonpie_lo;
    g_nonpie_hi = m->nonpie_hi;
    g_nonpie_bias = m->nonpie_bias;
    g_stack_lo = m->stack_lo;
    g_stack_hi = m->stack_hi;
    return 0;
fail:
    {
        int restore_errno = errno != 0 ? errno : EIO;
        if (f != NULL) ckpt_source_fclose(f);
        errno = restore_errno;
        ckpt_restore_rollback(topology, processed, registered, mapped_a, mapped_e, nmapped);
        errno = restore_errno;
    }
    free(mapped);
    free(topology);
    return -1;
}

// Reopen this process's own path-backed fds. TTY fds are NOT reopened here -- they are inherited down the
// restore fork from the launcher's pty (init got 0/1/2 from the launcher; each child inherits them).
struct ckpt_restore_pipe {
    uint64_t identity;
    int reader;
    int writer;
    int size;
};
static struct ckpt_restore_pipe *g_restore_pipes;
static int g_nrestore_pipes;
static int g_restore_pipes_capacity;

struct ckpt_restore_eventfd {
    uint64_t identity;
    uint64_t count;
    int reader;
    int writer;
    int slot;
    uint8_t semaphore;
    uint8_t guest_nonblock;
};
static struct ckpt_restore_eventfd *g_restore_eventfds;
static int g_nrestore_eventfds;
static int g_restore_eventfds_capacity;

struct ckpt_restore_timerfd {
    uint64_t identity;
    struct timerfd_shared_state *state;
    int clock_id;
    int fd;
    int slot;
    uint8_t first_oneshot;
};
static struct ckpt_restore_timerfd *g_restore_timerfds;
static int g_nrestore_timerfds;
static int g_restore_timerfds_capacity;

struct ckpt_restore_signalfd {
    uint64_t identity;
    uint64_t mask;
    int reader;
    int writer;
};
static struct ckpt_restore_signalfd *g_restore_signalfds;
static int g_nrestore_signalfds;
static int g_restore_signalfds_capacity;

struct ckpt_restore_socket_endpoint {
    uint64_t identity;
    uint64_t peer_identity;
    int fd;
    int type;
    uint8_t guest_present;
    uint8_t peer_closed;
    uint8_t state_loaded;
    struct ckpt_socket_state state;
};
static struct ckpt_restore_socket_endpoint *g_restore_socket_endpoints;
static int g_nrestore_socket_endpoints;
static int g_restore_socket_endpoints_capacity;

struct ckpt_restore_right {
    uint64_t ofd_id;
    uint64_t object_id;
    int fd;
    uint8_t owned;
};
static struct ckpt_restore_right *g_restore_rights;
static int g_nrestore_rights;
static int g_restore_rights_capacity;

static struct ckpt_restore_right *ckpt_restore_right_find(uint64_t ofd_id) {
    for (int index = 0; index < g_nrestore_rights; ++index)
        if (g_restore_rights[index].ofd_id == ofd_id) return &g_restore_rights[index];
    return NULL;
}

struct ckpt_restore_socket {
    uint64_t identity;
    int fd;
    struct ckpt_socket_state state;
};
static struct ckpt_restore_socket *g_restore_sockets;
static int g_nrestore_sockets;
static int g_restore_sockets_capacity;

static struct ckpt_restore_socket *ckpt_restore_socket_state_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_sockets; ++i)
        if (g_restore_sockets[i].identity == identity) return &g_restore_sockets[i];
    return NULL;
}

static struct ckpt_restore_socket_endpoint *ckpt_restore_socket_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_socket_endpoints; ++i)
        if (g_restore_socket_endpoints[i].identity == identity) return &g_restore_socket_endpoints[i];
    return NULL;
}

static struct ckpt_restore_timerfd *ckpt_restore_timerfd_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_timerfds; i++)
        if (g_restore_timerfds[i].identity == identity) return &g_restore_timerfds[i];
    return NULL;
}

static struct ckpt_restore_signalfd *ckpt_restore_signalfd_find(uint64_t identity) {
    for (int index = 0; index < g_nrestore_signalfds; ++index)
        if (g_restore_signalfds[index].identity == identity) return &g_restore_signalfds[index];
    return NULL;
}

static struct ckpt_restore_eventfd *ckpt_restore_eventfd_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_eventfds; i++)
        if (g_restore_eventfds[i].identity == identity) return &g_restore_eventfds[i];
    return NULL;
}

static struct ckpt_restore_pipe *ckpt_restore_pipe_find(uint64_t identity) {
    for (int i = 0; i < g_nrestore_pipes; i++)
        if (g_restore_pipes[i].identity == identity) return &g_restore_pipes[i];
    return NULL;
}

static void ckpt_restore_pipe_seeds_close(void) {
    for (int i = 0; i < g_nrestore_pipes; i++) {
        hl_host_process_fd_private_remove(g_restore_pipes[i].reader);
        hl_host_process_fd_private_remove(g_restore_pipes[i].writer);
        close(g_restore_pipes[i].reader);
        close(g_restore_pipes[i].writer);
    }
}

static void ckpt_restore_eventfd_seeds_close(void) {
    for (int i = 0; i < g_nrestore_eventfds; i++) {
        hl_host_process_fd_private_remove(g_restore_eventfds[i].reader);
        close(g_restore_eventfds[i].reader);
        /* The writer is not a disposable seed: it is the live hidden peer referenced by every restored
         * alias in this process. fd_reset_emul closes it when the process's final alias is released. */
    }
}

static void ckpt_restore_signalfd_seeds_close(void) {
    for (int index = 0; index < g_nrestore_signalfds; ++index) {
        hl_host_process_fd_private_remove(g_restore_signalfds[index].reader);
        hl_host_process_fd_private_remove(g_restore_signalfds[index].writer);
        close(g_restore_signalfds[index].reader);
        close(g_restore_signalfds[index].writer);
    }
}

static void ckpt_restore_socket_seeds_close(void) {
    for (int i = 0; i < g_nrestore_socket_endpoints; ++i) {
        if (g_restore_socket_endpoints[i].fd < 0) continue;
        hl_host_process_fd_private_remove(g_restore_socket_endpoints[i].fd);
        close(g_restore_socket_endpoints[i].fd);
        g_restore_socket_endpoints[i].fd = -1;
    }
    g_nrestore_socket_endpoints = 0;
    for (int i = 0; i < g_nrestore_sockets; ++i) {
        if (g_restore_sockets[i].fd < 0) continue;
        hl_host_process_fd_private_remove(g_restore_sockets[i].fd);
        close(g_restore_sockets[i].fd);
        g_restore_sockets[i].fd = -1;
    }
    g_nrestore_sockets = 0;
    for (int i = 0; i < g_nrestore_rights; ++i) {
        if (g_restore_rights[i].owned == 2) {
            if (g_linux_box != NULL) (void)hl_linux_close(g_linux_box, (hl_linux_fd)g_restore_rights[i].fd);
            proc_fdvis_close(g_restore_rights[i].fd);
            close(g_restore_rights[i].fd);
        } else if (g_restore_rights[i].owned) {
            hl_host_process_fd_private_remove(g_restore_rights[i].fd);
            close(g_restore_rights[i].fd);
        }
    }
    g_nrestore_rights = 0;
}

static int ckpt_restore_file_blob(const char *procdir, const struct ckpt_fd *record) {
    char source_path[1400], temporary[] = "/tmp/hl-checkpoint-file.XXXXXX";
    snprintf(source_path, sizeof source_path, "%s", record->path);
    FILE *source = ckpt_source_fopen(source_path);
    if (!source) return -1;
    int staging = mkstemp(temporary);
    if (staging < 0) {
        ckpt_source_fclose(source);
        return -1;
    }
    unsigned char buffer[65536];
    int failed = 0;
    size_t count;
    while ((count = fread(buffer, 1, sizeof buffer, source)) != 0) {
        size_t offset = 0;
        while (offset < count) {
            ssize_t written = write(staging, buffer + offset, count - offset);
            if (written > 0) {
                offset += (size_t)written;
                continue;
            }
            if (written < 0 && errno == EINTR) continue;
            failed = 1;
            break;
        }
        if (failed) break;
    }
    if (ferror(source)) failed = 1;
    ckpt_source_fclose(source);
    if (!failed && fsync(staging) != 0) failed = 1;
    close(staging);
    if (failed) {
        unlink(temporary);
        return -1;
    }
    int flags = record->flags & ~(O_CREAT | O_EXCL | O_TRUNC);
    int restored = open(temporary, flags);
    unlink(temporary);
    if (restored < 0) return -1;
    if (restored != record->gfd) {
        if (dup2(restored, record->gfd) < 0) {
            close(restored);
            return -1;
        }
        close(restored);
    }
    if (lseek(record->gfd, (off_t)record->offset, SEEK_SET) < 0) return -1;
    if (record->descriptor_flags & FD_CLOEXEC)
        if (fcntl(record->gfd, F_SETFD, FD_CLOEXEC) != 0) return -1;
    return proc_fdvis_publish_native_fd(record->gfd);
}

static int ckpt_restore_epoll_watches(const char *procdir, const struct ckpt_fd *record) {
    char path[1400];
    snprintf(path, sizeof path, "%s/%s", procdir, record->path);
    int64_t stored = ckpt_source_object_size(path);
    size_t size;
    const size_t maximum = sizeof(struct ckpt_epoll_header) +
                           (size_t)CKPT_EPOLL_WATCH_LIMIT * sizeof(struct ckpt_epoll_watch);
    if (ckpt_bounded_object_size(stored, sizeof(struct ckpt_epoll_header), maximum, &size) != 0) return -1;
    unsigned char *image = malloc(size);
    if (image == NULL || ckpt_source_load(path, image, size) != 0) {
        free(image);
        return -1;
    }
    struct ckpt_epoll_header header;
    memcpy(&header, image, sizeof header);
    if (header.magic != CKPT_EPOLL_MAGIC ||
        ckpt_counted_object_size(size, sizeof header, header.count, sizeof(struct ckpt_epoll_watch),
                                 CKPT_EPOLL_WATCH_LIMIT) != 0) {
        free(image);
        return -1;
    }
    const struct ckpt_epoll_watch *watches = (const void *)(image + sizeof header);
    for (uint32_t index = 0; index < header.count; ++index) {
        const struct ckpt_epoll_watch *saved = &watches[index];
        if (saved->descriptor < 0 || saved->descriptor >= HL_NFD || fcntl(saved->descriptor, F_GETFD) < 0) {
            free(image);
            return -1;
        }
        hl_linux_fd_snapshot snapshot;
        int typed = g_linux_box != NULL &&
                    hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)saved->descriptor, &snapshot) == HL_STATUS_OK;
        if (typed && hl_provider_files_is_handle(snapshot.host_handle)) {
            ep_provider_watch *watch = ep_provider_alloc(g_ep_provider_watches, EP_PROVIDER_WATCH_LIMIT);
            if (watch == NULL) {
                free(image);
                return -1;
            }
            uint32_t serial = g_ep_provider_serial = ep_provider_next(g_ep_provider_serial);
            ep_provider_activate(watch, record->gfd, g_ep_provider_generations[record->gfd], saved->descriptor,
                                 snapshot.descriptor_generation, serial, snapshot.host_handle, saved->events,
                                 saved->interests, saved->data);
            if (saved->interests != 0 &&
                hl_provider_files_subscribe(snapshot.host_handle, saved->interests, bound_epoll_provider_ready, watch,
                                            atomic_load(&watch->serial)) != 0) {
                ep_provider_reservation_cancel(watch);
                free(image);
                return -1;
            }
            continue;
        }
        if (typed) {
            hl_linux_object_pin pin;
            int object_ready = 0;
            if (hl_linux_object_pin_fd(g_linux_box, (hl_linux_fd)saved->descriptor, &pin) == HL_STATUS_OK) {
                object_ready = pin.ops != NULL && pin.ops->readiness != NULL;
                hl_linux_object_unpin(&pin);
            }
            if (object_ready) {
                ep_object_watch *watch = ep_object_alloc();
                if (watch == NULL) {
                    free(image);
                    return -1;
                }
                watch->epoll = record->gfd;
                watch->epoll_generation = g_ep_provider_generations[record->gfd];
                watch->descriptor = saved->descriptor;
                watch->descriptor_generation = snapshot.descriptor_generation;
                watch->events = saved->events;
                watch->interests = saved->interests;
                watch->data = saved->data;
                g_ep_object_count[record->gfd]++;
                continue;
            }
        }
        struct kevent changes[2];
        int change_count = 0;
        uint16_t flags = (uint16_t)((saved->events & UINT32_C(0x80000000) ? EV_CLEAR : 0) |
                                    (saved->events & UINT32_C(0x40000000) ? EV_ONESHOT : 0));
        if ((saved->armed & 1u) != 0) {
            EV_SET(&changes[change_count++], saved->descriptor, EVFILT_READ, EV_ADD | flags, 0, 0,
                   (void *)(uintptr_t)saved->data);
        }
        if ((saved->armed & 2u) != 0) {
            EV_SET(&changes[change_count++], saved->descriptor, EVFILT_WRITE, EV_ADD | flags, 0, 0,
                   (void *)(uintptr_t)saved->data);
        }
        if (change_count != 0 && kevent(record->gfd, changes, change_count, NULL, 0, NULL) < 0) {
            free(image);
            return -1;
        }
        ep_mem_set(record->gfd, saved->descriptor, 1);
        g_ep_owner[saved->descriptor] = record->gfd + 1;
        g_ep_events[saved->descriptor] = saved->events;
        g_ep_udata[saved->descriptor] = saved->data;
        g_ep_rd[saved->descriptor] = (saved->armed & 1u) != 0;
        g_ep_wr[saved->descriptor] = (saved->armed & 2u) != 0;
        g_ep_os[saved->descriptor] = (saved->events & UINT32_C(0x40000000)) != 0;
        if (ep_native_set(record->gfd, saved->descriptor, 3, saved->events, saved->data) != 0) {
            free(image);
            return -1;
        }
        ep_native_watch *native = ep_native_find(record->gfd, saved->descriptor);
        if (native) native->armed = saved->armed;
    }
    ep_wake_arm(record->gfd);
    free(image);
    return 0;
}

static int ckpt_restore_inotify_sidecar(const char *procdir) {
    char path[1300];
    snprintf(path, sizeof path, "%s/inotify", procdir);
    FILE *file = ckpt_source_fopen(path);
    if (!file) return errno == ENOENT ? 0 : -1;
    uint32_t watches = 0, moves = 0, raw_instances = 0;
    if (ckpt_rd_all(file, &watches, sizeof watches) != 0 || ckpt_rd_all(file, &moves, sizeof moves) != 0 ||
        ckpt_rd_all(file, &raw_instances, sizeof raw_instances) != 0 || watches > HL_NFD ||
        moves > (uint32_t)(sizeof g_inomv / sizeof g_inomv[0]) || raw_instances > HL_NFD)
        goto fail;
    for (uint32_t index = 0; index < watches; index++) {
        struct ckpt_inotify_watch watch;
        if (ckpt_rd_all(file, &watch, sizeof watch) != 0 || watch.instance < 0 || watch.instance >= HL_NFD ||
            watch.wd < 0 || watch.wd >= HL_NFD || !g_inotify[watch.instance] || !watch.path[0] ||
            watch.snapshot_size > 16 * 1024 * 1024u)
            goto fail;
        char *snapshot = NULL;
        if (watch.snapshot_size) {
            snapshot = malloc(watch.snapshot_size);
            if (!snapshot || ckpt_rd_all(file, snapshot, watch.snapshot_size) != 0 ||
                snapshot[watch.snapshot_size - 1] != '\0') {
                free(snapshot);
                goto fail;
            }
        }
#if defined(__linux__)
        int restored_wd = inotify_add_watch(watch.instance, watch.path, watch.mask);
        if (restored_wd != watch.wd) {
            free(snapshot);
            goto fail;
        }
#else
        int opened = hl_native_open_watch(watch.path);
        if (opened < 0) {
            free(snapshot);
            goto fail;
        }
        engine_fd_vacate(watch.wd);
        if (opened != watch.wd) {
            if (dup2(opened, watch.wd) < 0) {
                close(opened);
                free(snapshot);
                goto fail;
            }
            close(opened);
        }
        struct kevent event;
        EV_SET(&event, watch.wd, EVFILT_VNODE, EV_ADD | EV_CLEAR,
               NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND, 0, (void *)(intptr_t)watch.wd);
        if (kevent(watch.instance, &event, 1, NULL, 0, NULL) < 0) {
            close(watch.wd);
            free(snapshot);
            goto fail;
        }
#endif
        g_inotify_owner[watch.wd] = watch.instance;
        g_inotify_mask[watch.wd] = watch.mask;
        g_inotify_pending[watch.wd] = watch.pending;
        g_inotify_isdir[watch.wd] = (uint8_t)(watch.is_directory != 0);
        snprintf(g_inotify_wpath[watch.wd], sizeof g_inotify_wpath[watch.wd], "%s", watch.path);
        free(g_inotify_snap[watch.wd]);
        g_inotify_snap[watch.wd] = snapshot;
    }
    for (uint32_t index = 0; index < moves; index++) {
        struct ckpt_inotify_move move;
        if (ckpt_rd_all(file, &move, sizeof move) != 0 || move.wd < 0 || move.wd >= HL_NFD ||
            !g_inotify_owner[move.wd] || g_inomv_n >= (int)(sizeof g_inomv / sizeof g_inomv[0]))
            goto fail;
        g_inomv[g_inomv_n].wd = move.wd;
        g_inomv[g_inomv_n].mask = move.mask;
        g_inomv[g_inomv_n].cookie = move.cookie;
        snprintf(g_inomv[g_inomv_n].name, sizeof g_inomv[g_inomv_n].name, "%s", move.name);
        g_inomv_n++;
    }
    for (uint32_t index = 0; index < raw_instances; index++) {
        struct ckpt_inotify_raw raw;
        if (ckpt_rd_all(file, &raw, sizeof raw) != 0 || raw.instance < 0 || raw.instance >= HL_NFD ||
            !g_inotify[raw.instance] || raw.size > 16 * 1024 * 1024u)
            goto fail;
        uint8_t *bytes = malloc(raw.size ? raw.size : 1);
        if (!bytes || (raw.size && ckpt_rd_all(file, bytes, raw.size) != 0)) {
            free(bytes);
            goto fail;
        }
        free(g_inotify_raw[raw.instance]);
        g_inotify_raw[raw.instance] = bytes;
        g_inotify_raw_len[raw.instance] = raw.size;
        g_inotify_raw_pos[raw.instance] = 0;
    }
    if (!feof(file)) {
        int byte = fgetc(file);
        if (byte != EOF) goto fail;
    }
    ckpt_source_fclose(file);
    return 0;
fail:
    ckpt_source_fclose(file);
    return -1;
}

#if defined(HL_NATIVE_TEST_HOOKS)
// ANONYMOUS MAP_SHARED round trip -- the two mechanisms that keep such a region shared across a
// checkpoint, exercised without a guest.
//
// Scenario 0 (identity): the kernel names the object -- a shmem inode on Linux, a vm_object id on
// Darwin. A shared anonymous mapping must get an id, a sub-range of it must get the SAME id with
// the right offset, and a PRIVATE anonymous mapping must get NO id at all -- the discriminator that
// keeps every private region on its existing per-process restore.
//
// TWO HOST FACTS THIS ENCODES, both measured on macOS 26.3.1 (arm64) and neither true of Linux:
//
//  - An untouched region has no object yet. mmap(MAP_SHARED|MAP_ANON) with no page faulted reads
//    share_mode SM_EMPTY and object_id 0, so it has no identity to record -- correctly, since it
//    has no pages for anyone to share. The mappings are therefore written before they are named.
//  - Adjacent shared anonymous mappings COALESCE into one vm_object. Two 8 KiB mappings made back
//    to back landed in one entry with one object_id, the second at offset 0x4000. That is not a
//    collision: they really are one object, and the identity+offset pair restores both losslessly.
//    Linux mints a separate shmem inode per mmap, so only Linux can assert two distinct ids.
//
// Scenario 1 (one object, two processes): a parent and a forked child independently derive the
// (id, offset) pair for the region they share, then each takes the RESTORE-side seed for that id
// and maps it AT THAT OFFSET. The child writes; the parent must read the child's bytes back through
// its own mapping. Before the fix the same sequence produced two unrelated private copies, which is
// exactly what nine PostgreSQL members got.
//
// THE OFFSET IS PART OF THE ANSWER, NOT NOISE. This scenario used to demand offset == 0 and map the
// seed at 0, which is not what the restore does: memory_restore's region loop sizes the seed
// `adjusted_offset + map_len` and passes `map_offset = adjusted_offset`. On Darwin the demand is not
// merely stricter, it is wrong -- adjacent shared anonymous mappings coalesce into ONE vm_object, so
// a region legitimately starts partway in (measured at 0x4000) and the object is still the object.
// The old form therefore reported a broken restore whenever the host's layout happened to coalesce:
// 20/20 failures for this test alone on macOS 26.3.1 arm64, 1/20 when the sibling tests' threads
// changed the layout enough to leave the region at offset 0. Mirroring production's arithmetic is
// what makes the assertion about sharing rather than about allocator luck.
// A LEFTOVER SEGMENT FROM A CRASHED EARLIER RESTORE MUST NEVER BE ADOPTED.
//
// Constructed deliberately rather than waited for: a first generation seeds an object, writes a
// recognisable pattern into it and abandons the name exactly as a creator that leaves through `_exit`
// does. A second generation then asks for the SAME object id -- the collision the kernel hands out for
// free once an id is recycled -- and must come back with the captured bytes' object, never the stale
// one. Under the old (ipc_ns, object_id) name the two generations spelled the same name, so the second
// restore opened the abandoned segment: too small, it failed the restore with `ftruncate` EINVAL on
// Darwin (a POSIX shm object is sizeable once, by its creator); large enough, it was mapped and the
// guest resumed on stale memory with no diagnostic at all. This asserts the second outcome is gone.
static int ckpt_anon_shared_leftover_test(void) {
    const uint64_t object_id = 0x5eedc0dedeadbeefull;
    const uint64_t size = 8192;
    ckpt_anon_shared_generation_init();
    char stale_name[CKPT_ANON_SHARED_NAME_MAX];
    ckpt_anon_shared_name(object_id, stale_name);
    int stale = ckpt_restore_anon_shared_seed(object_id, size);
    if (stale < 0) return 30;
    unsigned char *stale_map = mmap(NULL, (size_t)size, PROT_READ | PROT_WRITE, MAP_SHARED, stale, 0);
    if (stale_map == MAP_FAILED) return 31;
    memset(stale_map, 0xAB, (size_t)size);
    munmap(stale_map, (size_t)size);
    // Abandon it the way a crashed member does: drop the descriptors, keep the NAME.
    ckpt_restore_backings_close();
    g_nanon_shared_unlink = 0;
    int abandoned = shm_open(stale_name, O_RDWR, 0600);
    if (abandoned < 0) { // the leftover must really exist, or the rest of this proves nothing
        shm_unlink(stale_name);
        return 32;
    }
    close(abandoned);

    // A NEW restore generation, the SAME recycled object id.
    g_anon_shared_generation = 0;
    ckpt_anon_shared_generation_init();
    char fresh_name[CKPT_ANON_SHARED_NAME_MAX];
    ckpt_anon_shared_name(object_id, fresh_name);
    int verdict = 0;
    int fresh = ckpt_restore_anon_shared_seed(object_id, size);
    if (fresh < 0) verdict = 34; // a leftover must not be able to FAIL the restore either
    unsigned char *fresh_map = fresh >= 0 ? (unsigned char *)mmap(NULL, (size_t)size, PROT_READ | PROT_WRITE,
                                                                 MAP_SHARED, fresh, 0)
                                          : (unsigned char *)MAP_FAILED;
    if (verdict == 0 && fresh_map == MAP_FAILED) verdict = 35;
    if (verdict == 0) {
        for (size_t i = 0; i < (size_t)size; i++)
            if (fresh_map[i] != 0) { // stale content in place of the captured bytes: the worst outcome
                verdict = 36;
                break;
            }
    }
    if (fresh_map != MAP_FAILED) munmap(fresh_map, (size_t)size);
    // Checked AFTER the bytes, deliberately: the content is the guarantee and the name is the
    // mechanism, so a regression reports the wrong memory rather than the spelling that caused it.
    if (verdict == 0 && strcmp(fresh_name, stale_name) == 0) verdict = 33;
    ckpt_restore_backings_close();
    ckpt_anon_shared_unlink_all();
    shm_unlink(stale_name);
    return verdict;
}

static int ckpt_anon_shared_roundtrip_test(uint32_t scenario) {
    const size_t length = 8192;
    if (scenario == 0) {
        void *first = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        void *second = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        void *private_region = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        if (first == MAP_FAILED || second == MAP_FAILED || private_region == MAP_FAILED) return 10;
        // Fault every page in before naming the objects: on Darwin an untouched region has no
        // vm_object and therefore no identity. Harmless on Linux, where the inode exists at mmap.
        memset(first, 0x11, length);
        memset(second, 0x22, length);
        memset(private_region, 0x33, length);
        ckpt_anon_shared_scan();
        uint64_t first_id = 0, second_id = 0, private_id = 0, tail_id = 0;
        uint64_t first_offset = 0, second_offset = 0, private_offset = 0, tail_offset = 0;
        int verdict = 0;
        if (g_anon_shared_truncated) verdict = 11;
        // The offset is asserted RELATIVE to the region's own offset, never as zero. Linux mints one
        // shmem inode per mmap so the region always starts its object, but Darwin coalesces adjacent
        // shared anonymous mappings into one vm_object and a region can start partway into it --
        // measured at offset 0x4000. What must hold on both hosts is that the offset tracks the
        // address, which the sub-range check below is what actually proves.
        else if (!ckpt_anon_shared_object((uint64_t)(uintptr_t)first, length, &first_id, &first_offset) ||
                 first_id == 0)
            verdict = 12;
        else if (!ckpt_anon_shared_object((uint64_t)(uintptr_t)second, length, &second_id, &second_offset) ||
                 second_id == 0)
            verdict = 13;
#if !defined(__APPLE__)
        // Linux mints one shmem inode per mmap, so two mappings are two objects. Darwin coalesces
        // adjacent shared anonymous mappings into one, which the offsets below already carry.
        else if (first_id == second_id) verdict = 14; // two objects must not share one identity
#endif
        else if (!ckpt_anon_shared_object((uint64_t)(uintptr_t)first + 4096, 4096, &tail_id, &tail_offset) ||
                 tail_id != first_id || tail_offset != first_offset + 4096)
            verdict = 15; // a sub-range of one object is the SAME object at an offset
        else if (ckpt_anon_shared_object((uint64_t)(uintptr_t)private_region, length, &private_id, &private_offset) ||
                 private_id != 0)
            verdict = 16; // MAP_PRIVATE anonymous keeps its per-process restore
        munmap(first, length);
        munmap(second, length);
        munmap(private_region, length);
        return verdict;
    }
    if (scenario == 2) {
        // In its own process: the scenario deliberately retires one naming generation and mints
        // another, and that generation plus the seed table are process-global restore state a sibling
        // test running in the same binary would otherwise see change underneath it.
        pid_t generation_child = hl_host_process_clone_current();
        if (generation_child == 0) _exit(ckpt_anon_shared_leftover_test());
        if (generation_child < 0) return 39;
        int generation_status = 0;
        if (waitpid(generation_child, &generation_status, 0) != generation_child) return 39;
        if (!WIFEXITED(generation_status)) return 39;
        return WEXITSTATUS(generation_status);
    }
    if (scenario != 1) return 99;
    ckpt_anon_shared_generation_init();
    // What each process independently derives for the region, and what both must agree on.
    struct ckpt_anon_shared_named {
        uint64_t identity;
        uint64_t offset;
    };
    unsigned char *shared = mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (shared == MAP_FAILED) return 20;
    memset(shared, 0, length);
    memcpy(shared, "PARENT", 6);
    int channel[2];
    if (pipe(channel) != 0) {
        munmap(shared, length);
        return 21;
    }
    pid_t child = hl_host_process_clone_current();
    if (child == 0) {
        // Async-signal-safe only: no allocation, no locking, no stdio. _exit, never return.
        close(channel[0]);
        struct ckpt_anon_shared_named named = {0, 0};
        ckpt_anon_shared_scan();
        if (!ckpt_anon_shared_object((uint64_t)(uintptr_t)shared, length, &named.identity, &named.offset))
            named.identity = 0;
        int seed = named.identity != 0 ? ckpt_restore_anon_shared_seed(named.identity, named.offset + length) : -1;
        unsigned char *restored = seed >= 0 ? mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, seed,
                                                   (off_t)named.offset)
                                            : MAP_FAILED;
        if (restored != MAP_FAILED) memcpy(restored + 4096, "CHILD", 5);
        if (restored == MAP_FAILED) named.identity = 0;
        ssize_t ignored = write(channel[1], &named, sizeof named);
        (void)ignored;
        close(channel[1]);
        _exit(0);
    }
    close(channel[1]);
    int verdict = 0;
    struct ckpt_anon_shared_named from_child = {0, 0};
    if (child < 0) verdict = 22;
    else {
        size_t read_bytes = 0;
        while (read_bytes < sizeof from_child) {
            ssize_t got = read(channel[0], (unsigned char *)&from_child + read_bytes, sizeof from_child - read_bytes);
            if (got <= 0) break;
            read_bytes += (size_t)got;
        }
        if (read_bytes != sizeof from_child || from_child.identity == 0) verdict = 23;
    }
    close(channel[0]);
    int status = 0;
    if (child > 0) waitpid(child, &status, 0);
    struct ckpt_anon_shared_named here = {0, 0};
    ckpt_anon_shared_scan();
    if (verdict == 0 &&
        (!ckpt_anon_shared_object((uint64_t)(uintptr_t)shared, length, &here.identity, &here.offset) ||
         here.identity == 0))
        verdict = 24;
    // The two processes must have named the SAME object at the SAME offset without any engine-side
    // coordination. The offset is asserted because it is what the restore maps at: two processes that
    // agreed on the id but not the offset would map two disjoint windows of one object and lose the
    // sharing just as completely as two private copies would.
    if (verdict == 0 && (here.identity != from_child.identity || here.offset != from_child.offset)) verdict = 25;
    unsigned char *restored = MAP_FAILED;
    if (verdict == 0) {
        int seed = ckpt_restore_anon_shared_seed(here.identity, here.offset + length);
        restored = seed >= 0
                       ? mmap(NULL, length, PROT_READ | PROT_WRITE, MAP_SHARED, seed, (off_t)here.offset)
                       : MAP_FAILED;
        if (restored == MAP_FAILED) verdict = 26;
    }
    // The child's write, made through ITS OWN mapping of the restored object, is visible here.
    if (verdict == 0 && memcmp(restored + 4096, "CHILD", 5) != 0) verdict = 27;
    if (restored != MAP_FAILED) munmap(restored, length);
    char name[CKPT_ANON_SHARED_NAME_MAX];
    if (verdict == 0) ckpt_anon_shared_name(here.identity, name);
    ckpt_restore_backings_close();
    ckpt_anon_shared_unlink_all();
    // THE SEGMENT DID NOT OUTLIVE THE RESTORE THAT MADE IT. Its creator was the child, and the child
    // left through `_exit`, which runs no atexit handler -- so the old creator-only unlink list never
    // fired and the name survived into the next run, where a recycled object id could adopt it. The
    // name is derivable by every sharer, so the parent retires it on the creator's behalf.
    if (verdict == 0) {
        int leftover = shm_open(name, O_RDWR, 0600);
        if (leftover >= 0) {
            close(leftover);
            shm_unlink(name);
            verdict = 28;
        }
    }
    munmap(shared, length);
    return verdict;
}

HL_API int HL_TARGET_LOCAL(checkpoint_anon_shared_test)(uint32_t scenario) {
    return ckpt_anon_shared_roundtrip_test(scenario);
}
#endif
