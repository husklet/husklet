// Kernel-AIO (libaio) family: io_setup / io_destroy / io_submit / io_cancel / io_getevents.
// Canonical (aarch64) numbers 0-4; the x86-64 forms 206-210 are already remapped onto these by
// sysmap.h (206->0,207->1,208->4,209->2,210->3), so this switch only sees the canonical numbers.
// Returns 1 if nr was handled, 0 otherwise. Included by dispatch.c AFTER io.c/vfs.c (eventfd tables,
// memf_materialize/fd_evict) and thread.c (host_range_mapped) -- same TU scope.
//
// macOS has no libaio / kernel-AIO. nginx:alpine (and mysql/mariadb innodb) call io_setup at worker
// startup and treat ENOSYS as FATAL ("io_setup() failed"), so an unhandled syscall kills every worker.
// We emulate SYNCHRONOUSLY: io_submit performs each I/O (pread/pwrite/preadv/pwritev/fsync) IMMEDIATELY
// at the given offset and queues the completion; io_getevents just drains the already-full queue. This is
// semantically valid AIO (a completion may arrive any time after submit, including instantly) and is all
// nginx/innodb require -- they submit, then epoll/io_getevents to reap. The eventfd (IOCB_FLAG_RESFD)
// path is honoured so nginx's epoll-on-eventfd wakes right after submission.

// struct iocb (LP64 <linux/aio_abi.h>, 64 bytes). Fixed byte offsets so the layout is exact regardless
// of host struct packing:
//   0  u64 aio_data     (echoed back in io_event.data)
//   8  u32 aio_key
//   12 u32 aio_rw_flags
//   16 u16 aio_lio_opcode
//   18 s16 aio_reqprio
//   20 u32 aio_fildes
//   24 u64 aio_buf      (read/write buffer, or iovec array for PREADV/PWRITEV)
//   32 u64 aio_nbytes   (byte count, or iovec count for PREADV/PWRITEV)
//   40 s64 aio_offset
//   48 u64 aio_reserved2
//   56 u32 aio_flags    (IOCB_FLAG_RESFD=1, IOCB_FLAG_IOPRIO=2)
//   60 u32 aio_resfd    (eventfd to signal when IOCB_FLAG_RESFD set)
#define IOCB_CMD_PREAD 0
#define IOCB_CMD_PWRITE 1
#define IOCB_CMD_FSYNC 2
#define IOCB_CMD_FDSYNC 3
#define IOCB_CMD_PREADV 7
#define IOCB_CMD_PWRITEV 8
#define IOCB_FLAG_RESFD 1

// struct io_event (LP64, 32 bytes): { u64 data; u64 obj; s64 res; s64 res2; }.
struct aio_evt {
    uint64_t data, obj;
    int64_t res, res2;
};

// Engine-side AIO context. The "context id" handed back to the guest (io_setup's *ctx_idp) is the ADDRESS
// of one of these table entries. In dd's in-process model the guest shares this address space, so it can
// pass the value back to us; we always VALIDATE it against the table before use (a bogus ctx -> -EINVAL),
// never blind-deref. NOTE on the libaio userspace fast path: libaio's io_getevents reads (aio_ring*)ctx
// and, only if the u32 at offset 16 equals AIO_RING_MAGIC (0xa10a10a1), drains events in userspace and
// skips the syscall. Our struct's offset 16 is `head` (a small ring index, never that magic), so libaio
// always mismatches and falls through to the real io_getevents syscall handled here. Programs using raw
// syscalls (nginx) never inspect ctx at all.
struct aio_ctx {
    struct aio_evt *q; // off 0: completion ring (malloc'd, `cap` entries)
    int used;          // off 8
    int cap;           // off 12
    int head, tail, n; // off 16/20/24: ring head/tail and queued count
};

