// Extracted from service(): Event loop -- epoll/eventfd2/timerfd/signalfd4/inotify, emulated on macOS
// kqueue/pipes. Returns 1 if nr was handled, 0 otherwise. Included by service.c after service/net.c,
// before service() -- same TU scope (shares io.c/signal.c fd-redirection state).

#include "../checkpoint.h"

// struct epoll_event has a DIFFERENT layout per guest arch: x86-64 forces __attribute__((packed)) so it
// is 12 bytes with `data` at offset 4; every other arch (aarch64/asm-generic) leaves it naturally aligned
// at 16 bytes (4 bytes pad after the u32 events, then `data` at offset 8). Derive both from the same
// G_O_DIRECTORY discriminator io.c uses, so epoll_ctl reads `data` and epoll_pwait writes the out-array at
// the stride/offset the guest's libc/runtime expects (Go's netpoller stores a pointer in `data`).
#if G_O_DIRECTORY == 0x10000
#define G_EPEV_SZ 12u // x86-64 (packed)
#define G_EPEV_DOFF 4u
#else
#define G_EPEV_SZ 16u // aarch64 / asm-generic
#define G_EPEV_DOFF 8u
#endif

// Edge-triggered "prime" on registration. Linux reports an fd that is ALREADY readable/writable at
// EPOLL_CTL_ADD/MOD time when it is registered EPOLLET -- the registration itself counts as the edge (this
// is how Go's netpoller learns about an accepted connection whose request bytes are already buffered). A
// macOS kqueue EV_CLEAR filter, by contrast, reports only a *subsequent* transition, so an already-ready fd
// is never delivered and a Go HTTP server accepts the connection but never responds. So when we arm an edge
// filter on a fd that currently polls ready, stash a synthetic readiness event here and deliver it on the
// next epoll_wait -- once (edge semantics). Tables are indexed by epoll fd (<HL_NFD); larger fds use the
// immediate path and simply don't get primed. Level-triggered fds need no prime (kqueue without EV_CLEAR
// already reports current readiness), so only EPOLLET arms reach here -- level semantics are untouched.
static struct kevent *g_ep_prime[HL_NFD];
static int g_ep_primen[HL_NFD], g_ep_primecap[HL_NFD];

static void ep_prime_push(int ep, uintptr_t ident, int16_t filt, void *udata) {
    if (ep < 0 || ep >= HL_NFD) return;
    struct kevent *a = g_ep_prime[ep];
    for (int i = 0; i < g_ep_primen[ep]; i++)
        if (a[i].ident == ident && a[i].filter == filt) {
            a[i].udata = udata;
            return;
        }
    if (g_ep_primen[ep] >= g_ep_primecap[ep]) {
        int nc = g_ep_primecap[ep] ? g_ep_primecap[ep] * 2 : 8;
        struct kevent *na = realloc(a, (size_t)nc * sizeof *na);
        if (!na) return;
        g_ep_prime[ep] = na;
        g_ep_primecap[ep] = nc;
        a = na;
    }
    EV_SET(&a[g_ep_primen[ep]++], ident, filt, 0, 0, 0, udata);
}

// If `fd` currently polls ready for the direction `filt` covers, record a one-shot prime on `ep`.
static void ep_prime_if_ready(int ep, int fd, int16_t filt, void *udata) {
    if (ep < 0 || ep >= HL_NFD || fd < 0) return;
    short want = (filt == EVFILT_READ) ? POLLIN : POLLOUT;
    struct pollfd pfd = {.fd = fd, .events = want, .revents = 0};
    if (poll(&pfd, 1, 0) > 0 && (pfd.revents & (want | POLLHUP | POLLERR)))
        ep_prime_push(ep, (uintptr_t)fd, filt, udata);
}

// LEVEL-triggered counterpart to the edge prime, for a fd that is ALREADY ready when its knote is
// (re)armed under a multi-threaded guest. A level knote needs no synthetic prime -- once it is on the
// kqueue, kevent() reports the current readiness naturally. The hazard is purely SUBMISSION: the W3E fast
// path DEFERS the EV_ADD into the per-instance changelist and only flushes it as one batched kevent()
// (ep_flush). Under concurrent epoll_ctl churn that batch routinely carries an EV_ADD/EV_DELETE for an fd a
// peer just closed/re-cycled; macOS kevent() stops applying the changelist at the first erroring element
// when the eventlist has no room for the EV_ERROR echo, STRANDING every change queued behind it -- including
// this ready fd's EV_ADD. The knote then never arms, the parked pump never sees the ready socket, and its
// readiness is lost (the level-triggered pump-primary-channel stall). Fix: for a ready-at-arm level fd, arm
// its knote in ITS OWN isolated kevent() here, so an unrelated churn error can't strand it. The batched copy
// in the changelist is left in place (an EV_ADD of an already-armed knote is idempotent); the subsequent
// ep_flush wake then makes a peer already blocked in kevent() return and re-scan, at which point the knote is
// armed and the socket's level readiness is delivered. Threaded only -- single-threaded the same thread
// issues the next epoll_wait and submits the changelist itself, so its fast path stays byte-unchanged.
static void ep_submit_ready_level(int ep, int fd, int16_t filt, uint16_t xf, void *udata) {
    if (ep < 0 || ep >= HL_NFD || fd < 0) return;
    short want = (filt == EVFILT_READ) ? POLLIN : POLLOUT;
    struct pollfd pfd = {.fd = fd, .events = want, .revents = 0};
    if (poll(&pfd, 1, 0) <= 0 || !(pfd.revents & (want | POLLHUP | POLLERR))) return;
    struct kevent kv;
    EV_SET(&kv, (uintptr_t)fd, filt, EV_ADD | xf, 0, 0, udata);
    kevent(ep, &kv, 1, NULL, 0, NULL); // isolated arm: a churn EV_ERROR in the batch can't strand it
}

