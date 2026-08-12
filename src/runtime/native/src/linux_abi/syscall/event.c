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

// Re-arm one watched fd's kqueue knotes on epoll instance `ep`, from its recorded interest (events+udata).
// Shared by the fork-child rebuild and the close-with-surviving-dup re-home. `ident` is the fd whose knote
// is (re)armed on the kqueue; `slot` is the interest-table fd the events/udata come from (they differ only
// when re-homing a closed fd onto a surviving alias). Returns the armed read/write direction bits.
static void ep_rearm_from_interest(int ep, int ident, int slot) {
    uint32_t ev = g_ep_events[slot];
    uint16_t xf = (uint16_t)((ev & 0x80000000u ? EV_CLEAR : 0) | (ev & 0x40000000u ? EV_ONESHOT : 0));
    void *ud = (void *)g_ep_udata[slot];
    struct kevent kv[2];
    int n = 0;
    if (ev & 0x1) { // EPOLLIN
        EV_SET(&kv[n++], ident, EVFILT_READ, EV_ADD | xf, 0, 0, ud);
    }
    if (ev & 0x4) { // EPOLLOUT
        EV_SET(&kv[n++], ident, EVFILT_WRITE, EV_ADD | xf, 0, 0, ud);
    }
    for (int i = 0; i < n; i++) {
        kevent(ep, &kv[i], 1, NULL, 0, NULL);
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
                // EPOLLRDHUP on a half-close, EPOLLHUP only on a full disconnect. poll() reports POLLHUP
                // only for a full hangup, so use it to tell the two apart. EPOLLRDHUP is edge-reported
                // only when the guest actually registered interest in it (unlike EPOLLHUP/EPOLLERR).
                int hup = 1;
                if (kv[i].filter == EVFILT_READ) {
                    struct pollfd pf = {.fd = (int)kv[i].ident, .events = POLLIN, .revents = 0};
                    if (poll(&pf, 1, 0) >= 0) hup = (pf.revents & POLLHUP) != 0;
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

static int svc_event(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                     uint64_t a5) {
    switch (nr) {
    // ===================== Event loop — epoll/eventfd/timerfd/signalfd/inotify (macOS kqueue) =====================
    // eventfd2(initval, flags) -> pipe
    case 19: {
        // Validate `flags` exactly as Linux (fs/eventfd.c): only EFD_SEMAPHORE(1) | EFD_NONBLOCK(O_NONBLOCK
        // 0x800) | EFD_CLOEXEC(O_CLOEXEC 0x80000) are defined; any other bit -> EINVAL. IPC runtimes
        // EventFDNotifier::KernelSupported() probes eventfd2(0, ~0) and PCHECKs it FAILS with EINVAL/ENOSYS/
        // EPERM; without this the probe succeeded and the caller aborted.
        if ((unsigned)a1 & ~(unsigned)(1u | 0x800u | 0x80000u)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int fds[2];
        if (pipe(fds) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        int peer = fds[1];
        int hi = fcntl(peer, F_DUPFD, 1 << 20);
        if (hi < 0) hi = fcntl(peer, F_DUPFD, 64);
        if (hi >= 0) {
            close(peer);
            peer = hi;
        }
        if (a1 & 0x80000) {
            fcntl(fds[0], F_SETFD, FD_CLOEXEC);
            fcntl(peer, F_SETFD, FD_CLOEXEC);
            // EFD_CLOEXEC
        }
        // Keep the read end PERMANENTLY O_NONBLOCK at the host level so the counter/pipe drains in io.c
        // never toggle the (cross-process-shared) fd flags. The guest's real EFD_NONBLOCK intent is tracked
        // in g_eventfd_gnb and honoured by the read path (poll() when the guest wants to block). See the
        // g_eventfd_gnb note in vfs.c.
        // Record whether the host honoured it. A host with no per-descriptor status-flag channel refuses,
        // and the drain/wait paths in io.c must know: their "read until it stops returning bytes" idiom
        // terminates only on a non-blocking read end, and on a blocking one the first drain of an empty
        // pipe never returns. See g_eventfd_readend_nb.
        g_eventfd_readend_nb = fcntl(fds[0], F_SETFL, O_NONBLOCK) == 0;
        // writes to the eventfd go to fds[1]; the counter + sema-flag live alongside.
        // Defensive: the accumulating-counter arena is bound once per process (eventfd_count_init, from a
        // cold hl_run_linux_guest or the fork-server prewarm parent). If it is somehow still NULL, indexing
        // it would SIGSEGV the process (fatal in a resident fork-server parent) -- report ENOMEM instead.
        if (!g_eventfd_count) {
            close(fds[0]);
            close(peer);
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        if (fds[0] < 0 || fds[0] >= HL_NFD) {
            // No per-fd tracking slot for a host fd past the table: the eventfd would be untrackable AND the
            // dup'd `peer` write end (not stored in g_eventfd_peer) would leak. Close both and report EMFILE,
            // matching the fd-past-cap convention used across io.c/fs.c (host fd table is far larger).
            close(fds[0]);
            close(peer);
            G_RET(c) = (uint64_t)(int64_t)(-EMFILE);
            break;
        }
        g_eventfd_peer[fds[0]] = peer + 1;
        g_eventfd_cslot[fds[0]] = fds[0] + 1;
        g_eventfd_sema[fds[0]] = (a1 & 1) != 0;          // EFD_SEMAPHORE
        eventfd_guest_nb_set(fds[0], (a1 & 0x800) != 0); // OFD-shared guest non-blocking intent
        g_eventfd_count[fds[0]] = a0;                    // initval
        g_eventfd_refs[fds[0]] = 1;                      // one alias (this fd); dup() bumps it (fd_carry_virt)
        if (a0 > 0) {
            char b = 1;
            if (write(peer, &b, 1) < 0) {}
        } // make it readable
        G_RET(c) = (uint64_t)fds[0];
        break;
    }
    case 20: {
        // epoll_create1(flags) -> kqueue. Only EPOLL_CLOEXEC (0x80000) is defined; any other bit is
        // rejected with EINVAL by the Linux kernel (LTP epoll_create1_01).
        if (a0 & ~0x80000u) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int r = kqueue();
        // EPOLL_CLOEXEC
        // macOS kqueue() defaults FD_CLOEXEC SET; Linux epoll_create1(0) leaves it CLEAR. Set it exactly
        // per the EPOLL_CLOEXEC flag (clear when absent) so epoll_create1_01's no-CLOEXEC assertion holds.
        if (r >= 0) fcntl(r, F_SETFD, (a0 & 0x80000) ? FD_CLOEXEC : 0);
        // a reused fd number must start with an empty prime buffer + no stale wake knote + no stale membership
        // (close() doesn't clear ours -- this is how an epoll fd's per-instance state is reset on reuse)
        if (r >= 0 && r < HL_NFD) {
            g_ep_provider_generations[r] = ep_provider_next(g_ep_provider_generations[r]);
            ep_object_retire_endpoint(r);
            g_ep_primen[r] = 0;
            g_ep_wake_armed[r] = 0;
            g_epoll[r] = 1;
            g_ep_cslot[r] = (uint16_t)(r + 1);
            g_epoll_family_seen = 1;
            ep_native_retire_epoll(r);
            ep_mem_clear(r);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // epoll_ctl(epfd, op, fd, event) -> kevent
    case 21: {
        int op = (int)a1, fd = (int)a2, epfd = (int)a0;
        uint8_t event[G_EPEV_SZ];
        int registry_ep = epoll_slot(epfd);
        uint32_t ev = 0;
        uint64_t data = (uint64_t)(unsigned)fd;
        // (extends): epoll_ctl(2) full error surface, in the kernel's exact ORDER (LTP
        // epoll_ctl02). kqueue silently accepts bad ops/fds and never faults on a NULL event, so enforce
        // each Linux return explicitly. Every check below fires ONLY on input that already errors on Linux,
        // so a well-formed ADD/MOD/DEL is behaviourally unchanged (it costs one extra fstat for the EBADF/
        // EPERM probe -- the ADD path already did that fstat).
        // (1) EFAULT: ADD(1)/MOD(3) -- any op that "has an event" (op != DEL) -- dereference `event`.
        if (op != 2 && (!a3 || guest_copy_from(event, a3, G_EPEV_SZ) != G_EPEV_SZ)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) EBADF: epfd must be an open fd. A engine-tracked epoll (g_epoll set) is known-valid -> only an
        // untracked epfd is probed (a dup'd/large epoll fd keeps the best-effort immediate path).
        if (!(epfd >= 0 && epfd < HL_NFD && g_epoll[epfd]) && fcntl(epfd, F_GETFD) == -1) {
            G_RET(c) = (uint64_t)(-EBADF);
            break;
        }
        // (3) EINVAL: cannot add the epoll fd to itself. Checked before the fd fstat -- epfd is a kqueue,
        // which is a valid pollable fd, so this avoids relying on fstat's shape for a kqueue.
        if (fd == epfd) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (4) EBADF if fd is not open; (5) EPERM if it is open but cannot be polled (a regular file /
        // directory is not epoll-watchable). One fstat serves both -- gated to ADD only (as the path
        // was) so the hot MOD/DEL rearm path (Go's EPOLLONESHOT netpoller) stays fstat-free; a MOD/DEL of a
        // bad/unregistered fd still resolves correctly via the ENOENT membership check below.
        if (op == 1) {
            struct stat st;
            if (fstat(fd, &st) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (S_ISREG(st.st_mode) || S_ISDIR(st.st_mode)) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
        }
        // (6) EINVAL: op must be ADD/DEL/MOD.
        if (op != 1 && op != 2 && op != 3) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (a3) {
            if (op == 2 && guest_copy_from(event, a3, G_EPEV_SZ) != G_EPEV_SZ) memset(event, 0, sizeof(event));
            memcpy(&ev, event, sizeof(ev));
            memcpy(&data, event + G_EPEV_DOFF, 8);
            // struct epoll_event {u32 events; [pad;] u64 data} -- layout per guest arch (see G_EPEV_*)
        }
        // EPOLLEXCLUSIVE (1<<28) may be specified only at EPOLL_CTL_ADD. Linux (fs/eventpoll.c) rejects it in
        // an EPOLL_CTL_MOD event, and rejects any EPOLL_CTL_MOD of a registration that was ADDed exclusive,
        // both with EINVAL. Checked before the membership ENOENT probe to match the kernel's error order.
        if (op == 3 && ((ev & 0x10000000u) || (epfd >= 0 && epfd < HL_NFD && g_epoll[epfd] && fd >= 0 && fd < HL_NFD &&
                                               (g_ep_events[fd] & 0x10000000u)))) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (7/8/9) EEXIST (ADD an already-registered fd) / ENOENT (MOD|DEL an absent fd) on a engine-tracked epoll
        // instance (membership bitmap). Confined to fd < HL_NFD, matching the readiness path below.
        if (epfd >= 0 && epfd < HL_NFD && g_epoll[epfd] && registry_ep >= 0 && registry_ep < HL_NFD && fd >= 0 &&
            fd < HL_NFD) {
            int ep = registry_ep;
            int member = ep_mem_test(ep, fd);
            if (op == 1 && member) {
                G_RET(c) = (uint64_t)(-EEXIST);
                break;
            } // ADD an already-registered fd
            if ((op == 2 || op == 3) && !member) {
                G_RET(c) = (uint64_t)(-ENOENT);
                break;
            } // MOD/DEL an absent fd
            ep_mem_set(ep, fd, op != 2); // commit membership
        }
        // Record this registration in the per-instance interest table (fd -> owner + events + udata) so it
        // survives fork (re-armed on the rebuilt child kqueue) and a watched-fd close whose OFD lives on via a
        // dup (re-homed onto the surviving alias). DEL drops the entry. Confined to in-range epfd/fd, matching
        // the readiness path; a couple of fd-indexed stores, so the epoll_ctl hot path is essentially unchanged.
        if (epfd >= 0 && epfd < HL_NFD && g_epoll[epfd] && registry_ep >= 0 && registry_ep < HL_NFD && fd >= 0 &&
            fd < HL_NFD) {
            if (op == 2) { // DEL
                g_ep_owner[fd] = 0;
                g_ep_events[fd] = 0;
                g_ep_udata[fd] = 0;
            } else { // ADD / MOD
                g_ep_owner[fd] = registry_ep + 1;
                g_ep_events[fd] = ev;
                g_ep_udata[fd] = data;
            }
            if (ep_native_set(registry_ep, fd, op, ev, data) != 0) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
        }
        // op: 1=ADD 2=DEL 3=MOD ; EPOLLET=0x80000000 -> EV_CLEAR ; EPOLLONESHOT=0x40000000 -> EV_ONESHOT
        uint16_t xf = (uint16_t)((ev & 0x80000000u ? EV_CLEAR : 0) | (ev & 0x40000000u ? EV_ONESHOT : 0));
        int want_rd = (op != 2) && (ev & 0x1); // EPOLLIN
        int want_wr = (op != 2) && (ev & 0x4); // EPOLLOUT
        if (epopt_on() && (int)a0 >= 0 && (int)a0 < HL_NFD && !g_ep_dupd[(int)a0] && fd >= 0 && fd < HL_NFD) {
            // W3E fast path: track armed filters, defer the change to the next epoll_wait kevent(). A dup'd
            // instance is excluded (g_ep_dupd) so its interest is submitted immediately to the shared kqueue.
            int ep = (int)a0;
            int lk = ep_lock();
            if (want_rd) {
                ep_push(ep, fd, EVFILT_READ, EV_ADD | xf, (void *)data);
                g_ep_rd[fd] = 1;
                if (xf & EV_CLEAR)
                    ep_prime_if_ready(ep, fd, EVFILT_READ, (void *)data);
                else if (lk)
                    ep_submit_ready_level(ep, fd, EVFILT_READ, xf, (void *)data);
            } else if (g_ep_rd[fd]) {
                ep_push(ep, fd, EVFILT_READ, EV_DELETE, (void *)data);
                g_ep_rd[fd] = 0;
            }
            if (want_wr) {
                ep_push(ep, fd, EVFILT_WRITE, EV_ADD | xf, (void *)data);
                g_ep_wr[fd] = 1;
                if (xf & EV_CLEAR)
                    ep_prime_if_ready(ep, fd, EVFILT_WRITE, (void *)data);
                else if (lk)
                    ep_submit_ready_level(ep, fd, EVFILT_WRITE, xf, (void *)data);
            } else if (g_ep_wr[fd]) {
                ep_push(ep, fd, EVFILT_WRITE, EV_DELETE, (void *)data);
                g_ep_wr[fd] = 0;
            }
            g_ep_os[fd] = (op != 2 && (ev & 0x40000000u)) ? 1 : 0;
            // Nested epoll: arm member knotes eagerly instead of deferring, since a nested inner epoll is
            // never epoll_wait'd by the guest to consume its changelist. Case A: we just added an inner epoll
            // fd into this outer -> flush the inner (fd) so it starts reporting its members' readiness. Case B:
            // this instance is itself watched by an outer (g_ep_owner) -> flush its own members now.
            if (op != 2 && fd != ep && fd >= 0 && fd < HL_NFD && g_epoll[fd]) ep_submit_changes(fd);
            if (g_ep_owner[ep]) ep_submit_changes(ep);
            // Multi-threaded guest: a peer M may be blocked in epoll_wait on this instance right now, so the
            // deferred registration/prime must reach it -- flush the changelist to the kernel and (when we
            // added/modified interest) wake the peer to re-scan primes. No-op single-threaded, where the same
            // thread issues the next epoll_wait and consumes the changelist itself.
            if (lk) ep_flush(ep, op != 2);
            ep_unlock(lk);
            G_RET(c) = 0;
            break;
        }
        // ---- original immediate path (NOEPOLLOPT=1 or fd/epfd out of range) ----
        struct kevent kv[2];
        int n = 0;
        uint16_t base = (op == 2) ? EV_DELETE : EV_ADD;
        if (op == 2 || (ev & 0x1)) {
            EV_SET(&kv[n], fd, EVFILT_READ, base | xf, 0, 0, (void *)data);
            n++;
            // EPOLLIN
        }
        if (op == 2 || (ev & 0x4)) {
            EV_SET(&kv[n], fd, EVFILT_WRITE, base | xf, 0, 0, (void *)data);
            n++;
            // EPOLLOUT
        }
        for (int i = 0; i < n; i++) {
            // per-filter so DEL of an absent one is ignored
            kevent((int)a0, &kv[i], 1, NULL, 0, NULL);
        }
        if ((int)a0 >= 0 && (int)a0 < HL_NFD && fd >= 0 && fd < HL_NFD) {
            g_ep_rd[fd] = want_rd ? 1 : 0;
            g_ep_wr[fd] = want_wr ? 1 : 0;
            g_ep_os[fd] = (op != 2 && (ev & UINT32_C(0x40000000))) ? 1 : 0;
        }
        // EPOLLET: prime an already-ready fd so its initial readiness is reported (see g_ep_prime).
        if ((xf & EV_CLEAR) && op != 2) {
            if (want_rd) ep_prime_if_ready((int)a0, fd, EVFILT_READ, (void *)data);
            if (want_wr) ep_prime_if_ready((int)a0, fd, EVFILT_WRITE, (void *)data);
        }
        G_RET(c) = 0;
        break;
    }
    // epoll_pwait(epfd, events, max, timeout_ms, sigmask)
    case 22: {
        int maxev = (int)a2;
        // Linux rejects maxevents <= 0 with EINVAL before waiting; do not clamp it to a poll.
        if (maxev <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (maxev > 256) maxev = 256;
        // epoll_pwait(epfd, events, max, tmo, sigmask, sigsetsize): a4 is the guest sigset_t pointer, a5 its
        // size. Apply the temporary signal mask for the wait (Linux swaps it atomically); NULL a4 = no mask.
        uint64_t sm_set = 0;
        int have_mask = 0;
        if (a4) {
            if ((size_t)a5 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (guest_copy_from(&sm_set, a4, sizeof(sm_set)) != sizeof(sm_set)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            have_mask = 1;
        }
        // guest timeout ms (a3): <0 = infinite, 0 = poll, >0 = finite. Widen to nanoseconds for the
        // shared wait core (which epoll_pwait2 also drives with a sub-ms timespec).
        int32_t tmo_ms = (int32_t)a3;
        int64_t timeout_ns = tmo_ms < 0 ? -1 : (int64_t)tmo_ms * 1000000LL;
        svc_epoll_wait_common(c, (int)a0, a1, maxev, timeout_ns, have_mask, sm_set);
        break;
    }
    // epoll_pwait2(epfd, events, max, timeout(const struct timespec*), sigmask, sigsetsize)
    case 441: {
        // Same as epoll_pwait (case 22) except the timeout is a NANOSECOND-resolution struct timespec at
        // a3 (NULL = block forever) instead of an int millisecond count, so sub-ms waits are honored
        // exactly. tokio/glibc>=2.35 issue this when the kernel supports it; unimplemented it read as
        // ENOSYS and every wait failed. Argument order: a3=timeout*, a4=sigmask*, a5=sigsetsize.
        int maxev = (int)a2;
        // Linux rejects maxevents <= 0 with EINVAL before touching the timeout/sigmask.
        if (maxev <= 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (maxev > 256) maxev = 256;
        // NULL timeout -> infinite; otherwise read + validate the timespec (tv_nsec in [0,1e9), tv_sec>=0)
        // and fold it to a single nanosecond budget for the shared wait core.
        int64_t timeout_ns = -1;
        if (a3) {
            struct timespec to;
            if (guest_copy_from(&to, a3, sizeof(to)) != sizeof(to)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            if (to.tv_sec < 0 || to.tv_nsec < 0 || to.tv_nsec >= 1000000000L) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            timeout_ns = (int64_t)to.tv_sec * 1000000000LL + to.tv_nsec;
        }
        // sigmask (a4) + sigsetsize (a5): identical contract to epoll_pwait -- size must be 8, mask readable.
        uint64_t sm_set = 0;
        int have_mask = 0;
        if (a4) {
            if ((size_t)a5 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (guest_copy_from(&sm_set, a4, sizeof(sm_set)) != sizeof(sm_set)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            have_mask = 1;
        }
        svc_epoll_wait_common(c, (int)a0, a1, maxev, timeout_ns, have_mask, sm_set);
        break;
    }
    case 26: {
        // inotify_init1(flags) -> kqueue. Only IN_NONBLOCK(0x800) and IN_CLOEXEC(0x80000) are defined;
        // Linux rejects any other flag bit with EINVAL, so a bad-flag probe must not read as supported.
        if ((int)a0 & ~(0x800 | 0x80000)) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int r;
#if defined(__linux__)
        r = inotify_init1((int)a0);
#else
        r = kqueue();
        if (r >= 0) {
            if (r < HL_NFD) {
                g_inotify[r] = 1;
                g_epoll_family_seen = 1;
                g_inotify_nb[r] = (a0 & 0x800) ? 1 : 0; // remember IN_NONBLOCK for the fork-child kqueue rebuild
            }
            if (a0 & 0x800) fcntl(r, F_SETFL, O_NONBLOCK);
            // macOS kqueue() defaults FD_CLOEXEC SET; Linux inotify_init1(0) leaves it CLEAR. Set it exactly
            // per IN_CLOEXEC (clearing the kqueue default otherwise) so an inotify fd created without the
            // flag survives exec instead of being swept by hl's close-on-exec pass.
            fcntl(r, F_SETFD, (a0 & 0x80000) ? FD_CLOEXEC : 0);
        }
#endif
        if (r >= 0 && r < HL_NFD) {
            g_inotify[r] = 1;
            g_epoll_family_seen = 1;
            g_inotify_nb[r] = (a0 & 0x800) ? 1 : 0;
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // inotify_add_watch(fd, path, mask) -- kqueue EVFILT_VNODE
    case 27: {
        char pb[4200];
        char imported_path[4200];
        // EFAULT on an inaccessible path pointer BEFORE atpath dereferences it -- inotify_add_watch(fd, NULL,
        // mask) and a wild/unmapped path both return -EFAULT on Linux; without this guard atpath reads the
        // unmapped guest address and the engine SIGSEGVs (guest-triggerable crash). guest_bad_ptr also catches
        // a PROT_NONE guard page that host_range_mapped alone would miss. Mirrors the sibling *at path syscalls
        // in fs.c (openat/newfstatat/unlinkat all guard `!a1 || guest_bad_ptr(a1, 1)` first).
        if (guest_copy_string(imported_path, sizeof imported_path, a1) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // confined (realpath gate)
        const char *p = atpath(-100, imported_path, pb, sizeof pb, 0);
#if defined(__linux__)
        int wd = inotify_add_watch((int)a0, p, (uint32_t)a2);
        if (wd >= 0 && wd < HL_NFD) {
            struct stat dst;
            snprintf(g_inotify_wpath[wd], sizeof g_inotify_wpath[wd], "%s", p);
            g_inotify_mask[wd] = (uint32_t)a2;
            g_inotify_isdir[wd] = stat(p, &dst) == 0 && S_ISDIR(dst.st_mode);
            if (g_inotify_isdir[wd]) {
                free(g_inotify_snap[wd]);
                g_inotify_snap[wd] = dir_snapshot(p);
            }
            g_inotify_owner[wd] = (int)a0;
        }
        G_RET(c) = wd < 0 ? (uint64_t)(-errno) : (uint64_t)wd;
#else
        int wfd = hl_native_open_watch(p);
        if (wfd < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        struct kevent kv;
        EV_SET(&kv, wfd, EVFILT_VNODE, EV_ADD | EV_CLEAR,
               NOTE_WRITE | NOTE_DELETE | NOTE_RENAME | NOTE_ATTRIB | NOTE_EXTEND, 0, (void *)(intptr_t)wfd);
        if (kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0) {
            int e = errno;
            close(wfd);
            G_RET(c) = (uint64_t)(-(int64_t)e);
            break;
        }
        // Remember every watched path/mask; directories also retain a name snapshot for create/delete diffs.
        struct stat dst;
        if (wfd >= 0 && wfd < HL_NFD) {
            snprintf(g_inotify_wpath[wfd], sizeof g_inotify_wpath[wfd], "%s", p);
            g_inotify_mask[wfd] = (uint32_t)a2;
            g_inotify_isdir[wfd] = stat(p, &dst) == 0 && S_ISDIR(dst.st_mode);
            if (g_inotify_isdir[wfd]) {
                free(g_inotify_snap[wfd]);
                g_inotify_snap[wfd] = dir_snapshot(p);
            }
            g_inotify_owner[wfd] = (int)a0; // the inotify instance this watch belongs to (for the move queue)
        }
        G_RET(c) = (uint64_t)wfd;
#endif
        break;
        // watch descriptor = the watched fd
    }
    case 28: {
#if defined(__linux__)
        int result = inotify_rm_watch((int)a0, (int)a1);
        G_RET(c) = result < 0 ? (uint64_t)(-errno) : 0;
#else
        struct kevent kv;
        // inotify_rm_watch(fd, wd). The wd is the watched fd; deleting the EVFILT_VNODE knote from THIS
        // inotify kqueue is the source of truth for "is this a real watch of this instance". If the knote
        // is not registered here (bad/foreign wd), kevent fails ENOENT -- Linux returns EINVAL and leaves
        // the fd alone, so we must NOT close(wd) or we would silently destroy an unrelated guest fd.
        EV_SET(&kv, (int)a1, EVFILT_VNODE, EV_DELETE, 0, 0, NULL);
        if (kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        close((int)a1);
        G_RET(c) = 0;
#endif
        break;
    }
    case 72: { // pselect6(nfds, readfds, writefds, exceptfds, timeout(timespec), sigmask) -> pselect.
        // The Linux/macOS fd_set byte-layout is identical (bit N at byte N/8), so pass the sets through.
        int have_to = a4 != 0;
        fd_set read_set, write_set, except_set;
        fd_set *read_host = NULL, *write_host = NULL, *except_host = NULL;
        struct timespec timeout_value;
        // EFAULT on any inaccessible fd_set / timeout pointer -- incl. a PROT_NONE guard page (LTP's
        // tst_get_bad_addr), which host_range_mapped alone misses since hl force-maps guest anon host-RW.
        int selnfds = (int)a0;
        size_t nb = selnfds > 0 ? ((size_t)selnfds + 7) / 8 : 0;
        if (nb > sizeof(fd_set)) nb = sizeof(fd_set);
        if ((a1 && guest_copy_from(&read_set, a1, nb) != (ssize_t)nb) ||
            (a2 && guest_copy_from(&write_set, a2, nb) != (ssize_t)nb) ||
            (a3 && guest_copy_from(&except_set, a3, nb) != (ssize_t)nb) ||
            (have_to && guest_copy_from(&timeout_value, a4, sizeof(timeout_value)) != sizeof(timeout_value))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (a1) read_host = &read_set;
        if (a2) write_host = &write_set;
        if (a3) except_host = &except_set;
        // Linux rejects an out-of-range timeout nanoseconds field (tv_nsec < 0 or >= 1e9) with EINVAL
        // before waiting; hl must not treat it as a normal timeout and hide the caller bug.
        if (have_to) {
            long tns = timeout_value.tv_nsec;
            if (tns < 0 || tns >= 1000000000L) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // pselect6 6th arg (a5): pointer to { const sigset_t *ss; size_t ss_len; }. Resolve the guest sigset
        // address so the wait honours the temporary signal mask Linux swaps in atomically (see
        // poll_sigmask_enter). NULL a5 (or a NULL inner ss) = no temporary mask.
        uint64_t sm_set = 0, sm_saved = 0;
        int have_mask = 0;
        if (a5) {
            uint64_t pk[2];
            if (guest_copy_from(pk, a5, sizeof(pk)) != sizeof(pk)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            uint64_t ssp = pk[0], sslen = pk[1];
            if (ssp) {
                if (sslen != 8) {
                    G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                    break;
                }
                if (guest_copy_from(&sm_set, ssp, sizeof(sm_set)) != sizeof(sm_set)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                have_mask = 1;
            }
        }
        // a spurious EINTR (a signal hl hooks with host_sigh but the guest has BLOCKED or defaults to
        // ignore -- e.g. an LTP heartbeat, or SIGCHLD from a reaped child) interrupts the host pselect but
        // must NOT restart the FULL original timeout: that overshoots the deadline and, under a *repeating*
        // spurious wakeup, never reaches it -> select02 hangs (and every timing sample overshoots -> the
        // select01/poll02/pselect01 tst_timer FAILs). Capture a monotonic deadline once and re-block only
        // for the time that remains, exactly like epoll_pwait (case 22). Linux ALSO writes the leftover time
        // back into the timeout struct (both select(2) and the raw pselect6(2) syscall do), so mirror that.
        struct timespec deadline = {0, 0};
        if (have_to) {
            struct timespec ts = timeout_value;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += ts.tv_sec;
            deadline.tv_nsec += ts.tv_nsec;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
        }
        int sm_on = poll_sigmask_enter(c, have_mask, sm_set, &sm_saved);
        int r;
        for (;;) {
            struct timespec rem, *tsp = NULL;
            if (have_to) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns < 0) ns = 0;
                rem.tv_sec = (time_t)(ns / 1000000000LL);
                rem.tv_nsec = (long)(ns % 1000000000LL);
                tsp = &rem;
            }
            ts_wait_enter();
            r = pselect((int)a0, read_host, write_host, except_host, tsp, NULL);
            ts_wait_leave(); // S while blocked (glibc pause on aarch64 lands in ppoll below; select here)
            // pselect is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry),
            // and then only for the time that remains (recomputed above), never the full budget again.
            if (r < 0 && svc_poll_retry(c)) continue;
            break;
        }
        if (sm_on) poll_sigmask_leave(c, sm_saved);
        if (r >= 0 && have_to) {
            struct timespec now;
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
            int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
            if (ns < 0) ns = 0;
            timeout_value.tv_sec = (time_t)(ns / 1000000000LL);
            timeout_value.tv_nsec = (long)(ns % 1000000000LL);
        }
        int copy_fault = 0;
        if (r >= 0) {
            if ((a1 && guest_copy_to(a1, &read_set, nb) != (ssize_t)nb) ||
                (a2 && guest_copy_to(a2, &write_set, nb) != (ssize_t)nb) ||
                (a3 && guest_copy_to(a3, &except_set, nb) != (ssize_t)nb) ||
                (have_to && guest_copy_to(a4, &timeout_value, sizeof(timeout_value)) != sizeof(timeout_value)))
                copy_fault = 1;
        }
        G_RET(c) = copy_fault ? (uint64_t)(int64_t)(-EFAULT) : r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    case 73: {
        struct pollfd *fds = NULL;
        struct timespec timeout_value;
        // ppoll -> poll. macOS has no ppoll, so collapse the timespec deadline into poll's int-ms timeout.
        // skalibs iopause (s6-supervise et al.) hands a huge but FINITE relative deadline for an
        // idle-but-up service -- settimeout_infinite() makes the delta exactly tain_infinite_relative,
        // whose tv_sec = 2^61 = 2305843009213693952. On real Linux ppoll takes that timespec and blocks.
        // Here the naive (int)(tv_sec*1000) truncates: 2^61 * 1000 == 0 (mod 2^32), so tmo became 0 ->
        // poll returned immediately -> s6 saw a spurious timeout in the UP state and busy-looped printing
        // "can't happen: timeout while the service is up!". Clamp the conversion to [0, 0x7fffffff] ms.
        struct timespec *ts = a2 ? &timeout_value : NULL;
        // EFAULT on an inaccessible pollfd array (a0, a1=nfds) or timeout (a2), PROT_NONE guard page too.
        if (a1 > SIZE_MAX / sizeof(struct pollfd)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        size_t fds_bytes = (size_t)a1 * sizeof(struct pollfd);
        fds = calloc(a1 ? (size_t)a1 : 1, sizeof(*fds));
        if (!fds) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        if ((a0 && a1 && guest_copy_from(fds, a0, fds_bytes) != (ssize_t)fds_bytes) ||
            (ts && guest_copy_from(ts, a2, sizeof(*ts)) != sizeof(*ts))) {
            free(fds);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Linux rejects an out-of-range timeout nanoseconds field (tv_nsec < 0 or >= 1e9) with EINVAL.
        if (ts && (ts->tv_sec < 0 || ts->tv_nsec < 0 || ts->tv_nsec >= 1000000000L)) {
            free(fds);
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // ppoll(fds, n, tmo, sigmask, sigsetsize): a3 is the guest sigset_t pointer, a4 its size. Apply the
        // temporary signal mask for the duration of the wait (Linux swaps it atomically); NULL a3 = no mask.
        uint64_t sm_set = 0, sm_saved = 0;
        int have_mask = 0;
        if (a3) {
            if ((size_t)a4 != 8) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                free(fds);
                break;
            }
            if (guest_copy_from(&sm_set, a3, sizeof(sm_set)) != sizeof(sm_set)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                free(fds);
                break;
            }
            have_mask = 1;
        }
        int have_to = ts != NULL;
        // like pselect (case 72), a spurious EINTR must re-block only for the REMAINING time, not the
        // full budget again -- otherwise a repeating hooked-but-blocked signal restarts the timeout forever
        // (the select02-class hang) and every finite wait overshoots. Capture the exact nanosecond deadline;
        // truncating ppoll's timeout to integer milliseconds made sub-ms waits return immediately and every
        // non-integral-ms wait finish early.
        struct timespec deadline = {0, 0};
        if (have_to && ts->tv_sec >= 0) {
            hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &deadline);
            deadline.tv_sec += ts->tv_sec;
            deadline.tv_nsec += ts->tv_nsec;
            if (deadline.tv_nsec >= 1000000000L) {
                deadline.tv_sec++;
                deadline.tv_nsec -= 1000000000L;
            }
        }
        int sm_on = poll_sigmask_enter(c, have_mask, sm_set, &sm_saved);
        int r;
        for (;;) {
            r = socket_poll_error_fixup(fds, (nfds_t)a1, 0);
            if (r > 0) break;
            struct timespec rem = {0, 0};
            if (have_to && ts->tv_sec >= 0) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns > 0) {
                    rem.tv_sec = (time_t)(ns / 1000000000LL);
                    rem.tv_nsec = (long)(ns % 1000000000LL);
                }
            }
            ts_wait_enter();
#if defined(__linux__)
            // Linux already provides the exact relative-timespec primitive.
            // Hand it the complete remaining budget.  An earlier attempt to
            // wake several milliseconds early and spin to the deadline burned
            // a scheduler quantum on every call; repeated timer waits were
            // consequently preempted for about 8ms and became much less
            // accurate than the host ppoll they were trying to improve.
            r = ppoll(fds, (nfds_t)a1, have_to ? &rem : NULL, NULL);
#else
            // poll(2) only accepts milliseconds. Round UP so a finite wait
            // never returns before its Linux ppoll deadline.
            int tmo = -1;
            if (have_to) {
                int64_t ns = (int64_t)rem.tv_sec * 1000000000LL + rem.tv_nsec;
                int64_t ms = (ns + 999999LL) / 1000000LL;
                tmo = ms > 0x7fffffff ? 0x7fffffff : (int)ms;
            }
            r = poll(fds, (nfds_t)a1, tmo);
#endif
            ts_wait_leave(); // S while blocked (glibc pause on aarch64 -> ppoll)
            r = socket_poll_error_fixup(fds, (nfds_t)a1, r);
            if (r == 0 && have_to) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t left =
                    (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (left > 0) continue;
            }
            // poll/ppoll is never restarted by a handler; loop only on a spurious EINTR (svc_poll_retry).
            if (r < 0 && svc_poll_retry(c)) continue;
            break;
        }
        r = eventfd_poll_writable_fixup(fds, (nfds_t)a1, r);
        if (sm_on) poll_sigmask_leave(c, sm_saved);
        // Linux ppoll(2) writes the leftover time back into the timespec (glibc's ppoll wrapper hides it via
        // a local copy, so this is invisible to POSIX callers but correct for the raw syscall).
        if (r >= 0 && have_to) {
            struct timespec left = {0, 0};
            if (ts->tv_sec >= 0) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t ns = (int64_t)(deadline.tv_sec - now.tv_sec) * 1000000000LL + (deadline.tv_nsec - now.tv_nsec);
                if (ns > 0) {
                    left.tv_sec = (time_t)(ns / 1000000000LL);
                    left.tv_nsec = (long)(ns % 1000000000LL);
                }
            }
            *ts = left;
        }
        int copy_fault = r >= 0 && a0 && a1 && guest_copy_to(a0, fds, fds_bytes) != (ssize_t)fds_bytes;
        if (r >= 0 && have_to && guest_copy_to(a2, ts, sizeof(*ts)) != sizeof(*ts)) copy_fault = 1;
        free(fds);
        G_RET(c) = copy_fault ? (uint64_t)(int64_t)(-EFAULT) : r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // signalfd4(fd, mask, sizemask, flags)
    case 74: {
        // signalfd4(2) error surface, in Linux order (LTP signalfd02).
        // (1) EINVAL: the only valid flag bits are SFD_CLOEXEC(0x80000) and SFD_NONBLOCK(0x800).
        if ((int)a3 & ~(int)(0x80000 | 0x800)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (1b) EINVAL: the kernel requires sizemask == sizeof(sigset_t) (8 on the 64-bit ABI). A zero or
        // otherwise wrong sizemask is rejected BEFORE the mask is read (LTP signalfd4_01).
        if ((size_t)a2 != 8) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (1c) EFAULT: a non-null but inaccessible mask pointer must return EFAULT, never fault the engine.
        uint64_t lm = 0;
        if (a1 && guest_copy_from(&lm, a1, sizeof(lm)) != sizeof(lm)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) fd == -1 creates a new signalfd; otherwise it must reference an EXISTING signalfd -- EBADF if
        // it is not an open fd at all, EINVAL if it is open but not one of our signalfds (each signalfd OFD is
        // an independent self-pipe in g_sfd[], tracked by fd number in g_sigfd_slot).
        int sslot = -1;
        if ((int)a0 != -1) {
            if (fcntl((int)a0, F_GETFD) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            // The signalfd read end OR any dup of it (g_sigfd_slot) updates the SAME OFD; Linux accepts a mask
            // update on a dup'd signalfd, so resolve the fd number to its pool slot rather than the original.
            if (!((int)a0 >= 0 && (int)a0 < HL_NFD && g_sigfd_slot[(int)a0])) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            sslot = g_sigfd_slot[(int)a0] - 1;
        }
        // sigset bit (signo-1) -> g_pending bit signo
        uint64_t pm = 0;
        for (int s = 1; s < 64; s++)
            if (lm & (1ull << (s - 1))) pm |= (1ull << s);
        // Create: allocate an INDEPENDENT OFD (its own self-pipe + mask). The read end is the guest's signalfd;
        // the write end is engine-private (relocated out of the guest's low fd range, poked on delivery).
        if (sslot < 0) {
            sslot = sfd_alloc();
            if (sslot < 0) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            int fds[2];
            if (pipe(fds) < 0) {
                g_sfd[sslot].refs = 0; // release the slot
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            int wr = fcntl(fds[1], F_DUPFD, 1 << 20); // move the write end clear of the guest's low fds
            if (wr < 0) wr = fcntl(fds[1], F_DUPFD, 64);
            if (wr >= 0) {
                close(fds[1]);
                fds[1] = wr;
            }
            g_sfd[sslot].rd = fds[0];
            g_sfd[sslot].wr = fds[1];
            if (fds[0] >= 0 && fds[0] < HL_NFD) g_sigfd_slot[fds[0]] = (uint8_t)(sslot + 1);
        }
        // Linux signalfd(fd != -1, mask): UPDATE replaces this OFD's mask EXACTLY (a narrowed mask drops the
        // signals it removed). A fresh create sets the new OFD's mask. Masks never cross between OFDs.
        g_sfd[sslot].mask = pm;
        for (int s = 1; s < 64; s++)
            // make sure the host delivers them
            if ((pm & (1ull << s)) && !sig_is_sync(s) && !sig_host_is_engine_control(sig_l2m(s))) {
                struct sigaction sa;
                memset(&sa, 0, sizeof sa);
                sa.sa_handler = host_sigh;
                sa.sa_flags = SA_ONSTACK;
                sigaction(sig_l2m(s), &sa, NULL);
            }
        int srd = g_sfd[sslot].rd;
        // SFD_CLOEXEC
        if (a3 & 0x80000) fcntl(srd, F_SETFD, FD_CLOEXEC);
        // SFD_NONBLOCK
        if (a3 & 0x800) fcntl(srd, F_SETFL, O_NONBLOCK);
        // An UPDATE (fd != -1) returns the SAME fd the caller passed (Linux), including a dup alias; a fresh
        // create returns this OFD's read end.
        G_RET(c) = (int)a0 != -1 ? a0 : (uint64_t)srd;
        break;
    }
    case 85: {
        // timerfd_create(clockid, flags) -> kqueue. validate args per Linux (LTP timerfd_create01).
        // Only these clocks back a timerfd: REALTIME(0), MONOTONIC(1), BOOTTIME(7) and the ALARM pair
        // (REALTIME_ALARM=8 / BOOTTIME_ALARM=9); anything else (e.g. -1) is -EINVAL. The only valid flag
        // bits are TFD_NONBLOCK(O_NONBLOCK=0x800) and TFD_CLOEXEC(O_CLOEXEC=0x80000); any other bit
        // (e.g. flags=-1) is -EINVAL. (The old code both accepted every clock/flag AND read NONBLOCK from
        // the wrong bit -- 0x1 instead of 0x800 -- so a TFD_NONBLOCK timerfd was left blocking.)
        int clk = (int)a0;
        if (clk != 0 && clk != 1 && clk != 7 && clk != 8 && clk != 9) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if ((int)a1 & ~(int)(0x800 | 0x80000)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        int r = kqueue();
        if (r >= 0) {
            if (r < HL_NFD) {
                g_timerfd[r] = 1;
                g_epoll_family_seen = 1;
                g_tfd_clock[r] = (int)a0; // remember the clockid for TFD_TIMER_ABSTIME conversion
            }
            if (a1 & 0x800) fcntl(r, F_SETFL, O_NONBLOCK); // TFD_NONBLOCK
            // macOS kqueue() defaults FD_CLOEXEC SET; Linux timerfd_create(...,0) leaves it CLEAR. Set it
            // exactly per TFD_CLOEXEC (clearing the kqueue default otherwise) so a timerfd created without
            // the flag survives exec instead of being swept by hl's close-on-exec pass.
            fcntl(r, F_SETFD, (a1 & 0x80000) ? FD_CLOEXEC : 0);
        }
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // timerfd_settime(fd, flags, new, old)
    case 86: {
        struct kevent kv;
        uint64_t new_value[4];
        uint64_t old_value[4] = {0};
        // timerfd_settime(2) error surface, in Linux order (LTP timerfd_settime01).
        // (1) EFAULT: new_value must be a readable itimerspec (the kernel copy_from_user's it first).
        if (guest_copy_from(new_value, a2, sizeof(new_value)) != sizeof(new_value)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // (2) EINVAL: only TFD_TIMER_ABSTIME(1) and TFD_TIMER_CANCEL_ON_SET(2) are valid flag bits.
        if ((int)a1 & ~(int)(1 | 2)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        uint64_t iv_s = 0, iv_n = 0, vl_s = 0, vl_n = 0;
        memcpy(&iv_s, new_value + 0, 8);
        memcpy(&iv_n, new_value + 1, 8);
        memcpy(&vl_s, new_value + 2, 8);
        memcpy(&vl_n, new_value + 3, 8);
        // (3) EINVAL: itimerspec tv_nsec must be in [0,1e9) and tv_sec non-negative (itimerspec64_valid).
        if (iv_n >= 1000000000ull || vl_n >= 1000000000ull || (int64_t)iv_s < 0 || (int64_t)vl_s < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // (4) EBADF if fd is not an open descriptor; EINVAL if it is open but not a timerfd (e.g. a plain
        // file). Our timerfds are engine-tracked kqueues (< HL_NFD, g_timerfd set); a larger valid fd is left to
        // the best-effort path below.
        {
            int fd = (int)a0;
            if (fcntl(fd, F_GETFD) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (fd >= 0 && fd < HL_NFD && !g_timerfd[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // (5) EFAULT: a non-NULL old_value must be writable -- the kernel reports the previous setting there.
        if (a3 && guest_accessible_prefix(a3, sizeof(old_value), PROT_WRITE) != sizeof(old_value)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Report the PREVIOUS setting into old_value before re-arming (remaining it_value + it_interval),
        // mirroring timerfd_gettime's math against the stashed deadline.
        if (a3) {
            int ofd = (int)a0;
            int64_t odl = (ofd >= 0 && ofd < HL_NFD) ? g_tfd_deadline[ofd] : 0;
            int64_t oiv = (ofd >= 0 && ofd < HL_NFD) ? g_tfd_interval[ofd] : 0;
            if (odl > 0) {
                struct timespec onow;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &onow);
                int64_t onow_ns = (int64_t)onow.tv_sec * 1000000000LL + onow.tv_nsec;
                int64_t orem = odl - onow_ns;
                if (orem < 0 && oiv > 0) orem += ((-orem) / oiv + 1) * oiv;
                if (orem < 0) orem = 0;
                old_value[0] = (uint64_t)(oiv / 1000000000LL);
                old_value[1] = (uint64_t)(oiv % 1000000000LL);
                old_value[2] = (uint64_t)(orem / 1000000000LL);
                old_value[3] = (uint64_t)(orem % 1000000000LL);
            }
            if (guest_copy_to(a3, old_value, sizeof(old_value)) != sizeof(old_value)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        int64_t interval_ns = (int64_t)(iv_s * 1000000000ull + iv_n);
        int64_t value_ns = (int64_t)(vl_s * 1000000000ull + vl_n);
        // itimerspec.it_value==0 disarms (regardless of it_interval), same as Linux.
        if (value_ns <= 0) {
            EV_SET(&kv, 1, EVFILT_TIMER, EV_DELETE, 0, 0, NULL);
            kevent((int)a0, &kv, 1, NULL, 0, NULL);
            if ((int)a0 >= 0 && (int)a0 < HL_NFD) {
                g_tfd_deadline[(int)a0] = 0;
                g_tfd_interval[(int)a0] = 0;
                g_tfd_first_oneshot[(int)a0] = 0;
            }
            G_RET(c) = 0;
            break;
            // disarm
        }
        struct timespec now;
        hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
        int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
        // TFD_TIMER_ABSTIME (flags bit 1): it_value is an ABSOLUTE deadline expressed in the TIMER'S OWN
        // clock. The kqueue EVFILT_TIMER delay is always RELATIVE, so convert by subtracting "now" IN THAT
        // SAME CLOCK -- a CLOCK_REALTIME timerfd's absolute deadline is a realtime epoch value, and
        // subtracting CLOCK_MONOTONIC from it made the delay ~decades (a near-future realtime deadline never
        // fired). A past deadline fires asap (0).
        int64_t first_delay;
        if ((int)a1 & 1) {
            int clkid = ((int)a0 >= 0 && (int)a0 < HL_NFD) ? g_tfd_clock[(int)a0] : 1;
            // Linux CLOCK_REALTIME(0)/REALTIME_ALARM(8) are wall-clock; everything else is monotonic-scale.
            struct timespec tnow;
            int service_clock =
                (clkid == 0 || clkid == 8) ? HL_PRODUCTION_CLOCK_REALTIME : HL_PRODUCTION_CLOCK_MONOTONIC;
            hl_production_clock_gettime(effective_host_services(), service_clock, &tnow);
            int64_t tnow_ns = (int64_t)tnow.tv_sec * 1000000000LL + tnow.tv_nsec;
            first_delay = value_ns - tnow_ns;
        } else {
            first_delay = value_ns;
        }
        if (first_delay < 0) first_delay = 0;
        // Record the absolute next-expiry deadline + interval so timerfd_gettime can report the remaining time.
        if ((int)a0 >= 0 && (int)a0 < HL_NFD) {
            g_tfd_deadline[(int)a0] = now_ns + first_delay;
            g_tfd_interval[(int)a0] = interval_ns;
        }
        // Arm the kqueue. kqueue's EVFILT_TIMER can't express "first at it_value, then every it_interval" in
        // one entry (a recurring EV_ADD fires FIRST only after a full period). Cases:
        //   - one-shot (interval==0): EV_ONESHOT at the relative first delay.
        //   - periodic whose first delay == interval: a plain recurring EV_ADD at the interval (exact, no drift).
        //   - periodic whose first delay DIFFERS from interval: honor Linux by arming a ONE-SHOT at the first
        //     delay and flagging it_first_oneshot; the read() drain (io.c) re-arms the recurring periodic at
        //     the interval once that first tick is consumed. Without this the first expiry was wrongly delayed
        //     to a full interval (a periodic timerfd with a short it_value + long it_interval never fired early).
        int periodic = (iv_s || iv_n);
        int first_distinct = periodic && (first_delay != interval_ns);
        if ((int)a0 >= 0 && (int)a0 < HL_NFD) g_tfd_first_oneshot[(int)a0] = first_distinct ? 1 : 0;
        uint16_t fl = EV_ADD | ((periodic && !first_distinct) ? 0 : EV_ONESHOT);
        int64_t arm_ns = (periodic && !first_distinct) ? interval_ns : first_delay;
        EV_SET(&kv, 1, EVFILT_TIMER, fl, NOTE_NSECONDS | NOTE_CRITICAL, arm_ns, NULL);
        G_RET(c) = kevent((int)a0, &kv, 1, NULL, 0, NULL) < 0 ? (uint64_t)(-errno) : 0;
        break;
    }
    case 87: {
        uint64_t current_value[4] = {0};
        // timerfd_gettime(fd, curr): report the remaining time to the next expiry (it_value) and the
        // interval (it_interval), computed from the deadline timerfd_settime stashed. A disarmed timer
        // (deadline 0) reports {0,0}; an expired periodic timer reports the time to its next tick.
        // validate the fd FIRST (Linux order) -- EBADF if not open, EINVAL if open but not a timerfd,
        // and only then EFAULT on a bad curr pointer (LTP timerfd_gettime01).
        {
            int fd = (int)a0;
            if (fcntl(fd, F_GETFD) == -1) {
                G_RET(c) = (uint64_t)(-EBADF);
                break;
            }
            if (fd >= 0 && fd < HL_NFD && !g_timerfd[fd]) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        if (a1) {
            if (guest_accessible_prefix(a1, sizeof(current_value), PROT_WRITE) != sizeof(current_value)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            int fd = (int)a0;
            int64_t deadline = (fd >= 0 && fd < HL_NFD) ? g_tfd_deadline[fd] : 0;
            int64_t interval = (fd >= 0 && fd < HL_NFD) ? g_tfd_interval[fd] : 0;
            if (deadline > 0) {
                struct timespec now;
                hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_MONOTONIC, &now);
                int64_t now_ns = (int64_t)now.tv_sec * 1000000000LL + now.tv_nsec;
                int64_t rem = deadline - now_ns;
                if (rem < 0 && interval > 0) rem += ((-rem) / interval + 1) * interval; // next periodic tick
                if (rem < 0) rem = 0;
                current_value[0] = (uint64_t)(interval / 1000000000LL);
                current_value[1] = (uint64_t)(interval % 1000000000LL);
                current_value[2] = (uint64_t)(rem / 1000000000LL);
                current_value[3] = (uint64_t)(rem % 1000000000LL);
            }
            if (guest_copy_to(a1, current_value, sizeof(current_value)) != sizeof(current_value)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = 0;
        break;
    }
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
