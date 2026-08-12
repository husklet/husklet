/*
 * Guest userspace accessors for syscall emulation.
 *
 * Most guest VMAs have guest VA == host VA.  A logical VMA is different: its
 * Linux-visible 4 KiB interval is represented by canonical storage elsewhere
 * because the host cannot mmap its address/file offset exactly.  Host syscalls
 * and service-side memcpy must therefore never consume a raw guest pointer
 * while such an interval is involved.
 *
 * Included by dispatch.c after mem.c (for host_range_mapped) and before the
 * syscall families.  These routines deliberately walk immutable logical-VMA
 * snapshots; mapping publication/reclamation is synchronized at the engine's
 * mapping quiescence boundary.
 */
#include "../logical_vma.h"
#include "../host_poll.h"
#include "../host_tty.h"

#ifndef G_SMC_COPYOUT
#error "target must define G_SMC_COPYOUT(guest_first, guest_last)"
#endif

enum { GUEST_IOV_STACK_MAX = 1024 };

static void guest_smc_alias(uint64_t first, uint64_t last, void *opaque) {
    (void)opaque;
    G_SMC_COPYOUT(first, last);
}

static void guest_smc_copyout(const hl_logical_vma_pin *pin, uint64_t guest, size_t length) {
    if (!length) return;
    if (pin != NULL && pin->token != NULL)
        hl_logical_vma_visit_exec_aliases_pin(pin, 0, length, guest_smc_alias, NULL);
    else
        G_SMC_COPYOUT(guest, guest + length);
}

/*
 * Resolve the longest immediately usable prefix at `guest`.
 *
 * Return 1 for canonical storage, 0 for an ordinary identity-mapped span, and
 * -1 at the first inaccessible byte.  A direct span stops before the next
 * logical VMA, even if the host mappings happen to be adjacent.
 */
static int guest_span(uint64_t guest, size_t length, uint32_t protection, void **host, size_t *available,
                      hl_logical_vma_pin *pin) {
    if (!length || !host || !available || !pin || guest > UINT64_MAX - length) {
        errno = EFAULT;
        return -1;
    }
    /* These are the engine's copy_{to,from}_user, so this is exactly the "one host dereference" the non-PIE
     * coordinate rule (thread.c) says to fold at.  Doing it HERE rather than at each caller is what stops
     * the family recurring: dispatch.c's per-syscall pointer-argument allowlist has to name every syscall
     * and every argument position, and a handler it forgets (clone's child_tid, set_tid_address,
     * set_robust_list) dereferenced a low link vaddr.  Idempotent -- an already-rebased argument is above
     * the link range -- so the allowlist stays correct and this is inert for PIE/static-PIE. */
    guest = nonpie_fold(guest);
    *pin = (hl_logical_vma_pin){0};

    int logical = hl_logical_vma_pin_data(guest, length, protection, pin);
    if (logical < 0) {
        errno = EFAULT; /* includes a logical VMA with insufficient protection */
        return -1;
    }
    if (logical == 1) {
        *host = pin->host;
        *available = pin->contiguous < length ? pin->contiguous : length;
        return 1;
    }
    /* pin_data bounded this identity span before the next logical interval. */
    size_t bound = pin->contiguous < length ? pin->contiguous : length;

    /*
     * Find a precise mapped prefix a host-page at a time.  host_range_mapped
     * is fault guarded and also applies the guest PROT_NONE ledger.  Stopping
     * at page boundaries gives Linux copy_{to,from}_user's useful partial-I/O
     * behavior when the second page faults.
     */
    size_t prefix = 0;
    while (prefix < bound) {
        size_t page_left = 4096u - (size_t)((guest + prefix) & 4095u);
        size_t chunk = bound - prefix < page_left ? bound - prefix : page_left;
        int accessible = (protection & HL_LOGICAL_VMA_WRITE) ? host_range_writable((uintptr_t)(guest + prefix), chunk)
                                                             : host_range_mapped((uintptr_t)(guest + prefix), chunk);
        if (!accessible) break;
        prefix += chunk;
    }
    if (!prefix) {
        errno = EFAULT;
        return -1;
    }
    *host = (void *)(uintptr_t)guest;
    *available = prefix;
    return 0;
}

/* Copy a userspace prefix. Return bytes copied, or -EFAULT if byte zero faults. */
static ssize_t guest_copy_from(void *destination, uint64_t source, size_t length) {
    size_t done = 0;
    while (done < length) {
        void *host;
        size_t span;
        hl_logical_vma_pin pin;
        if (guest_span(source + done, length - done, HL_LOGICAL_VMA_READ, &host, &span, &pin) < 0)
            return done ? (ssize_t)done : -EFAULT;
        memcpy((char *)destination + done, host, span);
        hl_logical_vma_unpin(&pin);
        done += span;
    }
    return (ssize_t)done;
}