// --- cross-thread readiness wakeup (EVFILT_USER) --------------------------------------------------
static int bound_shadow_install(int fd);
static int bound_snapshot(uint64_t value, hl_linux_fd_snapshot *snapshot);
static int bound_fdvis_publish_snapshot(int fd, const hl_linux_fd_snapshot *snapshot);
// A Go netpoller (and node's worker-thread pool) shares ONE epoll instance across several OS threads
// (Go Ms): one M blocks in epoll_wait while ANOTHER M accepts a connection and registers it on the same
// instance (epoll_ctl). That connection usually already has its request bytes buffered, so on Linux the
// EPOLLET registration edge wakes the blocked epoll_wait at once. Two things defeat that emulation here:
// (1) the W3E fast path DEFERS the kevent registration to the next epoll_wait on the SAME thread, so a
// peer M already blocked in kevent() never sees it; (2) an already-ready fd armed EV_CLEAR produces no
// kqueue edge, so its readiness is stashed in g_ep_prime and only consulted when THIS thread next waits.
// Either way the readiness is stranded on the registering thread and the connection is accepted but never
// serviced. Fix: give every epoll kqueue an EVFILT_USER "wake" knote; when a thread registers interest
// while the process is multi-threaded, flush the pending changelist to the kernel (so the fd is visible
// to a blocked peer) and NOTE_TRIGGER the knote (so the peer returns from kevent and re-scans primes).
// A single mutex serializes the W3E per-instance state (changelist/prime/armed maps) whenever guest
// threads exist; the single-threaded path is untouched (g_threaded == 0 -> no lock, no wake, no change).
#define EP_WAKE_IDENT ((uintptr_t)0x7fffffe0u) // EVFILT_USER ident, disjoint from any real fd number
static uint8_t g_ep_wake_armed[HL_NFD];        // per epoll fd: EVFILT_USER wake knote installed on its kqueue
static pthread_mutex_t g_ep_mtx = PTHREAD_MUTEX_INITIALIZER;
// per-epoll-instance registered-fd membership (lazily allocated HL_NFD-bit bitmap indexed by the
// watched fd -- the bitmap must span the SAME index range as the fd < HL_NFD guard on ep_mem_test/
// ep_mem_set and the sibling [HL_NFD] interest tables; large event loops register hundreds of fds,
// so the watched-fd number routinely exceeds 1024 and any narrower bitmap would be indexed out of
// bounds -- a heap overflow whose corrupted/garbage membership bit spuriously returns EEXIST and drops
// the real registration, stranding that fd's readiness (a load-dependent node-connect stall).
// watched fd). kqueue silently accepts an EV_ADD of an already-armed filter and an EV_DELETE of an
// absent one, but Linux epoll_ctl returns EEXIST / ENOENT respectively, so track membership to serve
// those (plus EINVAL for adding the epoll fd to itself and EPERM for a regular file / directory). Only
// engine-tracked epoll fds (< HL_NFD, g_epoll set) get this surface -- a dup'd/large epfd keeps the existing
// best-effort immediate path, so correct software's readiness path is byte-unchanged.
// A guest that shares ONE epoll instance across threads (Go's netpoller, node's worker pool) issues
// concurrent epoll_ctl from different threads, so the membership bitmap is touched cross-thread: the byte
// RMW and the lazy alloc are therefore ATOMIC. A plain `byte |= bit` / `byte &= ~bit` is a read-modify-write
// that loses a concurrent update to a DIFFERENT bit in the SAME byte (fds 8k..8k+7 share one byte) -- e.g. a
// waiter's DEL(fd X) clear racing a peer's ADD(fd Z) set would resurrect X's stale membership bit, so when
// fd X's number is later reused a fresh EPOLL_CTL_ADD wrongly returns EEXIST (Linux never does: its
// epoll_ctl is internally serialized and close() auto-removes). Atomic OR/AND on the byte + a CAS-installed
// bitmap close that race without a lock (the single-threaded path is unchanged: uncontended atomics).
static uint8_t *g_ep_member[HL_NFD];

#define EP_NATIVE_WATCH_LIMIT 16384

typedef struct ep_native_watch {
    volatile uint8_t active;
    uint8_t owned;
    int32_t epoll;
    int32_t descriptor;
    int32_t logical_descriptor;
    uint32_t events;
    uint32_t armed;
    uint64_t data;
} ep_native_watch;

static ep_native_watch g_ep_native_watches[EP_NATIVE_WATCH_LIMIT];

static ep_native_watch *ep_native_find(int epoll, int descriptor) {
    for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
        ep_native_watch *watch = &g_ep_native_watches[index];
        if (__atomic_load_n(&watch->active, __ATOMIC_ACQUIRE) == 1 && watch->epoll == epoll &&
            watch->descriptor == descriptor)
            return watch;
    }
    return NULL;
}

static int ep_native_set(int epoll, int descriptor, int op, uint32_t events, uint64_t data) {
    ep_native_watch *watch = ep_native_find(epoll, descriptor);
    if (op == 2) {
        if (watch) {
            if (watch->owned) {
                hl_host_process_fd_private_remove(watch->descriptor);
                close(watch->descriptor);
                watch->owned = 0;
            }
            __atomic_store_n(&watch->active, 0, __ATOMIC_RELEASE);
        }
        return 0;
    }
    if (!watch) {
        for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
            uint8_t empty = 0;
            if (__atomic_compare_exchange_n(&g_ep_native_watches[index].active, &empty, 2, 0, __ATOMIC_ACQ_REL,
                                            __ATOMIC_ACQUIRE)) {
                watch = &g_ep_native_watches[index];
                watch->epoll = epoll;
                watch->descriptor = descriptor;
                watch->logical_descriptor = descriptor;
                watch->owned = 0;
                break;
            }
        }
    }
    if (!watch) return -1;
    watch->events = events;
    watch->armed = ((events & 1u) ? 1u : 0u) | ((events & 4u) ? 2u : 0u);
    watch->data = data;
    __atomic_store_n(&watch->active, 1, __ATOMIC_RELEASE);
    return 0;
}

static void ep_native_retire_epoll(int epoll) {
    for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
        if (__atomic_load_n(&g_ep_native_watches[index].active, __ATOMIC_ACQUIRE) == 1 &&
            g_ep_native_watches[index].epoll == epoll) {
            if (g_ep_native_watches[index].owned) {
                hl_host_process_fd_private_remove(g_ep_native_watches[index].descriptor);
                close(g_ep_native_watches[index].descriptor);
                g_ep_native_watches[index].owned = 0;
            }
            __atomic_store_n(&g_ep_native_watches[index].active, 0, __ATOMIC_RELEASE);
        }
    }
}

static void ep_native_disarm(int epoll, int descriptor, int16_t filter) {
    ep_native_watch *watch = ep_native_find(epoll, descriptor);
    if (!watch || !(watch->events & UINT32_C(0x40000000))) return;
    if (filter == EVFILT_READ) watch->armed &= ~1u;
    if (filter == EVFILT_WRITE) watch->armed &= ~2u;
}

static int kqueue_scm_export(int fd, struct hl_cmsg_kqueue_meta *metadata) {
    if (!metadata || fd < 0 || fd >= HL_NFD) return 0;
    if (g_linux_box != NULL) {
        hl_linux_fd_snapshot snapshot;
        hl_status snapshot_status = hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &snapshot);
        if (snapshot_status == HL_STATUS_OK &&
            snapshot.kind == UINT32_C(0x696e6f74)) { // HL_LINUX_OBJECT_INOTIFY (header is included later)
            metadata->kind = 3;
            metadata->nonblock = (snapshot.status_flags & O_NONBLOCK) != 0;
            metadata->object_id = snapshot.ofd;
            metadata->descriptor_flags = snapshot.descriptor_flags;
            return 1;
        }
    }
    if (g_epoll[fd]) {
        int slot = epoll_slot(fd);
        if (slot < 0 || slot >= HL_NFD) return -1;
        if (g_ep_chgn[slot] > 0) {
            if (kevent(slot, g_ep_chg[slot], g_ep_chgn[slot], NULL, 0, NULL) < 0) return -1;
            g_ep_chgn[slot] = 0;
        }
        metadata->kind = 1;
        metadata->canonical_fd = slot;
        return 1;
    }
    if (fd < 1024 && g_inotify[fd]) {
        inotify_object_assign(fd);
        metadata->kind = 2;
        metadata->nonblock = g_inotify_nb[fd];
        metadata->object_id = g_inotify_object[fd];
        return 1;
    }
    return 0;
}