#define AIO_MAX_CTX 64
static struct aio_ctx g_aioctx[AIO_MAX_CTX];
// Guards the g_aioctx table slot allocation/free AND every ctx completion-ring mutation. io_submit
// (aio_push) and io_getevents (aio_drain) run on DIFFERENT guest threads against the same ctx (InnoDB
// submits from worker threads, reaps from dedicated io-handler threads), so the ring head/tail/n must
// not be mutated concurrently -- an unlocked race duplicates/loses a completion, handing InnoDB a bogus
// io_event.obj it then dereferences (a load-gated SIGSEGV/corruption source). Never held across the
// actual I/O (aio_do_one) so concurrent InnoDB I/O is not serialized.
static pthread_mutex_t g_aio_lock = PTHREAD_MUTEX_INITIALIZER;

// Resolve+validate a guest-supplied aio_context_t (a pointer into g_aioctx) to its table entry, or NULL.
static struct aio_ctx *aio_ctx_of(uint64_t id) {
    for (int i = 0; i < AIO_MAX_CTX; i++)
        if (g_aioctx[i].used && (uint64_t)(uintptr_t)&g_aioctx[i] == id) return &g_aioctx[i];
    return NULL;
}

// Queue one completion into ctx's ring (drops the oldest if full -- can't happen for well-behaved callers
// that io_getevents before re-submitting past nr_events, but stays bounded regardless).
static void aio_push(struct aio_ctx *x, uint64_t data, uint64_t obj, int64_t res) {
    pthread_mutex_lock(&g_aio_lock);
    if (x->n >= x->cap) { // overflow: advance head to make room (drop oldest)
        x->head = (x->head + 1) % x->cap;
        x->n--;
    }
    x->q[x->tail].data = data;
    x->q[x->tail].obj = obj;
    x->q[x->tail].res = res;
    x->q[x->tail].res2 = 0;
    x->tail = (x->tail + 1) % x->cap;
    x->n++;
    pthread_mutex_unlock(&g_aio_lock);
}

// Drain up to `max` completions from ctx `x` into the guest io_event buffer `ev` (32 bytes each),
// returning the count moved. Locked (pairs with aio_push).
static long aio_drain(struct aio_ctx *x, uint8_t *ev, long max) {
    long got = 0;
    pthread_mutex_lock(&g_aio_lock);
    long want = max < x->n ? max : x->n;
    while (got < want) {
        struct aio_evt *e = &x->q[x->head];
        uint8_t *o = ev + (size_t)got * 32;
        *(uint64_t *)(o + 0) = e->data;
        *(uint64_t *)(o + 8) = e->obj;
        *(int64_t *)(o + 16) = e->res;
        *(int64_t *)(o + 24) = e->res2;
        x->head = (x->head + 1) % x->cap;
        x->n--;
        got++;
    }
    pthread_mutex_unlock(&g_aio_lock);
    return got;
}

// Signal an AIO completion eventfd (aio_resfd): mirror io.c's eventfd write path exactly -- bump the
// accumulating counter and regenerate a single fresh readable edge on the backing pipe so a blocked/
// edge-triggered epoll_wait on the eventfd wakes. No-op for a non-eventfd / out-of-range fd.
static void aio_eventfd_kick(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_eventfd_peer[fd]) return;
    int eslot = eventfd_counter_slot(fd);
    // Same counter+pipe atomicity as io.c's eventfd write (see _eventfd-atomicity_): hold g_eventfd_lock
    // across the bump + drain + re-signal so an AIO completion never races the guest's read()/write().
    pthread_mutex_lock(&g_eventfd_lock);
    g_eventfd_count[eslot] += 1;
    // The read end is permanently O_NONBLOCK, so drain to one fresh byte with no flag toggle (the old
    // toggle mutated the cross-process-shared fd flags — see io.c / vfs.c g_eventfd_gnb).
    char buf[64];
    while (read(fd, buf, sizeof buf) > 0) {}
    char b = 1;
    if (write(g_eventfd_peer[fd] - 1, &b, 1) < 0) {}
    pthread_mutex_unlock(&g_eventfd_lock);
}