static ssize_t guest_copy_to(uint64_t destination, const void *source, size_t length) {
    size_t done = 0;
    destination = nonpie_fold(destination); // the SMC key below is an EXECUTION address, i.e. the storage one
    while (done < length) {
        void *host;
        size_t span;
        hl_logical_vma_pin pin;
        if (guest_span(destination + done, length - done, HL_LOGICAL_VMA_WRITE, &host, &span, &pin) < 0)
            return done ? (ssize_t)done : -EFAULT;
        memcpy(host, (const char *)source + done, span);
        guest_smc_copyout(&pin, destination + done, span);
        hl_logical_vma_unpin(&pin);
        done += span;
    }
    return (ssize_t)done;
}

/*
 * Turn one guest byte range into host iovecs, stopping at the first fault.
 * `covered` is always the accessible prefix.  Empty ranges produce zero
 * vectors.  Exhausting vector capacity is a legal short prefix, not EFAULT.
 */
static int guest_iov_range(uint64_t guest, size_t length, uint32_t protection, struct iovec *host_iov,
                           hl_logical_vma_pin *pins, uint64_t *guest_bases, size_t host_capacity, size_t *covered) {
    size_t done = 0, count = 0;
    guest = nonpie_fold(guest); // guest_bases feed guest_smc_copyout, which is keyed on execution addresses
    while (done < length && count < host_capacity) {
        void *host;
        size_t span;
        hl_logical_vma_pin pin;
        if (guest_span(guest + done, length - done, protection, &host, &span, &pin) < 0) break;
        host_iov[count++] = (struct iovec){.iov_base = host, .iov_len = span};
        pins[count - 1] = pin;
        guest_bases[count - 1] = guest + done;
        done += span;
    }
    *covered = done;
    return done || !length ? (int)count : -EFAULT;
}

static size_t guest_accessible_prefix(uint64_t guest, size_t length, uint32_t protection) {
    size_t done = 0;
    while (done < length) {
        void *host;
        size_t span;
        hl_logical_vma_pin pin;
        if (guest_span(guest + done, length - done, protection, &host, &span, &pin) < 0) break;
        done += span;
        hl_logical_vma_unpin(&pin);
    }
    return done;
}

static void guest_free_bounce(void *slot) {
    free(*(void **)slot);
}

static int guest_copy_string(char *destination, size_t capacity, uint64_t source) {
    if (!source || !capacity) return -EFAULT;
    for (size_t i = 0; i < capacity; ++i) {
        if (guest_copy_from(destination + i, source + i, 1) != 1) return -EFAULT;
        if (!destination[i]) return (int)i;
    }
    destination[capacity - 1] = 0;
    return -ENAMETOOLONG;
}

/* Resolve a fixed-size object that must remain one atomic host object. */
static int guest_atomic_address(uint64_t guest, size_t length, uint32_t protection, void **host,
                                hl_logical_vma_pin *pin) {
    size_t available;
    if (guest_span(guest, length, protection, host, &available, pin) < 0 || available < length) {
        hl_logical_vma_unpin(pin);
        return -EFAULT;
    }
    return 0;
}

/* Import the iovec descriptors themselves through canonical guest storage. */
static int guest_iov_import(uint64_t guest, size_t count, struct iovec *vectors) {
    if (count > SIZE_MAX / sizeof(*vectors)) return -EFAULT;
    size_t bytes = count * sizeof(*vectors);
    if (!bytes) return 0;
    ssize_t copied = guest_copy_from(vectors, guest, bytes);
    return copied == (ssize_t)bytes ? 0 : -EFAULT;
}

/*
 * Would Linux have rejected this descriptor before it ever looked at the user buffer?  ksys_read/ksys_write
 * resolve the fd first (fdget_pos -> EBADF, then the FMODE_READ/FMODE_WRITE test, also EBADF) and only the
 * file's ->read_iter/->write_iter reaches copy_{to,from}_user.  Our translation runs first -- that is what
 * keeps the transfer zero-copy -- so a translation failure must ask whether the descriptor, not the buffer,
 * was the real complaint.  `for_read` names the access mode the descriptor has to carry.
 */
static int guest_fd_rejects(int fd, int for_read) {
    int flags = fcntl(fd, F_GETFL);
    if (flags < 0) return 1; // no such descriptor
    int mode = flags & O_ACCMODE;
    return for_read ? mode == O_WRONLY : mode == O_RDONLY;
}

/*
 * Would this read have moved zero bytes anyway?  Linux resolves the descriptor and consults the source
 * before ->read_iter reaches copy_to_user, so a read that transfers nothing never observes a bad buffer:
 * EOF returns 0 and a non-blocking source with nothing ready returns EAGAIN.  Both must be reported
 * WITHOUT consuming data or touching the descriptor's flags, so seekable sources are probed with pread at
 * the current position (which does not advance it) and non-seekable ones with poll + FIONREAD.
 * Returns 0/-1 like read(2), with errno set on -1.
 */