static int kqueue_scm_import(int fd, const struct hl_cmsg_kqueue_meta *metadata, int marker) {
    if (!metadata || fd < 0 || fd >= HL_NFD) return -1;
    if (metadata->kind == 1) {
        int source = metadata->source_fd;
        int slot = fd;
        if (metadata->source_pid == (int32_t)getpid() && source >= 0 && source < HL_NFD && g_epoll[source]) {
            slot = epoll_slot(source);
            if (dup2(source, fd) < 0) return -1;
            g_ep_dupd[source] = 1;
            hl_native_kqueue_duplicate(source, fd);
        } else
            return epoll_scm_image_import(fd, metadata, marker) == 0 ? 1 : -1;
        g_epoll[fd] = 1;
        g_ep_dupd[fd] = 1;
        g_ep_cslot[fd] = (uint16_t)(slot + 1);
        g_epoll_family_seen = 1;
        return 0;
    }
    if (metadata->kind == 2 && fd < 1024) {
        int source = metadata->source_fd;
        if (metadata->source_pid != (int32_t)getpid() || source < 0 || source >= 1024 || !g_inotify[source] ||
            dup2(source, fd) < 0)
            return -1;
        g_inotify[fd] = 1;
        g_inotify_nb[fd] = metadata->nonblock != 0;
        g_inotify_object[fd] = metadata->object_id;
        g_epoll_family_seen = 1;
        return 0;
    }
    if (metadata->kind == 3) {
        int source = metadata->source_fd;
        if (g_linux_box != NULL && metadata->source_pid == (int32_t)getpid() && source >= 0 && source < HL_NFD) {
            hl_linux_fd_snapshot source_snapshot;
            if (bound_snapshot((uint64_t)(uint32_t)source, &source_snapshot)) {
                if (bound_shadow_install(fd) != fd) return -1;
                int64_t duplicated = hl_linux_dup3(g_linux_box, (hl_linux_fd)source, (hl_linux_fd)fd,
                                                   metadata->descriptor_flags ? HL_LINUX_O_CLOEXEC : 0);
                hl_linux_fd_snapshot snapshot;
                if (duplicated < 0 || !bound_snapshot((uint64_t)(uint32_t)fd, &snapshot) ||
                    bound_fdvis_publish_snapshot(fd, &snapshot) != 0) {
                    close(fd);
                    return -1;
                }
                return 0;
            }
        }
        return typed_inotify_scm_image_import(fd, metadata, marker);
    }
    return -1;
}

int epoll_scm_image_export(struct hl_cmsg_kqueue_meta *metadata, int marker) {
    if (metadata == NULL || metadata->kind != 1) return 0;
    int slot = metadata->canonical_fd;
    uint32_t count = 0;
    for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index)
        if (__atomic_load_n(&g_ep_native_watches[index].active, __ATOMIC_ACQUIRE) == 1 &&
            g_ep_native_watches[index].epoll == slot)
            count++;
    if (count > EP_NATIVE_WATCH_LIMIT) return -1;
    size_t size = sizeof count + (size_t)count * sizeof(struct hl_cmsg_epoll_watch);
    unsigned char *image = malloc(size);
    if (image == NULL) return -1;
    memcpy(image, &count, sizeof count);
    uint32_t written = 0;
    struct hl_cmsg_epoll_watch *saved = (void *)(image + sizeof count);
    for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
        ep_native_watch *watch = &g_ep_native_watches[index];
        if (__atomic_load_n(&watch->active, __ATOMIC_ACQUIRE) != 1 || watch->epoll != slot) continue;
        saved[written++] =
            (struct hl_cmsg_epoll_watch){watch->logical_descriptor, watch->events, watch->armed, 0, watch->data};
    }
    int result = pwrite(marker, image, size, (off_t)sizeof *metadata) == (ssize_t)size ? 0 : -1;
    free(image);
    if (result == 0) metadata->image_size = size;
    return result;
}

static int epoll_scm_hidden_export(struct hl_cmsg_kqueue_meta *metadata, int *fds, int capacity) {
    if (metadata == NULL || metadata->kind != 1) return 0;
    int count = 0;
    for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
        ep_native_watch *watch = &g_ep_native_watches[index];
        if (__atomic_load_n(&watch->active, __ATOMIC_ACQUIRE) != 1 || watch->epoll != metadata->canonical_fd) continue;
        if (fds != NULL) {
            if (count >= capacity || fcntl(watch->descriptor, F_GETFD) < 0) return -1;
            fds[count] = watch->descriptor;
        }
        count++;
    }
    metadata->hidden_count = (uint32_t)count;
    return count;
}

static int epoll_scm_image_remap(const struct hl_cmsg_kqueue_meta *metadata, int marker, const int *fds) {
    if (metadata == NULL || metadata->kind != 1 || metadata->hidden_count == 0) return 0;
    if (metadata->image_size != sizeof(uint32_t) + (size_t)metadata->hidden_count * sizeof(struct hl_cmsg_epoll_watch))
        return -1;
    for (uint32_t index = 0; index < metadata->hidden_count; ++index) {
        off_t offset = (off_t)sizeof *metadata + (off_t)sizeof(uint32_t) +
                       (off_t)index * (off_t)sizeof(struct hl_cmsg_epoll_watch);
        struct hl_cmsg_epoll_watch watch;
        if (pread(marker, &watch, sizeof watch, offset) != (ssize_t)sizeof watch) return -1;
        watch.reserved = (uint32_t)watch.descriptor;
        watch.descriptor = fds[index];
        if (pwrite(marker, &watch, sizeof watch, offset) != (ssize_t)sizeof watch) return -1;
    }
    return 0;
}

int epoll_scm_image_import(int fd, const struct hl_cmsg_kqueue_meta *metadata, int marker) {
    if (metadata == NULL || metadata->kind != 1 || metadata->image_size < sizeof(uint32_t) ||
        metadata->image_size > 64u * 1024u * 1024u || metadata->image_size > SIZE_MAX)
        return -1;
    size_t size = (size_t)metadata->image_size;
    unsigned char *image = malloc(size);
    if (image == NULL || pread(marker, image, size, (off_t)sizeof *metadata) != (ssize_t)size) {
        free(image);
        return -1;
    }
    uint32_t count;
    memcpy(&count, image, sizeof count);
    if (count > EP_NATIVE_WATCH_LIMIT || sizeof count + (size_t)count * sizeof(struct hl_cmsg_epoll_watch) != size) {
        free(image);
        return -1;
    }
    int instance = kqueue();
    if (instance < 0 || (instance != fd && dup2(instance, fd) < 0)) {
        if (instance >= 0) close(instance);
        free(image);
        return -1;
    }
    // Tell the kqueue layer the queue now also answers to `fd`. On macOS a dup'd kqueue descriptor IS the
    // same queue and this is a no-op; on a Linux host the queue lives in a side table keyed by descriptor,
    // so without this the alias stays registered only for `instance` -- which is closed a line later. Every
    // subsequent kevent(fd, ...) then failed EBADF on the EPOLL INSTANCE itself (not the watched fd), which
    // is what broke importing an epoll set over SCM_RIGHTS (compat case scm-epoll).
    if (instance != fd) hl_native_kqueue_duplicate(instance, fd);
    if (instance != fd) close(instance);
    g_epoll[fd] = 1;
    g_ep_dupd[fd] = 1;
    g_ep_cslot[fd] = (uint16_t)(fd + 1);
    g_epoll_family_seen = 1;
    const struct hl_cmsg_epoll_watch *saved = (const void *)(image + sizeof count);
    for (uint32_t index = 0; index < count; ++index) {
        if (saved[index].descriptor < 0 || (metadata->hidden_count == 0 && saved[index].descriptor >= HL_NFD) ||
            fcntl(saved[index].descriptor, F_GETFD) < 0) {
            fprintf(stderr, "[scm-epoll] hidden fd validation failed index=%u fd=%d errno=%d\n", index,
                    saved[index].descriptor, errno);
            goto fail;
        }
        struct kevent changes[2];
        int changes_count = 0;
        uint16_t flags = (uint16_t)((saved[index].events & UINT32_C(0x80000000) ? EV_CLEAR : 0) |
                                    (saved[index].events & UINT32_C(0x40000000) ? EV_ONESHOT : 0));
        if (saved[index].armed & 1u)
            EV_SET(&changes[changes_count++], saved[index].descriptor, EVFILT_READ, EV_ADD | flags, 0, 0,
                   (void *)(uintptr_t)saved[index].data);
        if (saved[index].armed & 2u)
            EV_SET(&changes[changes_count++], saved[index].descriptor, EVFILT_WRITE, EV_ADD | flags, 0, 0,
                   (void *)(uintptr_t)saved[index].data);
        if (changes_count && kevent(fd, changes, changes_count, NULL, 0, NULL) < 0) {
            fprintf(stderr, "[scm-epoll] kevent add failed index=%u ep=%d watched=%d errno=%d\n", index, fd,
                    saved[index].descriptor, errno);
            goto fail;
        }
        if (saved[index].descriptor < HL_NFD) ep_mem_set(fd, saved[index].descriptor, 1);
        if (ep_native_set(fd, saved[index].descriptor, 3, saved[index].events, saved[index].data) != 0) {
            fprintf(stderr, "[scm-epoll] sparse watch install failed index=%u ep=%d watched=%d\n", index, fd,
                    saved[index].descriptor);
            goto fail;
        }
        ep_native_watch *native = ep_native_find(fd, saved[index].descriptor);
        if (native) {
            native->armed = saved[index].armed;
            native->owned = metadata->hidden_count != 0;
            native->logical_descriptor =
                metadata->hidden_count != 0 ? (int32_t)saved[index].reserved : saved[index].descriptor;
        }
        if (saved[index].descriptor < HL_NFD) {
            g_ep_owner[saved[index].descriptor] = fd + 1;
            g_ep_events[saved[index].descriptor] = saved[index].events;
            g_ep_udata[saved[index].descriptor] = saved[index].data;
            g_ep_rd[saved[index].descriptor] = (saved[index].armed & 1u) != 0;
            g_ep_wr[saved[index].descriptor] = (saved[index].armed & 2u) != 0;
            g_ep_os[saved[index].descriptor] = (saved[index].events & UINT32_C(0x40000000)) != 0;
        }
    }
    free(image);
    return 0;
fail:
    ep_native_retire_epoll(fd);
    ep_mem_clear(fd);
    g_epoll[fd] = 0;
    g_ep_dupd[fd] = 0;
    g_ep_cslot[fd] = 0;
    free(image);
    return -1;
}

