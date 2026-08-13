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
        fprintf(stderr, "[restore] open %s: %s\n", pf, strerror(errno));
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
}

// Rebuild this process's guest memory (MAP_FIXED) + the mapping side-registries from `procdir`. For the init
// this runs BEFORE engine init (so MAP_FIXED lands on free VAs); a re-forked child calls hl_gmap_reset() +
// clears the anon/gna counters FIRST (dropping the COW-inherited init mappings) so its own RAM lands clean.
static int ckpt_restore_mem_dir(const char *procdir, const struct ckpt_meta *m) {
    uint64_t *mapped = NULL;
    struct ckpt_region *topology = NULL;
    uint64_t *mapped_a;
    uint64_t *mapped_e;
    size_t nmapped = 0;
    jit_guest_soft_restore_deactivate();
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/pages", procdir);
    FILE *f = ckpt_source_fopen(pf);
    if (!f) {
        fprintf(stderr, "[restore] open %s: %s\n", pf, strerror(errno));
        return -1;
    }
    if (m->n_regions > SIZE_MAX / (2u * sizeof(*mapped))) {
        ckpt_source_fclose(f);
        return -1;
    }
    if (m->n_regions != 0) {
        mapped = calloc((size_t)m->n_regions * 2u, sizeof(*mapped));
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
    for (uint64_t i = 0; i < m->n_regions; i++) {
        struct ckpt_region reg;
        if (ckpt_read_region(f, &reg) != 0) { goto fail; }
        if (reg.format_version != CKPT_REGION_VERSION || reg.logical > 1) {
            fprintf(stderr, "[restore] invalid region format=%u logical=%u\n", reg.format_version, reg.logical);
            goto fail;
        }
        topology[i] = reg;
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
            int map_flags = MAP_FIXED | MAP_ANON | MAP_PRIVATE;
            int map_fd = -1;
            off_t map_offset = 0;
            if (reg.backing_object != 0 && !reg.backing_emulated) {
                if (reg.backing_offset > UINT64_MAX - reg.len) goto fail;
                map_fd = ckpt_restore_backing_seed(procdir, reg.backing_object, reg.backing_offset + reg.len);
                if (map_fd < 0) {
                    fprintf(stderr, "[restore] cannot prepare backing object %llx\n",
                            (unsigned long long)reg.backing_object);
                    goto fail;
                }
                map_flags = MAP_FIXED | (reg.backing_shared ? MAP_SHARED : MAP_PRIVATE);
                map_offset = (off_t)reg.backing_offset;
            }
            // A guest mmap's VA is an ordinary host mmap result, so a saved region can name VA the restoring
            // process is already using for ENGINE state -- and MAP_FIXED would replace it silently. Probe
            // with MAP_FIXED_NOREPLACE first so the collision is named; the retry keeps the guest's VA (the
            // guest's own pointers are unrelocatable), but a corrupted engine is now diagnosed, not silent.
#ifdef MAP_FIXED_NOREPLACE
            int probe_flags = map_flags | MAP_FIXED_NOREPLACE;
#else
            int probe_flags = map_flags;
#endif
            void *r = mmap((void *)a, (size_t)reg.len, PROT_READ | PROT_WRITE, probe_flags, map_fd, map_offset);
            if (r == MAP_FAILED || (uint64_t)(uintptr_t)r != a) {
                if (r != MAP_FAILED) munmap(r, (size_t)reg.len);
                fprintf(stderr, "[restore] guest region %llx+%llx overlaps a live host mapping; reclaiming it\n",
                        (unsigned long long)a, (unsigned long long)reg.len);
                ckpt_report_overlap(a, e);
                r = mmap((void *)a, (size_t)reg.len, PROT_READ | PROT_WRITE, map_flags, map_fd, map_offset);
            }
            if (r == MAP_FAILED || (uint64_t)(uintptr_t)r != a) {
                fprintf(stderr, "[restore] cannot map guest region %llx+%llx: %s\n", (unsigned long long)a,
                        (unsigned long long)reg.len, strerror(errno));
                goto fail;
            }
            mapped_a[nmapped] = a;
            mapped_e[nmapped] = e;
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
    }
    ckpt_source_fclose(f);
    for (uint64_t i = 0; i < m->n_regions; i++) {
        struct ckpt_region *reg = &topology[i];
        if (reg->backing_object == 0) continue;
        if (reg->backing_offset > UINT64_MAX - reg->glen) {
            free(mapped);
            free(topology);
            return -1;
        }
        int seed = ckpt_restore_backing_seed(procdir, reg->backing_object, reg->backing_offset + reg->glen);
        if (seed < 0) {
            fprintf(stderr, "[restore] cannot rebuild backing object %llx\n", (unsigned long long)reg->backing_object);
            free(mapped);
            free(topology);
            return -1;
        }
        filemap_register(reg->addr, reg->glen, seed, reg->backing_offset, reg->backing_shared, reg->backing_emulated);
        if (reg->backing_shared && !reg->backing_emulated)
            futex_shared_register(reg->addr, reg->glen, seed, reg->backing_offset);
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
    ckpt_source_fclose(f);
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
    if (stored < (int64_t)sizeof(struct ckpt_epoll_header)) return -1;
    size_t size = (size_t)stored;
    unsigned char *image = malloc(size);
    if (image == NULL || ckpt_source_load(path, image, size) != 0) {
        free(image);
        return -1;
    }
    struct ckpt_epoll_header header;
    memcpy(&header, image, sizeof header);
    if (header.magic != CKPT_EPOLL_MAGIC || header.count > (size - sizeof header) / sizeof(struct ckpt_epoll_watch) ||
        sizeof header + (size_t)header.count * sizeof(struct ckpt_epoll_watch) != size) {
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