static ssize_t guest_read_would_not_copy(int fd, off_t offset, int positional) {
    if (guest_fd_rejects(fd, 1)) {
        errno = EBADF;
        return -1;
    }
    off_t probe_offset = positional ? offset : lseek(fd, 0, SEEK_CUR);
    if (probe_offset >= 0) {
        unsigned char probe;
        ssize_t probed = pread(fd, &probe, 1, probe_offset);
        if (probed <= 0) return probed; // EOF -> 0; a real error (ESPIPE on pread of a pipe) -> that errno
        errno = EFAULT;
        return -1;
    }
    short want = POLLIN;
#ifdef POLLRDHUP
    want |= POLLRDHUP;
#endif
    struct pollfd ready = {.fd = fd, .events = want};
    if (poll(&ready, 1, 0) > 0) {
        short hup = POLLHUP;
#ifdef POLLRDHUP
        hup |= POLLRDHUP;
#endif
        if (ready.revents & (POLLIN | hup)) {
            // A queue that reports readable-or-hung-up with nothing pending is at EOF.  FIONREAD itself
            // fails on a pty slave whose master is gone, where POLLHUP alone settles it.
            int pending = 0;
            int counted = ioctl(fd, FIONREAD, &pending) == 0;
            if (counted ? pending == 0 : (ready.revents & POLLHUP) != 0) return 0;
        }
    } else {
        int flags = fcntl(fd, F_GETFL);
        if (flags >= 0 && (flags & O_NONBLOCK)) {
            errno = EAGAIN;
            return -1;
        }
        // A blocking source with nothing ready would sleep in ->read_iter; there is no non-destructive
        // way to reach the copy it would eventually attempt, so report the buffer complaint.
    }
    errno = EFAULT;
    return -1;
}

/*
 * read/pread into a guest range.  One host syscall consumes all canonical and
 * direct spans, retaining atomic file-position advancement and kernel short
 * read/EINTR behavior.  A fault after a valid prefix exposes only that prefix
 * to the host, exactly like Linux copy_to_user's short read.
 */
static ssize_t guest_fd_read(int fd, uint64_t guest, size_t length, off_t offset, int positional) {
    if (!length)
        return positional ? pread(fd, (void *)(uintptr_t)guest, 0, offset) : read(fd, (void *)(uintptr_t)guest, 0);
    struct iovec vectors[GUEST_IOV_STACK_MAX];
    hl_logical_vma_pin pins[GUEST_IOV_STACK_MAX] = {0};
    uint64_t guest_bases[GUEST_IOV_STACK_MAX];
    size_t covered = 0;
    int count =
        guest_iov_range(guest, length, HL_LOGICAL_VMA_WRITE, vectors, pins, guest_bases, GUEST_IOV_STACK_MAX, &covered);
    if (count < 0) return guest_read_would_not_copy(fd, offset, positional);
    ssize_t result = positional ? preadv(fd, vectors, count, offset) : readv(fd, vectors, count);
    if (result > 0) {
        size_t remaining = (size_t)result;
        for (int index = 0; index < count && remaining; ++index) {
            size_t changed = vectors[index].iov_len < remaining ? vectors[index].iov_len : remaining;
            guest_smc_copyout(&pins[index], guest_bases[index], changed);
            remaining -= changed;
        }
    }
    for (int index = 0; index < count; ++index)
        hl_logical_vma_unpin(&pins[index]);
    return result;
}

static ssize_t guest_fd_write(int fd, uint64_t guest, size_t length, off_t offset, int positional) {
    if (!length)
        return positional ? pwrite(fd, (const void *)(uintptr_t)guest, 0, offset)
                          : write(fd, (const void *)(uintptr_t)guest, 0);
    struct iovec vectors[GUEST_IOV_STACK_MAX];
    hl_logical_vma_pin pins[GUEST_IOV_STACK_MAX] = {0};
    uint64_t guest_bases[GUEST_IOV_STACK_MAX];
    size_t covered = 0;
    int count =
        guest_iov_range(guest, length, HL_LOGICAL_VMA_READ, vectors, pins, guest_bases, GUEST_IOV_STACK_MAX, &covered);
    if (count < 0) {
        // A write always dereferences the source, so there is no EOF-shaped escape here: the descriptor
        // (missing, or not open for writing) simply loses first.
        errno = guest_fd_rejects(fd, 0) ? EBADF : EFAULT;
        return -1;
    }
    ssize_t result = positional ? pwritev(fd, vectors, count, offset) : writev(fd, vectors, count);
    for (int index = 0; index < count; ++index)
        hl_logical_vma_unpin(&pins[index]);
    return result;
}