static int ep_mem_test(int ep, int fd) {
    if (ep < 0 || ep >= HL_NFD || fd < 0 || fd >= HL_NFD) return 0;
    uint8_t *m = __atomic_load_n(&g_ep_member[ep], __ATOMIC_ACQUIRE);
    if (!m) return 0;
    return (__atomic_load_n(&m[fd >> 3], __ATOMIC_SEQ_CST) >> (fd & 7)) & 1;
}

static void ep_mem_set(int ep, int fd, int on) {
    if (ep < 0 || ep >= HL_NFD || fd < 0 || fd >= HL_NFD) return;
    uint8_t *m = __atomic_load_n(&g_ep_member[ep], __ATOMIC_ACQUIRE);
    if (!m) {
        if (!on) return;
        uint8_t *nm = calloc(HL_NFD / 8, 1);
        if (!nm) return;
        uint8_t *expect = NULL;
        // publish atomically; if a peer installed one first, adopt theirs and free ours (the bit RMW below
        // then lands on the single winning array, so no membership bit is stranded on a discarded buffer).
        if (__atomic_compare_exchange_n(&g_ep_member[ep], &expect, nm, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE))
            m = nm;
        else {
            free(nm);
            m = expect;
        }
    }
    uint8_t bit = (uint8_t)(1u << (fd & 7));
    if (on)
        __atomic_fetch_or(&m[fd >> 3], bit, __ATOMIC_SEQ_CST);
    else
        __atomic_fetch_and(&m[fd >> 3], (uint8_t)~bit, __ATOMIC_SEQ_CST);
}

static void ep_mem_close(int ep, int fd) {
    ep_mem_set(ep, fd, 0);
}

static void ep_mem_clear(int ep) {
    if (ep < 0 || ep >= HL_NFD) return;
    if (g_ep_member[ep]) {
        free(g_ep_member[ep]);
        g_ep_member[ep] = NULL;
    }
}

static void ep_rearm_native_watch(const ep_native_watch *watch) {
    uint16_t flags = (uint16_t)((watch->events & UINT32_C(0x80000000) ? EV_CLEAR : 0) |
                                (watch->events & UINT32_C(0x40000000) ? EV_ONESHOT : 0));
    struct kevent changes[2];
    int count = 0;
    if (watch->armed & 1u)
        EV_SET(&changes[count++], watch->descriptor, EVFILT_READ, EV_ADD | flags, 0, 0, (void *)(uintptr_t)watch->data);
    if (watch->armed & 2u)
        EV_SET(&changes[count++], watch->descriptor, EVFILT_WRITE, EV_ADD | flags, 0, 0,
               (void *)(uintptr_t)watch->data);
    if (count) (void)kevent(watch->epoll, changes, count, NULL, 0, NULL);
}

// A watched fd is being closed. If a dup keeps its OPEN FILE DESCRIPTION alive, Linux keeps the epoll
// registration (readiness must persist), but the macOS kqueue knote dies with the fd NUMBER. Re-home the
// registration onto a surviving alias of the same OFD so readiness continues to be reported with the same
// udata. Called from fd_reset_emul BEFORE the interest table + ofd id are cleared, and before the real
// close(). No-op unless the closing fd is both watched (g_ep_owner) and has a dup alias (g_ofd_id).
static void ep_close_rehome(int fd) {
    if (fd < 0 || fd >= HL_NFD || !g_ofd_id[fd]) return;
    int y = ofd_surviving_alias(fd);
    for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
        ep_native_watch *watch = &g_ep_native_watches[index];
        if (__atomic_load_n(&watch->active, __ATOMIC_ACQUIRE) != 1 || watch->descriptor != fd) continue;
        int owner = watch->epoll;
        if (y < 0 || y >= HL_NFD || y == fd || ep_native_find(owner, y)) {
            ep_mem_set(owner, fd, 0);
            __atomic_store_n(&watch->active, 0, __ATOMIC_RELEASE);
            continue;
        }
#if defined(__linux__)
        if (owner >= 0 && owner < HL_NFD && g_ep_chgn[owner] > 0) {
            (void)kevent(owner, g_ep_chg[owner], g_ep_chgn[owner], NULL, 0, NULL);
            g_ep_chgn[owner] = 0;
        }
        (void)hl_native_kevent_rehome(owner, fd, y);
#else
        int descriptor = watch->descriptor;
        watch->descriptor = y;
        ep_rearm_native_watch(watch);
        watch->descriptor = descriptor;
#endif
        watch->descriptor = y;
        watch->logical_descriptor = y;
        ep_mem_set(owner, y, 1);
        ep_mem_set(owner, fd, 0);
    }
    if (!g_ep_owner[fd] || y < 0 || y >= HL_NFD || y == fd) return;
    int ep = g_ep_owner[fd] - 1;
    if (ep < 0 || ep >= HL_NFD || !g_epoll[ep] || fcntl(ep, F_GETFD) == -1) return; // epoll instance gone
    if (g_ep_owner[y]) return; // the alias is already a watched fd of its own -> don't clobber
    /* Native epoll keys the registration by the watched open-file description and retains it until its
       final alias closes.  Re-adding y would create or collide with a second registration; only the guest
       bookkeeping needs to follow the surviving descriptor. */
    g_ep_owner[y] = ep + 1;
    g_ep_events[y] = g_ep_events[fd];
    g_ep_udata[y] = g_ep_udata[fd];
    g_ep_rd[y] = g_ep_rd[fd];
    g_ep_wr[y] = g_ep_wr[fd];
    g_ep_os[y] = g_ep_os[fd];
    ep_mem_set(ep, y, 1);
    ep_mem_set(ep, fd, 0);
}

// Capture g_threaded into the returned token so lock/unlock stay balanced even if a peer thread flips
// g_threaded (0->1 on its first clone) between the two calls. Single-threaded (token 0) takes no lock.
static inline int ep_lock(void) {
    int lk = g_threaded;
    if (lk) pthread_mutex_lock(&g_ep_mtx);
    return lk;
}

static inline void ep_unlock(int lk) {
    if (lk) pthread_mutex_unlock(&g_ep_mtx);
}

