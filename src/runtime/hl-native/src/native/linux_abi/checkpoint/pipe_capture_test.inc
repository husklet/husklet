// ------------------------------------------------------- pipe capture under the freeze: behavioral fixture
//
// Drives the REAL ckpt_capture_pipe_reason against a REAL kernel pipe through a sink that implements only
// the two operations the pipe path uses -- the image-wide claim and a byte stream -- so the election, the
// drain and the abort contract are exercised rather than modelled. The claim table and the drained bytes
// live in one MAP_SHARED page, which is what lets scenario 0 fork and still arbitrate a single election
// across processes that inherited the same pipe end, the postmaster/backend shape.

// The refill half of the round trip is defined later in the unity translation unit (resource_restore.c),
// where restore lives. The fixture drives capture and refill against each other, so it needs both.
static int ckpt_refill_restore_pipe(int writer, uint64_t identity);

#define CKPT_PIPE_TEST_PAYLOAD 24000u

struct ckpt_pipe_test_shared {
    _Atomic int claim;
    _Atomic int winners;
    _Atomic int losers;
    _Atomic unsigned length;
    _Atomic int pause_write;
    _Atomic int write_entered;
    _Atomic int release_write;
    unsigned char bytes[CKPT_PIPE_TEST_PAYLOAD];
};

static struct ckpt_pipe_test_shared *g_ckpt_pipe_test_shared;

static int ckpt_pipe_test_begin(struct ckpt_sink *sink, const char *group, const char *name, uint32_t flags,
                                struct ckpt_sink_stream **out) {
    (void)group;
    (void)name;
    (void)flags;
    // No allocation: scenario 0 runs this inside a fork of a threaded test process, where malloc's lock
    // may have been held by another thread at the instant of the fork.
    static struct ckpt_sink_stream stream;
    memset(&stream, 0, sizeof stream);
    stream.sink = sink;
    *out = &stream;
    return 0;
}
static int ckpt_pipe_test_write(struct ckpt_sink_stream *stream, const void *data, size_t size) {
    (void)stream;
    struct ckpt_pipe_test_shared *shared = g_ckpt_pipe_test_shared;
    if (atomic_load(&shared->pause_write)) {
        atomic_store(&shared->write_entered, 1);
        while (!atomic_load(&shared->release_write)) {
            struct timespec span = {0, 1000000};
            nanosleep(&span, NULL);
        }
    }
    unsigned at = atomic_load(&shared->length);
    if (size > sizeof shared->bytes - at) return -1;
    memcpy(shared->bytes + at, data, size);
    atomic_store(&shared->length, at + (unsigned)size);
    return 0;
}

static int ckpt_pipe_test_finish(struct ckpt_sink_stream *stream) {
    (void)stream;
    atomic_fetch_add(&g_ckpt_pipe_test_shared->winners, 1);
    return 0;
}

static void ckpt_pipe_test_abort(struct ckpt_sink_stream *stream) {
    (void)stream;
}

static int ckpt_pipe_test_claim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    (void)name;
    int free_slot = 0;
    if (atomic_compare_exchange_strong(&g_ckpt_pipe_test_shared->claim, &free_slot, 1)) return 0;
    atomic_fetch_add(&g_ckpt_pipe_test_shared->losers, 1);
    return 1;
}

static void ckpt_pipe_test_unclaim(struct ckpt_sink *sink, const char *name) {
    (void)sink;
    (void)name;
    atomic_store(&g_ckpt_pipe_test_shared->claim, 0);
}

static const ckpt_sink_vtable g_ckpt_pipe_test_ops = {
    .begin = ckpt_pipe_test_begin,
    .write = ckpt_pipe_test_write,
    .finish = ckpt_pipe_test_finish,
    .abort = ckpt_pipe_test_abort,
    .claim = ckpt_pipe_test_claim,
    .unclaim = ckpt_pipe_test_unclaim,
};

// The same shared page read back as an image source, so the restore refill consumes exactly the object the
// capture drain published rather than a hand-built copy of it.
static int64_t ckpt_pipe_test_source_size(struct ckpt_source *source, const char *name) {
    (void)source;
    (void)name;
    return (int64_t)atomic_load(&g_ckpt_pipe_test_shared->length);
}

static int64_t ckpt_pipe_test_source_read(struct ckpt_source *source, const char *name, uint64_t offset, void *out,
                                          size_t size) {
    (void)source;
    (void)name;
    unsigned length = atomic_load(&g_ckpt_pipe_test_shared->length);
    if (offset > length || size > length - offset) return -1;
    memcpy(out, g_ckpt_pipe_test_shared->bytes + offset, size);
    return (int64_t)size;
}