/*
 * Translate a guest vector array and each segment.  Linux imports the complete
 * descriptor array before moving data, but a fault in a later payload segment
 * permits the already-accessible byte prefix to be transferred.
 */
static ssize_t guest_fd_vector_flags(int fd, uint64_t guest_vectors, size_t guest_count, off_t offset, int positional,
                                     int output, int flags) {
    if (guest_count > GUEST_IOV_STACK_MAX) {
        errno = EINVAL;
        return -1;
    }
    struct iovec guest_iov[GUEST_IOV_STACK_MAX];
    if (guest_iov_import(guest_vectors, guest_count, guest_iov) < 0) {
        // do_readv/do_writev also reach the descriptor first: fdget_pos precedes import_iovec.
        errno = guest_fd_rejects(fd, output) ? EBADF : EFAULT;
        return -1;
    }
    if (!guest_count) {
#if defined(__linux__)
        if (positional && flags)
            return output ? preadv2(fd, NULL, 0, offset, flags) : pwritev2(fd, NULL, 0, offset, flags);
#else
        if (flags) {
            errno = EOPNOTSUPP;
            return -1;
        }
#endif
        return positional ? (output ? preadv(fd, NULL, 0, offset) : pwritev(fd, NULL, 0, offset))
                          : (output ? readv(fd, NULL, 0) : writev(fd, NULL, 0));
    }

    struct iovec host_iov[GUEST_IOV_STACK_MAX];
    hl_logical_vma_pin pins[GUEST_IOV_STACK_MAX] = {0};
    uint64_t guest_bases[GUEST_IOV_STACK_MAX];
    size_t host_count = 0;
    for (size_t index = 0; index < guest_count && host_count < GUEST_IOV_STACK_MAX; ++index) {
        uint64_t guest = (uint64_t)(uintptr_t)guest_iov[index].iov_base;
        size_t covered = 0;
        int count = guest_iov_range(
            guest, guest_iov[index].iov_len, output ? HL_LOGICAL_VMA_WRITE : HL_LOGICAL_VMA_READ, host_iov + host_count,
            pins + host_count, guest_bases + host_count, GUEST_IOV_STACK_MAX - host_count, &covered);
        if (count < 0) {
            if (!host_count) {
                // The descriptor array itself imported fine, so a read that moves nothing still never
                // reaches the payload segment: same EOF/would-block escape as plain read(2).
                if (output) return guest_read_would_not_copy(fd, offset, positional);
                errno = guest_fd_rejects(fd, output) ? EBADF : EFAULT;
                return -1;
            }
            break;
        }
        host_count += (size_t)count;
        if (covered != guest_iov[index].iov_len) break;
    }

    /*
     * Every guest segment was zero-length, so the vector collapsed to nothing.  Linux transfers no
     * bytes and returns 0 for such a call (fs/read_write.c: an iov_iter of count 0), but BSD/macOS
     * reject iovcnt 0 outright with EINVAL, so forwarding the empty host vector would invent an
     * error the guest must never see.  Hand the host one zero-length segment instead of zero
     * segments: it is accepted on both kernels, moves nothing, and still performs the descriptor's
     * access-mode check (a writev through an O_RDONLY fd must stay EBADF, which an early `return 0`
     * here would swallow).
     */
    static char empty_segment;
    if (!host_count) {
        host_iov[0] = (struct iovec){&empty_segment, 0};
        host_count = 1;
    }

    ssize_t result;
#if defined(__linux__)
    if (positional && flags)
        result = output ? preadv2(fd, host_iov, (int)host_count, offset, flags)
                        : pwritev2(fd, host_iov, (int)host_count, offset, flags);
    else
#else
    if (flags) {
        for (size_t index = 0; index < host_count; ++index)
            hl_logical_vma_unpin(&pins[index]);
        errno = EOPNOTSUPP;
        return -1;
    }
#endif
        result = positional ? (output ? preadv(fd, host_iov, (int)host_count, offset)
                                      : pwritev(fd, host_iov, (int)host_count, offset))
                            : (output ? readv(fd, host_iov, (int)host_count) : writev(fd, host_iov, (int)host_count));
    if (output && result > 0) {
        size_t remaining = (size_t)result;
        for (size_t index = 0; index < host_count && remaining; ++index) {
            size_t changed = host_iov[index].iov_len < remaining ? host_iov[index].iov_len : remaining;
            guest_smc_copyout(&pins[index], guest_bases[index], changed);
            remaining -= changed;
        }
    }
    for (size_t index = 0; index < host_count; ++index)
        hl_logical_vma_unpin(&pins[index]);
    return result;
}

static ssize_t guest_fd_vector(int fd, uint64_t guest_vectors, size_t guest_count, off_t offset, int positional,
                               int output) {
    return guest_fd_vector_flags(fd, guest_vectors, guest_count, offset, positional, output, 0);
}