// Install the one-shot self-wake knote on `ep`'s kqueue (idempotent). EV_CLEAR: a NOTE_TRIGGER is
// auto-consumed on delivery, so a trigger raised while no peer is blocked simply makes that peer's next
// kevent() return immediately -- it re-scans primes and re-blocks, so no wakeup is ever lost.
static void ep_wake_arm(int ep) {
    if (ep < 0 || ep >= HL_NFD || g_ep_wake_armed[ep]) return;
    struct kevent kv;
    EV_SET(&kv, EP_WAKE_IDENT, EVFILT_USER, EV_ADD | EV_CLEAR, 0, 0, NULL);
    if (kevent(ep, &kv, 1, NULL, 0, NULL) == 0) g_ep_wake_armed[ep] = 1;
}

// Push the deferred changelist to the kernel now (so an fd registered/removed on this thread becomes
// visible to a peer M already blocked in kevent) and, when `wake` is set (interest was added/modified),
// NOTE_TRIGGER the wake knote so that blocked peer returns and re-scans primes for an already-ready fd.
// Caller holds g_ep_mtx. Only used when g_threaded, so the W3E batching still applies single-threaded.
static void ep_flush(int ep, int wake) {
    if (ep < 0 || ep >= HL_NFD) return;
    if (g_ep_chgn[ep] > 0) {
        kevent(ep, g_ep_chg[ep], g_ep_chgn[ep], NULL, 0, NULL); // registrations only; ignore EV_ERROR echoes
        g_ep_chgn[ep] = 0;
    }
    if (!wake) return;
    ep_wake_arm(ep);
    struct kevent trig;
    EV_SET(&trig, EP_WAKE_IDENT, EVFILT_USER, 0, NOTE_TRIGGER, 0, NULL);
    kevent(ep, &trig, 1, NULL, 0, NULL);
}

// Submit an epoll instance's deferred W3E changelist to the kernel now, unconditionally. The W3E fast path
// (case 21) batches an instance's knote registrations and only submits them at the NEXT epoll_wait on that
// SAME instance. A NESTED inner epoll defeats that: a guest that registers an inner epoll fd into an outer
// one never epoll_waits the inner directly, so the inner's member knotes stay stranded in its changelist and
// the inner kqueue never becomes readable -- the outer wait sees no nested readiness. Flushing the inner's
// changelist here arms its member knotes so its kqueue reports readiness up to the outer. Caller holds
// g_ep_mtx when threaded.
static void ep_submit_changes(int ep) {
    if (ep < 0 || ep >= HL_NFD) return;
    if (g_ep_chgn[ep] > 0) {
        kevent(ep, g_ep_chg[ep], g_ep_chgn[ep], NULL, 0, NULL);
        g_ep_chgn[ep] = 0;
    }
}

// macOS does NOT inherit kqueue() descriptors across fork(2) (unlike Linux epoll/timer/inotify fds, which
// are), so every epoll/timerfd/inotify fd the engine emulates with a kqueue is DEAD in a freshly forked
// child. A guest that then closes or re-arms it sees EBADF -- e.g. Ruby's post-fork timer-thread reset
// close()s its inherited epoll fd, hits EBADF, reports "[ASYNC BUG] close event_fd" and aborts the child.
// Rebuild a fresh kqueue at each such fd NUMBER so the descriptor is valid again; the (empty) instance
// matches the re-init every runtime does post-fork, and the guest re-registers its own interest. Only fds
// that are actually dead are rebuilt -- a stale marker on an fd the parent closed and reused for a live
// (inherited) file leaves that file untouched. Called from the fork child in proc.c, before the guest runs.
static void kqueue_rebuild_after_fork(void) {
    // Nothing to rebuild, re-arm, or clear unless this lineage ever created an epoll/timerfd/inotify
    // instance (every watched/armed fd lives behind one). Skip the O(HL_NFD) scans + full-array memsets in
    // that common case; the fork-unsafe mutex re-init at the bottom still runs unconditionally.
    if (!g_epoll_family_seen) goto reinit_ep_mtx;
    for (int fd = 0; fd < HL_NFD; fd++) {
        if (!(g_epoll[fd] || g_timerfd[fd] || g_inotify[fd])) continue;
        if (fcntl(fd, F_GETFD) != -1 || errno != EBADF) continue; // still a live inherited fd -> leave it
        int kq = kqueue();
        if (kq < 0) continue;
        if (kq != fd) {
            dup2(kq, fd);
            hl_native_kqueue_duplicate(kq, fd); // register the alias BEFORE the source goes away (see above)
            close(kq);
        }
        // timerfd: Linux children INHERIT the armed timer. The deadline/interval survive the fork (COW BSS),
        // so re-arm the EVFILT_TIMER on the fresh kqueue from them (converting the absolute monotonic
        // deadline back to a relative first delay), rather than leaving the child's timer disarmed.
        if (g_timerfd[fd] && g_tfd_deadline[fd] > 0) {
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
            int64_t iv = g_tfd_interval[fd];
            int64_t delay = g_tfd_deadline[fd] - now_ns;
            if (delay < 0) delay = (iv > 0) ? (iv - ((-delay) % iv)) : 0;
            struct kevent kv;
            // A periodic timer still pending its distinct first tick (one-shot-first) inherits that pending
            // one-shot: re-arm one-shot at the remaining first delay; the child's read() then re-arms periodic.
            int one = (iv <= 0) || g_tfd_first_oneshot[fd];
            uint16_t flg = EV_ADD | (one ? EV_ONESHOT : 0);
            int64_t arm = one ? delay : iv;
            if (arm < 0) arm = 0;
            EV_SET(&kv, 1, EVFILT_TIMER, flg, NOTE_NSECONDS | NOTE_CRITICAL, arm, NULL);
            kevent(fd, &kv, 1, NULL, 0, NULL);
        }
        // inotify: Linux children inherit the instance AND its watches. The watch fds (O_EVTONLY opens) are
        // ordinary fds that survive the fork, so re-register each one's EVFILT_VNODE on the rebuilt kqueue and
        // re-apply O_NONBLOCK (the fresh kqueue is blocking by default -> an inherited nonblock read could hang).
        if (g_inotify[fd]) {
            if (g_inotify_nb[fd]) fcntl(fd, F_SETFL, O_NONBLOCK);
            for (int w = 0; w < 1024; w++) {
                if (g_inotify_owner[w] != fd) continue;
                if (fcntl(w, F_GETFD) == -1) continue; // the watch fd itself must still be open
                struct kevent wkv;
                EV_SET(&wkv, w, EVFILT_VNODE, EV_ADD | EV_CLEAR,
                       NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND, 0, (void *)(intptr_t)w);
                kevent(fd, &wkv, 1, NULL, 0, NULL);
            }
        }
        // the fresh instance carries no registrations: drop this epoll fd's inherited (now-invalid) changelist
        // and prime buffer so a later epoll_ctl/epoll_wait re-arms against the new kqueue, not stale state.
        if (g_ep_chg[fd]) {
            free(g_ep_chg[fd]);
            g_ep_chg[fd] = NULL;
        }
        g_ep_chgn[fd] = g_ep_chgcap[fd] = 0;
        if (g_ep_prime[fd]) {
            free(g_ep_prime[fd]);
            g_ep_prime[fd] = NULL;
        }
        g_ep_primen[fd] = g_ep_primecap[fd] = 0;
        ep_mem_clear(fd); // the rebuilt kqueue carries no registrations -> drop stale membership too
    }
    // every kqueue was rebuilt empty -> no watched fd is armed on any instance anymore (the armed map is
    // per-watched-fd and shared across epoll instances, so clear it wholesale to match the fresh kqueues).
    memset(g_ep_rd, 0, sizeof g_ep_rd);
    memset(g_ep_wr, 0, sizeof g_ep_wr);
    memset(g_ep_os, 0, sizeof g_ep_os);
    // the rebuilt kqueues carry no EVFILT_USER wake knote either -> re-arm lazily on next epoll op
    memset(g_ep_wake_armed, 0, sizeof g_ep_wake_armed);
    // Linux children inherit every registration in every epoll instance. A descriptor can be watched by
    // more than one instance with different events and user data, so the old descriptor-indexed
    // g_ep_owner table was insufficient here: the last EPOLL_CTL_ADD silently replaced every earlier owner
    // during the child rebuild. The pair-indexed native watch table is the authoritative interest list.
    for (uint32_t index = 0; index < EP_NATIVE_WATCH_LIMIT; ++index) {
        ep_native_watch *watch = &g_ep_native_watches[index];
        if (__atomic_load_n(&watch->active, __ATOMIC_ACQUIRE) != 1) continue;
        int ep = watch->epoll;
        int fd = watch->descriptor;
        int drop = ep < 0 || ep >= HL_NFD || !g_epoll[ep] || fcntl(ep, F_GETFD) == -1 || fcntl(fd, F_GETFD) == -1;
        if (drop) {
            __atomic_store_n(&watch->active, 0, __ATOMIC_RELEASE);
            continue;
        }
        ep_rearm_native_watch(watch);
        if (fd < HL_NFD) {
            g_ep_rd[fd] |= (watch->armed & 1u) != 0;
            g_ep_wr[fd] |= (watch->armed & 2u) != 0;
            g_ep_os[fd] |= (watch->events & UINT32_C(0x40000000)) != 0;
            ep_mem_set(ep, fd, 1);
        }
    }
    // fork() only clones the calling thread: if a peer M held g_ep_mtx (mid epoll_ctl/epoll_wait) at fork
    // time the child inherits it LOCKED with no owner, so its next svc_event ep_lock() deadlocks forever
    // (the go-build compile child hit exactly this after the g_jit_lock fix). The child is single-threaded
    // now, so reinitialising it to unlocked is always correct. (Same fork-unsafe-mutex class as g_jit_lock.)
reinit_ep_mtx:
    pthread_mutex_init(&g_ep_mtx, NULL);
}