static const ckpt_source_vtable g_ckpt_pipe_test_source_ops = {
    .size = ckpt_pipe_test_source_size,
    .read = ckpt_pipe_test_source_read,
};

static int ckpt_pipe_test_open_shared(void) {
    void *page =
        mmap(NULL, sizeof(struct ckpt_pipe_test_shared), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) return -1;
    memset(page, 0, sizeof(struct ckpt_pipe_test_shared));
    g_ckpt_pipe_test_shared = page;
    return 0;
}

static void ckpt_pipe_test_close_shared(void) {
    if (g_ckpt_pipe_test_shared) munmap(g_ckpt_pipe_test_shared, sizeof(struct ckpt_pipe_test_shared));
    g_ckpt_pipe_test_shared = NULL;
}

// The bytes must come back in order, so make every byte a function of its position.
static unsigned char ckpt_pipe_test_byte(unsigned index) {
    return (unsigned char)(index * 7u + 3u);
}

static int ckpt_pipe_test_fill(int writer) {
    for (unsigned at = 0; at < CKPT_PIPE_TEST_PAYLOAD;) {
        unsigned char block[4096];
        unsigned size = CKPT_PIPE_TEST_PAYLOAD - at < sizeof block ? CKPT_PIPE_TEST_PAYLOAD - at : sizeof block;
        for (unsigned index = 0; index < size; ++index)
            block[index] = ckpt_pipe_test_byte(at + index);
        ssize_t written = write(writer, block, size);
        if (written <= 0) return -1;
        at += (unsigned)written;
    }
    return 0;
}

static int ckpt_pipe_test_buffered(int fd) {
    int pending = -1;
    return ioctl(fd, FIONREAD, &pending) == 0 ? pending : -1;
}

// A two-descriptor walk in ascending guest-fd order, driven through the SAME production driver
// ckpt_scan_fds uses: a drainable pipe first, then a descriptor that is refused. The refusal is the real
// one -- ckpt_capture_native_fd rejects a socket outright -- and it consumes nothing.
struct ckpt_pipe_test_scan {
    int pipe_fd;
    uint64_t identity;
    int refused_fd;
};

static int ckpt_pipe_test_scan_pass(void *context) {
    struct ckpt_pipe_test_scan *walk = context;
    if (ckpt_capture_pipe_reason(walk->pipe_fd, walk->identity, NULL, NULL) != 0) return -1;
    hl_host_process_fd detail;
    char path[512];
    size_t path_size = 0;
    if (!hl_host_process_fd_read(getpid(), walk->refused_fd, &detail, path, sizeof(path) - 1, &path_size)) return -1;
    if (detail.kind == HL_HOST_FD_SOCKET) return -1; // the same arm ckpt_capture_native_fd takes
    return 0;
}

static int ckpt_pipe_test_refused_scan(int pipe_fd, uint64_t identity, int refused_fd) {
    struct ckpt_pipe_test_scan walk = {pipe_fd, identity, refused_fd};
    return ckpt_admit_then_consume(ckpt_pipe_test_scan_pass, &walk);
}