// Perform ONE iocb synchronously; returns the io_event.res value (bytes transferred, 0 for fsync, or a
// negative Linux errno). `iocb` is an already-validated 64-byte guest struct.
static int64_t aio_do_one(const uint8_t *iocb) {
    uint16_t op = *(const uint16_t *)(iocb + 16);
    int fd = (int)*(const uint32_t *)(iocb + 20);
    uint64_t buf = *(const uint64_t *)(iocb + 24);
    uint64_t nbytes = *(const uint64_t *)(iocb + 32);
    int64_t off = *(const int64_t *)(iocb + 40);
    memf_materialize(fd); // flush any RAM-backed cache so the real host fd sees/serves the right bytes
    ssize_t r;
    switch (op) {
    case IOCB_CMD_PREAD:
        if (nbytes && !host_range_mapped((uintptr_t)buf, (size_t)nbytes)) return -EFAULT;
        r = pread(fd, (void *)buf, (size_t)nbytes, (off_t)off);
        return r < 0 ? -errno : r;
    case IOCB_CMD_PWRITE:
        if (nbytes && !host_range_mapped((uintptr_t)buf, (size_t)nbytes)) return -EFAULT;
        fd_evict(fd);
        r = pwrite(fd, (const void *)buf, (size_t)nbytes, (off_t)off);
        return r < 0 ? -errno : r;
    case IOCB_CMD_PREADV:
    case IOCB_CMD_PWRITEV: {
        int niov = (int)nbytes; // for the *V ops aio_nbytes IS the iovec count, aio_buf the array base
        if (niov > 0 && !host_range_mapped((uintptr_t)buf, (size_t)niov * sizeof(struct iovec))) return -EFAULT;
        if (op == IOCB_CMD_PWRITEV) fd_evict(fd);
        r = op == IOCB_CMD_PREADV ? preadv(fd, (const struct iovec *)buf, niov, (off_t)off)
                                  : pwritev(fd, (const struct iovec *)buf, niov, (off_t)off);
        return r < 0 ? -errno : r;
    }
    case IOCB_CMD_FSYNC:
    case IOCB_CMD_FDSYNC: return fsync(fd) < 0 ? -errno : 0;
    default: return -EINVAL; // unsupported opcode (POLL/NOOP)
    }
}