// pselect6/ppoll/epoll_pwait install a temporary signal mask for the duration of the wait (Linux swaps the
// blocked mask atomically so a caller can unblock a signal exactly while it waits). hl's wait loops gate on
// c->sigmask via svc_poll_retry, so installing the guest mask into c->sigmask for the wait makes an
// unblocked signal interrupt the host poll/select/kevent -- previously the mask was ignored and a
// signal-driven wait slept the full timeout. `smptr` is the guest sigset_t address (bit signo-1), or 0 for
// "no temporary mask". Returns 1 if a mask was installed (previous mask stored in *saved).
static int poll_sigmask_enter(struct cpu *c, int have_mask, uint64_t nm, uint64_t *saved) {
    if (!have_mask) return 0;
    nm &= ~((1ull << (9 - 1)) | (1ull << (19 - 1))); // SIGKILL/SIGSTOP can never be blocked
    *saved = c->sigmask;
    c->sigmask = nm;
    return 1;
}

// Restore the pre-wait mask. A signal that became deliverable under the temporary mask but is blocked under
// the restored mask must still run its handler (Linux delivers it during the wait, then restores the mask on
// return) -- force exactly those bits via g_force_deliver, mirroring rt_sigsuspend (signal.c case 133).
static void poll_sigmask_leave(struct cpu *c, uint64_t saved) {
    uint64_t temp = c->sigmask;
    uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    for (int s = 1; s <= 64; s++) {
        uint64_t bit = 1ull << s;
        if (!(p & bit)) continue;
        if (temp & (1ull << (s - 1))) continue; // was blocked during the wait -> not delivered
        if ((saved & (1ull << (s - 1))) && g_sigact[s].handler > 1)
            g_force_deliver |= bit; // blocked again on restore, but Linux already delivered it -> force it
    }
    c->sigmask = saved;
}

// An eventfd is emulated by a pipe whose READ end is the guest's descriptor, so a host poll on it never
// reports POLLOUT (a pipe read end is not writable). Linux, however, reports an eventfd writable whenever its
// counter can still accept a value -- i.e. count < ULLONG_MAX-1 (0xfffffffffffffffe). Synthesize that
// write-side readiness after the host poll/select so a guest waiting for an eventfd to become writable is not
// stranded. POLLIN is already carried by the backing pipe's readable byte, so it is left untouched. Returns
// the (possibly incremented) ready-fd count.
static int eventfd_poll_writable_fixup(struct pollfd *fds, nfds_t n, int r) {
    if (!fds || r < 0 || !g_eventfd_count) return r;
    for (nfds_t i = 0; i < n; i++) {
        int fd = fds[i].fd;
        if (fd < 0 || fd >= HL_NFD || !g_eventfd_peer[fd]) continue;
        if (!(fds[i].events & POLLOUT) || (fds[i].revents & POLLOUT)) continue;
        if (g_eventfd_count[eventfd_counter_slot(fd)] < 0xfffffffffffffffeULL) {
            if (fds[i].revents == 0) r++; // a previously-idle fd now reports readiness
            fds[i].revents |= POLLOUT;
        }
    }
    return r;
}

// A private-loopback non-blocking connect has no host TCP stack behind it.  When its AF_UNIX rendezvous
// rejects the dial synchronously, connect() still reports Linux EINPROGRESS and g_so_error carries the
// deferred refusal.  macOS does not subsequently make that rejected AF_UNIX fd pollable, so publish the
// completion ourselves: Linux poll reports POLLOUT|POLLERR and SO_ERROR returns ECONNREFUSED.
static int socket_poll_error_fixup(struct pollfd *fds, nfds_t n, int r) {
    if (!fds || r < 0) return r;
    for (nfds_t index = 0; index < n; ++index) {
        int fd = fds[index].fd;
        if (fd < 0 || fd >= HL_NFD || !g_so_error[fd]) continue;
        if (fds[index].revents == 0) ++r;
        fds[index].revents |= POLLERR;
        if (fds[index].events & POLLOUT) fds[index].revents |= POLLOUT;
    }
    return r;
}