HL_API int HL_TARGET_LOCAL(checkpoint_pipe_capture_test)(uint32_t scenario) {
    struct ckpt_sink *saved_sink = ckpt_sink_current();
    int saved_destructive = g_ckpt_capture_destructive;
    int verdict = 99;
    int pair[2] = {-1, -1};

    if (scenario == 0) {
        // An inherited pipe held by three processes: exactly one drains, and the bytes it publishes are the
        // bytes that were written, in order.
        if (ckpt_pipe_test_open_shared() != 0) return 10;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 11;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 12;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        pid_t children[2] = {-1, -1};
        for (int index = 0; verdict == 0 && index < 2; ++index) {
            children[index] = hl_host_process_clone_current();
            if (children[index] == 0) {
                g_ckpt_capture_destructive = 0;
                int rc = ckpt_capture_pipe_reason(pair[0], 0x11, NULL, NULL);
                _exit(rc == 0 ? (g_ckpt_capture_destructive ? 0 : 1) : 2);
            }
            if (children[index] < 0) verdict = 13;
        }
        g_ckpt_capture_destructive = 0;
        if (verdict == 0 && ckpt_capture_pipe_reason(pair[0], 0x11, NULL, NULL) != 0) verdict = 14;
        if (verdict == 0 && !g_ckpt_capture_destructive) verdict = 15;
        for (int index = 0; index < 2; ++index) {
            if (children[index] <= 0) continue;
            int status = 0;
            if (waitpid(children[index], &status, 0) != children[index]) verdict = verdict ? verdict : 16;
            if (!WIFEXITED(status) || WEXITSTATUS(status) != 0) verdict = verdict ? verdict : 17;
        }
        struct ckpt_pipe_test_shared *shared = g_ckpt_pipe_test_shared;
        if (verdict == 0 && atomic_load(&shared->winners) != 1) verdict = 18;
        if (verdict == 0 && atomic_load(&shared->losers) != 2) verdict = 19;
        if (verdict == 0 && atomic_load(&shared->length) != CKPT_PIPE_TEST_PAYLOAD) verdict = 20;
        for (unsigned index = 0; verdict == 0 && index < CKPT_PIPE_TEST_PAYLOAD; ++index)
            if (shared->bytes[index] != ckpt_pipe_test_byte(index)) verdict = 21;
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != 0) verdict = 22;
    } else if (scenario == 1) {
        // The abort contract: the drain empties the pipe for EVERY holder, so every holder -- the winner and
        // each co-holder that returned 0 without reading a byte -- must be marked destructive. A holder left
        // unmarked resumes out of its park onto a pipe whose bytes are gone.
        if (ckpt_pipe_test_open_shared() != 0) return 30;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 31;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 32;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        // Three descriptors on ONE open file description, exactly what fork hands each holder.
        int holders[3] = {pair[0], -1, -1};
        for (int index = 1; verdict == 0 && index < 3; ++index) {
            holders[index] = dup(pair[0]);
            if (holders[index] < 0) verdict = 33;
        }
        for (int index = 0; verdict == 0 && index < 3; ++index) {
            g_ckpt_capture_destructive = 0;
            if (ckpt_capture_pipe_reason(holders[index], 0x11, NULL, NULL) != 0) verdict = 34;
            if (verdict == 0 && !g_ckpt_capture_destructive) verdict = 35 + index;
        }
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != 0) verdict = 38;
        if (verdict == 0 && atomic_load(&g_ckpt_pipe_test_shared->winners) != 1) verdict = 39;
        for (int index = 1; index < 3; ++index)
            if (holders[index] >= 0) close(holders[index]);
    } else if (scenario == 2) {
        // A write-only end is not drainable and must never take the claim: winning an election it cannot
        // satisfy would publish an empty object and strand the buffered bytes in the kernel.
        if (ckpt_pipe_test_open_shared() != 0) return 50;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 51;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 52;
        int reader_flags = fcntl(pair[0], F_GETFL);
        int writer_flags = fcntl(pair[1], F_GETFL);
        if (verdict == 0 && (reader_flags < 0 || writer_flags < 0)) verdict = 53;
        if (verdict == 0 && !ckpt_pipe_end_drains(reader_flags)) verdict = 54;
        if (verdict == 0 && ckpt_pipe_end_drains(writer_flags)) verdict = 55;
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != (int)CKPT_PIPE_TEST_PAYLOAD) verdict = 56;
        if (verdict == 0 && atomic_load(&g_ckpt_pipe_test_shared->claim) != 0) verdict = 57;
    } else if (scenario == 3) {
        // Both capture paths ask the same question. The queued-rights path used to ask "== O_RDONLY", which
        // dropped an O_RDWR end on the floor: no object published, and the pipe restored empty.
        verdict = 0;
        if (!ckpt_pipe_end_drains(O_RDONLY)) verdict = 60;
        if (verdict == 0 && !ckpt_pipe_end_drains(O_RDWR)) verdict = 61;
        if (verdict == 0 && ckpt_pipe_end_drains(O_WRONLY)) verdict = 62;
        return verdict;
    } else if (scenario == 4) {
        // The whole closed round trip, capture half against restore half. Drain a pipe holding a known
        // payload, then hand the published object to the production refill and read the bytes back out of a
        // freshly created pipe: same bytes, same order, nothing left over.
        if (ckpt_pipe_test_open_shared() != 0) return 70;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 71;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 72;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        g_ckpt_capture_destructive = 0;
        if (verdict == 0 && ckpt_capture_pipe_reason(pair[0], 0x11, NULL, NULL) != 0) verdict = 73;
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != 0) verdict = 74;
        int restored[2] = {-1, -1};
        struct ckpt_source *saved_source = ckpt_source_current();
        ckpt_source_install(&g_ckpt_pipe_test_source_ops);
        if (verdict == 0 && pipe(restored) != 0) verdict = 75;
        if (verdict == 0 && ckpt_refill_restore_pipe(restored[1], 0x11) != 0) verdict = 76;
        if (verdict == 0 && ckpt_pipe_test_buffered(restored[0]) != (int)CKPT_PIPE_TEST_PAYLOAD) verdict = 77;
        for (unsigned at = 0; verdict == 0 && at < CKPT_PIPE_TEST_PAYLOAD;) {
            unsigned char block[4096];
            ssize_t count = read(restored[0], block, sizeof block);
            if (count <= 0) {
                verdict = 78;
                break;
            }
            for (ssize_t index = 0; index < count; ++index)
                if (block[index] != ckpt_pipe_test_byte(at + (unsigned)index)) verdict = 79;
            at += (unsigned)count;
        }
        ckpt_source_install(saved_source ? saved_source->ops : NULL);
        if (restored[0] >= 0) close(restored[0]);
        if (restored[1] >= 0) close(restored[1]);
    } else if (scenario == 5) {
        // THE PRODUCT PROPERTY: a checkpoint refused for a non-destructive reason leaves the container
        // running unharmed.
        //
        // The descriptor set is walked in ASCENDING guest-fd order, so this fixture puts a drainable pipe
        // holding a known payload BELOW a descriptor the scan is guaranteed to refuse (a socket, which
        // ckpt_capture_native_fd refuses outright). Under the old single-pass ordering the pipe was drained
        // on the way to a refusal that consumed nothing, g_ckpt_capture_destructive latched, and every
        // member of the tree -- winner and losers alike -- took the terminal `cannot resume` arm.
        //
        // The assertions are the property, not the mechanism: after the refusal the pipe still holds every
        // byte the guest had buffered, and the disposition ckpt_coordinate_and_exit reads
        // (g_ckpt_capture_destructive) still says "resume". Nothing here names a pass or an ordering.
        if (pipe(pair) != 0) return 80;
        int refused_socket = socket(AF_UNIX, SOCK_STREAM, 0);
        if (refused_socket < 0) {
            close(pair[0]);
            close(pair[1]);
            return 81;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 82;
        // Ascending order is the whole point: the pipe must be admitted before the socket is reached.
        if (verdict == 0 && !(pair[0] < refused_socket)) verdict = 83;
        if (ckpt_pipe_test_open_shared() != 0) verdict = 84;
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        g_ckpt_capture_destructive = 0;
        if (verdict == 0 && ckpt_pipe_test_refused_scan(pair[0], (uint64_t)0x11, refused_socket) == 0)
            verdict = 85; // the scan must refuse; a fixture whose refusal stopped firing proves nothing
        if (verdict == 0 && ckpt_pipe_test_buffered(pair[0]) != (int)CKPT_PIPE_TEST_PAYLOAD)
            verdict = 86; // the guest's buffered bytes were consumed for a capture that was refused
        if (verdict == 0 && g_ckpt_capture_destructive != 0)
            verdict = 87; // the refusal is terminal for this member: the container does not survive it
        close(refused_socket);
    } else if (scenario == 6) {
        // A pipe drain must not mutate its shared OFD even transiently. Every holder snapshots status flags
        // independently during a tree capture; pausing the winner inside its sink write makes that window
        // deterministic and proves a co-holder can never record capture-internal O_NONBLOCK as guest state.
        if (ckpt_pipe_test_open_shared() != 0) return 90;
        if (pipe(pair) != 0) {
            ckpt_pipe_test_close_shared();
            return 91;
        }
        verdict = 0;
        if (ckpt_pipe_test_fill(pair[1]) != 0) verdict = 92;
        atomic_store(&g_ckpt_pipe_test_shared->pause_write, 1);
        ckpt_sink_install(&g_ckpt_pipe_test_ops);
        pid_t child = verdict == 0 ? hl_host_process_clone_current() : -1;
        if (child == 0) _exit(ckpt_capture_pipe_reason(pair[0], 0x11, NULL, NULL) == 0 ? 0 : 1);
        if (child < 0) verdict = 93;
        for (int waited = 0; verdict == 0 && !atomic_load(&g_ckpt_pipe_test_shared->write_entered); ++waited) {
            if (waited == 1000) {
                verdict = 94;
                break;
            }
            struct timespec span = {0, 1000000};
            nanosleep(&span, NULL);
        }
        int flags = fcntl(pair[0], F_GETFL);
        if (verdict == 0 && (flags < 0 || (flags & O_NONBLOCK) != 0)) verdict = 95;
        atomic_store(&g_ckpt_pipe_test_shared->release_write, 1);
        if (child > 0) {
            int status = 0;
            if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0)
                verdict = verdict ? verdict : 96;
        }
    } else {
        return 99;
    }

    if (pair[0] >= 0) close(pair[0]);
    if (pair[1] >= 0) close(pair[1]);
    ckpt_sink_install(saved_sink ? saved_sink->ops : NULL);
    g_ckpt_capture_destructive = saved_destructive;
    ckpt_pipe_test_close_shared();
    return verdict;
}