static int svc_aio(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                   uint64_t a5) {
    (void)a5;
    switch (nr) {
    case 0: { // io_setup(unsigned nr_events, aio_context_t *ctx_idp)
        unsigned nr_events = (unsigned)a0;
        if (!a1 || !host_range_mapped((uintptr_t)a1, sizeof(uint64_t))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (*(uint64_t *)a1 != 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // Linux: *ctx_idp must be 0
        if (nr_events == 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // Linux over-allocates the completion ring vs nr_events; a small headroom keeps a burst of
        // submissions from dropping completions before io_getevents drains them.
        int cap = (int)nr_events + 1;
        if (cap < 8) cap = 8;
        struct aio_evt *q = calloc((size_t)cap, sizeof *q);
        if (!q) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        // Reserve a slot under the lock so two concurrent io_setup calls never grab the same entry.
        pthread_mutex_lock(&g_aio_lock);
        int slot = -1;
        for (int i = 0; i < AIO_MAX_CTX; i++)
            if (!g_aioctx[i].used) {
                slot = i;
                break;
            }
        if (slot >= 0) {
            g_aioctx[slot].q = q;
            g_aioctx[slot].cap = cap;
            g_aioctx[slot].head = g_aioctx[slot].tail = g_aioctx[slot].n = 0;
            g_aioctx[slot].used = 1;
        }
        pthread_mutex_unlock(&g_aio_lock);
        if (slot < 0) {
            free(q);
            G_RET(c) = (uint64_t)(-EAGAIN);
            break;
        } // out of contexts (matches kernel ENOMEM/EAGAIN)
        *(uint64_t *)a1 = (uint64_t)(uintptr_t)&g_aioctx[slot];
        G_RET(c) = 0;
        break;
    }
    case 1: { // io_destroy(aio_context_t ctx)
        struct aio_ctx *x = aio_ctx_of(a0);
        if (!x) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // Under the lock so a concurrent io_getevents mid-drain never touches a freed ring.
        pthread_mutex_lock(&g_aio_lock);
        free(x->q);
        x->q = NULL;
        x->n = x->head = x->tail = 0; // so a racing aio_drain sees an empty ring, never derefs freed q
        x->used = 0;
        pthread_mutex_unlock(&g_aio_lock);
        G_RET(c) = 0;
        break;
    }
    case 2: { // io_submit(aio_context_t ctx, long nr, struct iocb **iocbpp)
        struct aio_ctx *x = aio_ctx_of(a0);
        if (!x) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        long count = (long)a1;
        if (count < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (count == 0) {
            G_RET(c) = 0;
            break;
        }
        // iocbpp is an array of `count` guest pointers (u64 each).
        if (!host_range_mapped((uintptr_t)a2, (size_t)count * sizeof(uint64_t))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        const uint64_t *pp = (const uint64_t *)a2;
        long done = 0;
        for (long i = 0; i < count; i++) {
            uint64_t iocb = pp[i];
            if (!iocb || !host_range_mapped((uintptr_t)iocb, 64))
                break; // stop; report count so far (or EFAULT if first)
            const uint8_t *cb = (const uint8_t *)iocb;
            uint64_t aio_data = *(const uint64_t *)(cb + 0);
            uint32_t aio_flags = *(const uint32_t *)(cb + 56);
            uint32_t aio_resfd = *(const uint32_t *)(cb + 60);
            int64_t res = aio_do_one(cb);
            aio_push(x, aio_data, iocb, res);
            if (aio_flags & IOCB_FLAG_RESFD) aio_eventfd_kick((int)aio_resfd);
            done++;
        }
        // Linux io_submit returns the number of iocbs submitted, or -errno only if NONE were.
        G_RET(c) = done > 0 ? (uint64_t)done : (uint64_t)(-EFAULT);
        break;
    }
    case 3: // io_cancel(aio_context_t ctx, struct iocb *, struct io_event *)
        // Every submission already completed synchronously, so nothing is ever in flight to cancel:
        // -EINVAL is the kernel's "not cancellable / already complete" answer either way.
        G_RET(c) = (uint64_t)(-EINVAL);
        break;
    case 4: { // io_getevents(aio_context_t ctx, long min_nr, long nr, struct io_event *events, struct timespec *tmo)
        struct aio_ctx *x = aio_ctx_of(a0);
        if (!x) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        long min_nr = (long)a1;
        long nr_max = (long)a2;
        if (nr_max < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint8_t *ev = (uint8_t *)a3;
        // Validate the FULL requested buffer up front (a blocking reap may fill up to nr_max events).
        if (nr_max > 0 && (!ev || !host_range_mapped((uintptr_t)a3, (size_t)nr_max * 32))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        long got = aio_drain(x, ev, nr_max);
        // BLOCK (bounded) when the caller wanted min_nr>got. Our AIO completes synchronously, but a
        // completion can still be pushed by a *concurrent* guest thread's io_submit into this same ctx.
        // Returning 0 instantly made InnoDB's io-handler thread busy-spin at 100% CPU on io_getevents;
        // under load that spinner starves the shutdown thread it waits on and the whole process hangs
        // forever (mariadb initdb, #305 -- uncontended runs won the scheduler race and exited, which is
        // why it was intermittent/load-gated). Poll-sleep up to the guest timeout like a real kernel so
        // the reaper sleeps instead of spinning; the timeout is capped so shutdown stays responsive and
        // a NULL ("block forever") timeout can never wedge the engine.
        if (min_nr > 0 && got < min_nr) {
            long long budget_ns = 50LL * 1000000LL; // NULL timeout -> block, but return periodically
            if (a4 && host_range_mapped((uintptr_t)a4, 16)) {
                long long ts_sec = *(const int64_t *)(uintptr_t)a4;
                long long ts_nsec = *(const int64_t *)((uintptr_t)a4 + 8);
                long long req = ts_sec * 1000000000LL + ts_nsec;
                if (req < budget_ns) budget_ns = req < 0 ? 0 : req; // honor a shorter guest timeout
            }
            long long waited = 0;
            while (got < min_nr && waited < budget_ns) {
                struct timespec slice = {0, 1000000}; // 1 ms poll granularity
                nanosleep(&slice, NULL);
                waited += 1000000;
                got += aio_drain(x, ev + (size_t)got * 32, nr_max - got);
            }
        }
        G_RET(c) = (uint64_t)got;
        break;
    }
    default: return 0;
    }
    return svc_done(c);
}