// Shared epoll_wait core for epoll_pwait (case 22, int-ms timeout) and epoll_pwait2 (case 441,
// struct timespec ns timeout). `ep` is the epoll fd, `out` the guest event out-array, `maxev` the
// already-validated (>0, capped 256) maxevents, `timeout_ns` the wait budget in NANOSECONDS with the
// Linux convention <0 = infinite (must NEVER return a spurious 0), 0 = non-blocking poll, >0 = finite,
// and `sm_set` the guest sigset_t address (0 = no temporary mask). Sets G_RET(c) to the ready count
// (>=0) or a negative errno. Extracted verbatim from case 22 so both entry points share one contract;
// the only generalization is the ms->ns timeout so epoll_pwait2 honors sub-ms timespecs exactly.
static void svc_epoll_wait_common(struct cpu *c, int ep, uint64_t guest_out, int maxev, int64_t timeout_ns,
                                  int have_mask, uint64_t sm_set) {
    struct kevent kv[256];
    uint8_t out[256 * G_EPEV_SZ];
    uint64_t sm_saved = 0;
    // A dup'd instance opts out of the deferred-changelist machinery (its interest was submitted straight
    // to the shared kqueue), so it just blocks on the kqueue like the immediate path.
    int opt = epopt_on() && ep >= 0 && ep < HL_NFD && !g_ep_dupd[ep];
    // regression fix: a cross-thread epoll_ctl fires the internal EVFILT_USER wake knote, which
    // returns us from kevent() with ONLY that nudge and no guest event -> oi==0. On real Linux epoll_wait
    // with an infinite timeout NEVER returns 0 (libuv asserts timeout!=-1 on a 0-return and node aborts),
    // and a finite wait must re-block for the REMAINING budget, not the full timeout again. So we loop:
    // capture a monotonic deadline at entry and, whenever we produced no guest event but the guest still
    // wants to block, re-enter kevent for the time that remains. Each re-block genuinely sleeps in kevent
    // (the EVFILT_USER knote is EV_CLEAR, already consumed) -- no busy spin.
    struct timespec deadline = {0, 0};
    if (timeout_ns > 0) {
        hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &deadline);
        deadline.tv_sec += (time_t)(timeout_ns / 1000000000LL);
        deadline.tv_nsec += (long)(timeout_ns % 1000000000LL);
        if (deadline.tv_nsec >= 1000000000L) {
            deadline.tv_sec++;
            deadline.tv_nsec -= 1000000000L;
        }
    }
    int oi = 0;
    int sm_on = poll_sigmask_enter(c, have_mask, sm_set, &sm_saved);
    for (;;) {
        struct timespec ts, *tp = NULL;
        if (timeout_ns == 0) {
            ts.tv_sec = 0;
            ts.tv_nsec = 0;
            tp = &ts; // non-blocking poll
        } else if (timeout_ns > 0) {
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t rem = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
            if (rem < 0) rem = 0;
            ts.tv_sec = (time_t)(rem / 1000000000LL);
            ts.tv_nsec = (long)(rem % 1000000000LL);
            tp = &ts;
        } // timeout_ns < 0 -> tp stays NULL (block forever)
        // Multi-threaded guest: serialize against peer Ms doing epoll_ctl on this instance. Arm the wake
        // knote and push any deferred changelist to the kernel BEFORE we block, so a peer's registration is
        // kernel-visible to us and its NOTE_TRIGGER can wake us. We then block on a pure wait (no changelist)
        // with the lock released, so epoll_ctl on another M is never blocked behind our sleep. Single-threaded
        // (lk == 0) keeps the classic one-syscall ctl+wait batching, unchanged.
        int lk = opt ? ep_lock() : 0;
        if (lk) {
            ep_wake_arm(ep);
            ep_flush(ep, 0);
        }
        // A pending edge-prime means some fd is ready *now*; don't sleep waiting for a fresh kqueue edge
        // (a Go server's epoll_wait blocks with an infinite timeout) -- poll kqueue and merge the prime in.
        if (opt && g_ep_primen[ep] > 0) {
            ts.tv_sec = 0;
            ts.tv_nsec = 0;
            tp = &ts;
        }
        // Object-backed watches (inotify) have no host descriptor on this kqueue, so a blocking wait would
        // never surface their readiness. Like poll()/select() over the same objects, cap the sleep to a
        // bounded tick and re-sample readiness below; a non-blocking (timeout_ns==0) wait keeps its zero timeout.
        if (timeout_ns != 0 && ep >= 0 && ep < HL_NFD && g_ep_object_count[ep] > 0 &&
            (tp == NULL || ts.tv_sec > 0 || ts.tv_nsec > 1000000L)) {
            ts.tv_sec = 0;
            ts.tv_nsec = 1000000L; // 1ms, matching the poll/select object cadence
            tp = &ts;
        }
        // W3E: submit the deferred changelist together with the wait in ONE kevent() syscall (single-threaded);
        // threaded already flushed it above and waits with no changelist.
        struct kevent *chg = (opt && !lk) ? g_ep_chg[ep] : NULL;
        int nchg = (opt && !lk) ? g_ep_chgn[ep] : 0;
        if (lk) ep_unlock(lk);
        int r;
        // epoll_wait is never restarted by a handler -- re-wait only on a SPURIOUS EINTR (nothing to
        // deliver); the moment a guest handler is runnable we return -EINTR and let the dispatcher run it.
        ts_wait_enter(); // 'S' while blocked in epoll_wait/epoll_pwait
        do {
            r = kevent(ep, chg, nchg, kv, maxev, tp);
            chg = NULL;
            nchg = 0;
        } while (r < 0 && svc_poll_retry(c));
        ts_wait_leave();
        if (opt && !lk) g_ep_chgn[ep] = 0; // consumed (threaded flushed it under the lock already)
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        lk = opt ? ep_lock() : 0; // re-acquire to guard the armed-map updates + prime scan below
        oi = 0;
        for (int i = 0; i < r && oi < maxev; i++) {
            // The EVFILT_USER self-wake knote is an internal cross-thread nudge, not a guest event -- drop it.
            if (kv[i].filter == EVFILT_USER) continue;
            // An EV_ERROR entry is a *changelist* processing result (errno in .data), NOT a readiness
            // event. With correct armed-state tracking these do not occur; skip them if they do.
            if (opt && (kv[i].flags & EV_ERROR)) continue;
            uint32_t ev = (kv[i].filter == EVFILT_READ) ? 0x1u : (kv[i].filter == EVFILT_WRITE) ? 0x4u : 0u;
            if (kv[i].flags & EV_EOF) {
                // kqueue raises EV_EOF for BOTH a peer half-close (shutdown SHUT_WR: the read side hits
                // EOF but the socket is still writable) and a full hangup. Linux distinguishes them:
                // EPOLLRDHUP on a peer close, EPOLLHUP only once the local connection is also closed.
                // The AF_UNIX transport collapses a peer close into host POLLHUP, so its socket EOF must
                // not become guest EPOLLHUP. Non-sockets retain the poll distinction. EPOLLRDHUP is
                // edge-reported only when the guest registered it (unlike EPOLLHUP/EPOLLERR).
                int hup = 1;
                if (kv[i].filter == EVFILT_READ) {
                    struct pollfd pf = {.fd = (int)kv[i].ident, .events = POLLIN, .revents = 0};
                    if (poll(&pf, 1, 0) >= 0) hup = (pf.revents & POLLHUP) != 0;
                    {
                        int socket_type;
                        socklen_t socket_type_size = sizeof(socket_type);
                        if (getsockopt((int)kv[i].ident, SOL_SOCKET, SO_TYPE, &socket_type, &socket_type_size) == 0)
                            hup = 0;
                    }
                }
                if (hup) ev |= 0x10u;                                                            // EPOLLHUP
                if (kv[i].ident < HL_NFD && (g_ep_events[kv[i].ident] & 0x2000u)) ev |= 0x2000u; // EPOLLRDHUP
            }
            // EPOLLERR (immediate-path semantics preserved when opt is off)
            if (!opt && (kv[i].flags & EV_ERROR)) ev |= 0x8u;
            *(uint32_t *)(out + (size_t)oi * G_EPEV_SZ) = ev;
            memcpy(out + (size_t)oi * G_EPEV_SZ + G_EPEV_DOFF, &kv[i].udata, 8);
            // EPOLLONESHOT: the kernel auto-removed this registration; keep our armed map in sync.
            if (kv[i].ident < HL_NFD && g_ep_os[kv[i].ident]) {
                if (kv[i].filter == EVFILT_READ)
                    g_ep_rd[kv[i].ident] = 0;
                else if (kv[i].filter == EVFILT_WRITE)
                    g_ep_wr[kv[i].ident] = 0;
            }
            if (kv[i].ident < HL_NFD) ep_native_disarm(epoll_slot(ep), (int)kv[i].ident, kv[i].filter);
            oi++;
        }
        /* Provider pumps only publish an atomic readiness mark and trigger the
         * EVFILT_USER wake.  The epoll owner consumes and formats it here, so
         * callbacks never mutate epoll queues or acquire inherited locks. */
        int registry_ep = epoll_slot(ep);
        uint32_t provider_ep_generation =
            registry_ep >= 0 && registry_ep < HL_NFD ? g_ep_provider_generations[registry_ep] : 0;
        for (uint32_t provider_index = 0; provider_index < EP_PROVIDER_WATCH_LIMIT && oi < maxev; ++provider_index) {
            ep_provider_watch *provider_watch = &g_ep_provider_watches[provider_index];
            if (atomic_load_explicit(&provider_watch->state, memory_order_acquire) != EP_PROVIDER_ACTIVE ||
                provider_watch->epoll != registry_ep || provider_watch->epoll_generation != provider_ep_generation)
                continue;
            hl_linux_fd_snapshot provider_snapshot;
            if (g_linux_box == NULL ||
                hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)provider_watch->descriptor, &provider_snapshot) !=
                    HL_STATUS_OK ||
                provider_snapshot.descriptor_generation != provider_watch->descriptor_generation ||
                provider_snapshot.host_handle != provider_watch->handle) {
                ep_provider_retire(provider_watch);
                continue;
            }
            uint32_t level = 0;
            if (!(provider_watch->events & 0x80000000u) && !(provider_watch->events & 0x40000000u))
                level = hl_provider_files_cached_readiness(provider_watch->handle, provider_watch->interests);
            int unsubscribe = 0;
            uint32_t provider_ready = ep_provider_take_ready(provider_watch, level, &unsubscribe);
            if (provider_ready == 0) continue;
            *(uint32_t *)(out + (size_t)oi * G_EPEV_SZ) = ep_provider_linux_events(provider_ready);
            memcpy(out + (size_t)oi * G_EPEV_SZ + G_EPEV_DOFF, &provider_watch->data, sizeof(provider_watch->data));
            if (unsubscribe) {
                hl_provider_files_unsubscribe(provider_watch->handle, provider_watch,
                                              atomic_load(&provider_watch->serial));
            }
            oi++;
        }
        // Deliver edge-triggered primes that kqueue didn't surface (fds already ready at registration).
        // This is the cross-thread-readiness delivery: a peer M that registered an already-ready fd
        // stashed a prime here, so a wake that carried no kqueue edge still hands the guest the ready fd.
        if (ep >= 0 && ep < HL_NFD && g_ep_primen[ep] > 0) {
            int kept = 0;
            for (int i = 0; i < g_ep_primen[ep]; i++) {
                struct kevent *pk = &g_ep_prime[ep][i];
                uint32_t pev = (pk->filter == EVFILT_READ) ? 0x1u : 0x4u;
                int dup = 0;
                for (int j = 0; j < oi; j++) {
                    uint32_t jev;
                    uint64_t ju;
                    memcpy(&jev, out + (size_t)j * G_EPEV_SZ, 4);
                    memcpy(&ju, out + (size_t)j * G_EPEV_SZ + G_EPEV_DOFF, 8);
                    if (ju == (uint64_t)pk->udata && (jev & pev)) {
                        dup = 1;
                        break;
                    }
                }
                if (dup) continue; // kqueue already reported it
                if (oi >= maxev) {
                    g_ep_prime[ep][kept++] = *pk;
                    continue;
                } // no room -> keep for next wait
                *(uint32_t *)(out + (size_t)oi * G_EPEV_SZ) = pev;
                memcpy(out + (size_t)oi * G_EPEV_SZ + G_EPEV_DOFF, &pk->udata, 8);
                oi++;
            }
            g_ep_primen[ep] = kept;
        }
        ep_unlock(lk);
        // Object-backed watches (inotify): no host fd feeds the kqueue, so sample the object's readiness
        // on this bounded tick and format the event here, exactly as poll()/select() observe the same
        // typed objects. Runs after ep_unlock so the object mutex is never taken under the epoll lock.
        if (registry_ep >= 0 && registry_ep < HL_NFD && g_ep_object_count[registry_ep] > 0) {
            uint32_t obj_ep_generation = g_ep_provider_generations[registry_ep];
            for (uint32_t oidx = 0; oidx < EP_OBJECT_WATCH_LIMIT && oi < maxev; ++oidx) {
                ep_object_watch *ow = &g_ep_object_watches[oidx];
                if (atomic_load_explicit(&ow->active, memory_order_acquire) == 0 || ow->epoll != registry_ep ||
                    ow->epoll_generation != obj_ep_generation)
                    continue;
                hl_linux_fd_snapshot osnap;
                hl_linux_object_pin opin;
                if (g_linux_box == NULL ||
                    hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)ow->descriptor, &osnap) != HL_STATUS_OK ||
                    osnap.descriptor_generation != ow->descriptor_generation) {
                    ep_object_free(ow); // the watched fd was closed or reused
                    continue;
                }
                if (hl_linux_object_pin_fd(g_linux_box, (hl_linux_fd)ow->descriptor, &opin) != HL_STATUS_OK) continue;
                uint32_t readiness = hl_linux_object_ready(&opin, ow->interests);
                hl_linux_object_unpin(&opin);
                uint32_t oev = ep_provider_linux_events(readiness);
                if (oev == 0) continue;
                *(uint32_t *)(out + (size_t)oi * G_EPEV_SZ) = oev;
                memcpy(out + (size_t)oi * G_EPEV_SZ + G_EPEV_DOFF, &ow->data, sizeof(ow->data));
                oi++;
                if (ow->events & 0x40000000u) ep_object_free(ow); // EPOLLONESHOT: one delivery only
            }
        }
        // Re-block instead of returning a spurious 0. A bare cross-thread wake (or a changelist that only
        // produced EV_ERROR echoes) leaves oi==0 while the guest still asked to block. timeout_ns<0: always
        // loop (epoll_wait(-1) must never return 0). timeout_ns>0: loop until the monotonic deadline elapses.
        // timeout_ns==0: returning 0 is correct (non-blocking poll) -- never loop.
        if (oi == 0 && timeout_ns != 0) {
            if (timeout_ns < 0) continue;
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t rem = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
            if (rem > 0) continue;
        }
        if (oi > 0 && guest_copy_to(guest_out, out, (size_t)oi * G_EPEV_SZ) != (ssize_t)((size_t)oi * G_EPEV_SZ))
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
        else
            G_RET(c) = (uint64_t)oi;
        break;
    }
    if (sm_on) poll_sigmask_leave(c, sm_saved);
}

#include "event/epoll_control.c"
#include "event/epoll_wait.c"
#include "event/inotify.c"
#include "event/poll.c"
#include "event/signal_timer.c"

static int svc_event(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                     uint64_t a5) {
    switch (nr) {
    case 19: return svc_eventfd2(c, nr, a0, a1, a2, a3, a4, a5);
    case 20: return svc_epoll_create1(c, nr, a0, a1, a2, a3, a4, a5);
    case 21: return svc_epoll_ctl(c, nr, a0, a1, a2, a3, a4, a5);
    case 22: return svc_epoll_pwait(c, nr, a0, a1, a2, a3, a4, a5);
    case 441: return svc_epoll_pwait2(c, nr, a0, a1, a2, a3, a4, a5);
    case 26: return svc_inotify_init1(c, nr, a0, a1, a2, a3, a4, a5);
    case 27: return svc_inotify_add_watch(c, nr, a0, a1, a2, a3, a4, a5);
    case 28: return svc_inotify_rm_watch(c, nr, a0, a1, a2, a3, a4, a5);
    case 72: return svc_pselect6(c, nr, a0, a1, a2, a3, a4, a5);
    case 73: return svc_ppoll(c, nr, a0, a1, a2, a3, a4, a5);
    case 74: return svc_signalfd4(c, nr, a0, a1, a2, a3, a4, a5);
    case 85: return svc_timerfd_create(c, nr, a0, a1, a2, a3, a4, a5);
    case 86: return svc_timerfd_settime(c, nr, a0, a1, a2, a3, a4, a5);
    case 87: return svc_timerfd_gettime(c, nr, a0, a1, a2, a3, a4, a5);
    default: return 0;
    }
}
