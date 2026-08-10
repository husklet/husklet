#ifndef HL_LINUX_ABI_HOST_POLL_H
#define HL_LINUX_ABI_HOST_POLL_H

/*
 * <poll.h> (and the select vocabulary that travels with it) for this layer.
 * Same construction and the same REAL/SHAPE/REFUSAL labelling as host_mman.h.
 *
 * Windows is the one host where the readiness question is genuinely different
 * rather than merely unbound, and the shape of the answer below follows from
 * that difference rather than working around it:
 *
 *   - WSAPoll exists and takes a pollfd-shaped array, but it accepts SOCKETs
 *     only.  Every other waitable thing on Windows -- a file, a pipe, a console,
 *     a timer, an event -- is a HANDLE, and no single call waits on a mixed set.
 *     poll(2) on this layer's descriptors is a mixed set by construction: the
 *     Linux ABI hands the same fd namespace to files, pipes, eventfds, timerfds,
 *     signalfds and sockets alike.
 *
 *   - The seam's answer to readiness is the event group, and it is a REGISTERED
 *     set (control/wait) rather than a per-call array.  That is a good fit for
 *     epoll, which is how src/linux_abi/epoll.c already consumes it, and a poor
 *     one for poll(2), whose whole shape is "here is a fresh array, tell me
 *     about it once".  Synthesizing poll on top of it would mean registering and
 *     tearing down a pollset per call.
 *
 * WHAT REPLACED THE REFUSAL, and why it is not an approximation.  The premise of
 * the third bullet this note used to carry -- "a pollfd's fd is a guest number
 * that names no host object on Windows" -- is simply false, and host_fd.h says
 * so at length from the other side: the UCRT has a real 8192-entry descriptor
 * table, the descriptors this layer hands the guest ARE those descriptors, and
 * _get_osfhandle() turns any one of them into the HANDLE underneath.  So the
 * missing piece was never the binding.  It was that Windows publishes readiness
 * per OBJECT KIND rather than through one call, and GetFileType() names the kind:
 *
 *     FILE_TYPE_DISK  a regular file.  Linux reports a regular file readable and
 *                     writable ALWAYS -- never blocking, never at EOF-as-HUP --
 *                     so this is a fact and not a guess.
 *     FILE_TYPE_CHAR  a console or the null device.  Same answer, same reason.
 *     FILE_TYPE_PIPE  the interesting one, and the only one this layer's own
 *                     pipe(), eventfd and fifo emulation ever produces.
 *                     PeekNamedPipe() reports the bytes buffered without
 *                     consuming them, which is exactly POLLIN's question, and
 *                     its FAILURE distinguishes the two ends of the pipe:
 *                     ERROR_BROKEN_PIPE means the last writer is gone and the
 *                     buffer is drained (POLLHUP), ERROR_ACCESS_DENIED means the
 *                     handle is the WRITE end, which the call cannot peek.
 *
 * Three residuals, named rather than hidden, because each is a real difference
 * from Linux and a caller may be relying on it:
 *
 *   (1) A pipe WRITE end always reports POLLOUT.  Windows publishes no free-space
 *       query for a pipe -- GetNamedPipeInfo reports the buffer SIZE, not the
 *       fill -- and PeekNamedPipe refuses a write handle outright (measured:
 *       ERROR_ACCESS_DENIED, both before and after the read end closes).  So a
 *       full pipe is reported writable here where Linux would report it not.
 *       That is the honest direction to be wrong in: a caller that writes anyway
 *       blocks in write(2), which is what a blocking descriptor is for, whereas
 *       claiming NOT writable would strand a writer that could have proceeded.
 *
 *   (2) A read end whose writer has closed but whose buffer still holds bytes
 *       reports POLLIN alone, where Linux reports POLLIN|POLLHUP together.
 *       PeekNamedPipe keeps succeeding until the buffer drains (measured), so
 *       the hang-up is observable exactly one poll later -- after the reader has
 *       consumed what was already there, which is the only order in which a
 *       reader could act on it anyway.
 *
 *   (3) The wait is a bounded-backoff sample loop, not a kernel-side block.
 *       There is no Windows primitive that blocks on "a pipe became readable",
 *       and the alternatives are worse: a thread per descriptor issuing an
 *       overlapped zero-byte read costs a thread per poll() and changes the
 *       descriptor's own I/O mode, and the seam's pollset is keyed on
 *       hl_host_handle, which is the registered-set mismatch above.  The loop
 *       therefore samples, then sleeps 0,0,1,1,2,4,8,10,10... milliseconds, so a
 *       ready descriptor is seen immediately and an idle wait costs one wakeup
 *       per 10ms rather than a spin.
 *
 * What this must NOT be, and still is not, is a return of 0 ("timed out, nothing
 * ready") or a return of nfds with POLLIN set ("everything ready") for a
 * descriptor whose readiness is unknown: the first turns every guest event loop
 * into a spin at the timeout period and the second into a spin at full speed,
 * and both look like a hang rather than an unimplemented call.  An object kind
 * this file cannot answer for -- FILE_TYPE_UNKNOWN, which is what a Winsock
 * SOCKET reports -- is POLLNVAL, the one answer that names the descriptor as the
 * thing that could not be polled.  Nothing in this layer can produce such a
 * descriptor today: host_socket.h refuses socket() and socketpair() outright.
 */

#if !defined(_WIN32)

#include <poll.h>

#else /* Windows */

#include <errno.h>
#include <io.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/time.h>
#include <time.h>

/* SHAPE.  Linux values -- this layer translates guest poll bits to host poll
 * bits by identity in several places, exactly as it does for mmap protections. */
#define POLLIN 0x001
#define POLLPRI 0x002
#define POLLOUT 0x004
#define POLLERR 0x008
#define POLLHUP 0x010
#define POLLNVAL 0x020
#define POLLRDNORM 0x040
#define POLLRDBAND 0x080
#define POLLWRNORM 0x100
#define POLLWRBAND 0x200
#define POLLRDHUP 0x2000

typedef unsigned long nfds_t;

struct pollfd {
    int fd;
    short events;
    short revents;
};

/* SHAPE.  select(2)'s descriptor set.  Sized to this layer's descriptor bound
 * rather than to a host FD_SETSIZE, because the numbers in it are guest
 * descriptors; a host-sized 64-entry winsock fd_set would silently truncate. */
#ifndef FD_SETSIZE
#define FD_SETSIZE 1024
#endif

typedef struct {
    unsigned long fds_bits[(FD_SETSIZE + (8 * sizeof(unsigned long)) - 1) / (8 * sizeof(unsigned long))];
} fd_set;

#define HL_LINUX_FD_WORD(d) ((unsigned)(d) / (8 * sizeof(unsigned long)))
#define HL_LINUX_FD_BIT(d) (1UL << ((unsigned)(d) % (8 * sizeof(unsigned long))))

#define FD_ZERO(set) memset((set), 0, sizeof(fd_set))
#define FD_SET(d, set) ((set)->fds_bits[HL_LINUX_FD_WORD(d)] |= HL_LINUX_FD_BIT(d))
#define FD_CLR(d, set) ((set)->fds_bits[HL_LINUX_FD_WORD(d)] &= ~HL_LINUX_FD_BIT(d))
#define FD_ISSET(d, set) (((set)->fds_bits[HL_LINUX_FD_WORD(d)] & HL_LINUX_FD_BIT(d)) != 0)

/* Win32 entry points declared by hand rather than by including <windows.h>, for
 * the reason native_compat.h states where it does the same: this header reaches
 * the guest-target unity TU, and <windows.h> would drop macros named ERROR, IN,
 * OUT, min and max -- plus a second file-status vocabulary -- into the unit that
 * defines the guest ABI.  Only the five entry points the bodies below call are
 * named.  Skipped when <windows.h> did get included first, so the declarations
 * can never drift from the real ones. */
#ifndef _WINDOWS_
__declspec(dllimport) unsigned long __stdcall GetLastError(void);
__declspec(dllimport) unsigned long __stdcall GetFileType(void *object);
__declspec(dllimport) int __stdcall PeekNamedPipe(void *pipe, void *buffer, unsigned long capacity,
                                                  unsigned long *copied, unsigned long *available,
                                                  unsigned long *message_remaining);
__declspec(dllimport) unsigned long long __stdcall GetTickCount64(void);
__declspec(dllimport) void __stdcall Sleep(unsigned long milliseconds);
#endif

/* GetFileType's answers.  Spelled out rather than taken from <winbase.h> for the
 * same reason as the declarations above. */
#define HL_LINUX_POLL_TYPE_UNKNOWN 0x0000ul
#define HL_LINUX_POLL_TYPE_DISK 0x0001ul
#define HL_LINUX_POLL_TYPE_CHAR 0x0002ul
#define HL_LINUX_POLL_TYPE_PIPE 0x0003ul

#define HL_LINUX_POLL_ACCESS_DENIED 5ul
#define HL_LINUX_POLL_BROKEN_PIPE 109ul

/* Longest the sample loop sleeps between scans; see residual (3) in the header
 * note.  Reached by doubling from zero, so a descriptor that is already ready
 * costs no sleep at all and a short wait keeps sub-millisecond resolution. */
#define HL_LINUX_POLL_BACKOFF_MAX_MS 10ul

/*
 * The current readiness of ONE descriptor, as the union of POLL* bits that are
 * true of it right now -- not masked by any caller's interest, which is done by
 * the caller.  Returns -1 for a descriptor that names no object at all, which is
 * POLLNVAL and is distinct from "names an object with no readiness".
 */
static inline int hl_linux_poll_state(int descriptor) {
    void *object;
    unsigned long type;
    unsigned long available = 0;
    if (descriptor < 0) return -1;
    object = (void *)(intptr_t)_get_osfhandle(descriptor);
    if (object == (void *)(intptr_t)-1 || object == NULL) return -1;
    type = GetFileType(object);
    switch (type) {
    /* A regular file and a character device are always ready both ways on
       Linux; neither can block and neither reports hang-up. */
    case HL_LINUX_POLL_TYPE_DISK:
    case HL_LINUX_POLL_TYPE_CHAR: return POLLIN | POLLOUT;
    case HL_LINUX_POLL_TYPE_PIPE:
        if (PeekNamedPipe(object, NULL, 0ul, NULL, &available, NULL)) return available > 0ul ? POLLIN : 0;
        switch (GetLastError()) {
        /* The last writer is gone AND the buffer is drained: residual (2). */
        case HL_LINUX_POLL_BROKEN_PIPE: return POLLHUP;
        /* Peek refuses a write handle, which is how the write end is
           identified at all.  Reported writable: residual (1). */
        case HL_LINUX_POLL_ACCESS_DENIED: return POLLOUT;
        default: return POLLERR;
        }
    /* FILE_TYPE_UNKNOWN, which is what a Winsock SOCKET reports.  POLLNVAL
       names the descriptor as the thing that could not be polled rather than
       inventing a readiness for it -- see the header note. */
    default: return -1;
    }
}

/* One pass over the caller's array.  Fills every revents and returns the number
 * of entries with a non-zero one, which is poll(2)'s return value.  POLLERR,
 * POLLHUP and POLLNVAL are delivered whether or not the caller asked for them,
 * exactly as poll(2) specifies; POLLIN and POLLOUT are masked by the request,
 * with the RDNORM/WRNORM spellings echoed alongside when they were asked for. */
static inline int hl_linux_poll_scan(struct pollfd *entries, nfds_t count) {
    nfds_t index;
    int ready = 0;
    for (index = 0; index < count; ++index) {
        struct pollfd *entry = &entries[index];
        int state;
        short revents = 0;
        entry->revents = 0;
        /* A negative fd is ignored and reports nothing -- not an error. */
        if (entry->fd < 0) continue;
        state = hl_linux_poll_state(entry->fd);
        if (state < 0) {
            entry->revents = POLLNVAL;
            ++ready;
            continue;
        }
        revents = (short)(state & (POLLERR | POLLHUP));
        if ((state & POLLIN) && (entry->events & (POLLIN | POLLRDNORM))) {
            revents |= (short)(entry->events & (POLLIN | POLLRDNORM));
            if (!(entry->events & POLLIN)) revents |= POLLRDNORM;
        }
        if ((state & POLLOUT) && (entry->events & (POLLOUT | POLLWRNORM))) {
            revents |= (short)(entry->events & (POLLOUT | POLLWRNORM));
            if (!(entry->events & POLLOUT)) revents |= POLLWRNORM;
        }
        entry->revents = revents;
        if (revents != 0) ++ready;
    }
    return ready;
}

/*
 * The shared wait every entry point below funnels into.  `timeout_ns` uses
 * poll(2)'s own convention rather than a host one: negative is infinite, zero is
 * a single non-blocking scan, positive is a budget in nanoseconds.  Returns the
 * ready count, or 0 on timeout; it never fails, because a scan cannot.
 */
static inline int hl_linux_poll_wait(struct pollfd *entries, nfds_t count, int64_t timeout_ns) {
    unsigned long long start = GetTickCount64();
    unsigned long backoff = 0ul;
    for (;;) {
        int ready = hl_linux_poll_scan(entries, count);
        if (ready > 0) return ready;
        if (timeout_ns == 0) return 0;
        if (timeout_ns > 0) {
            /* Milliseconds is the finest granularity the tick counter and Sleep
               both offer, so a sub-millisecond budget becomes "scan once more,
               then give up" rather than a busy wait to a deadline neither the
               clock nor the sleep can resolve. */
            unsigned long long elapsed_ms = GetTickCount64() - start;
            unsigned long long budget_ms = (unsigned long long)(timeout_ns / 1000000);
            if (elapsed_ms >= budget_ms) return 0;
        }
        Sleep(backoff);
        if (backoff == 0ul)
            backoff = 1ul;
        else if (backoff < HL_LINUX_POLL_BACKOFF_MAX_MS)
            backoff *= 2ul;
        if (backoff > HL_LINUX_POLL_BACKOFF_MAX_MS) backoff = HL_LINUX_POLL_BACKOFF_MAX_MS;
    }
}

/* REAL.  poll(2)'s int-millisecond timeout, widened to the shared nanosecond
 * budget.  A count of zero with a finite timeout is a sleep, which is a real use
 * of poll(2) and works here for free. */
static inline int poll(struct pollfd *entries, nfds_t count, int timeout_ms) {
    if (entries == NULL && count != 0) {
        errno = EFAULT;
        return -1;
    }
    return hl_linux_poll_wait(entries, count, timeout_ms < 0 ? -1 : (int64_t)timeout_ms * 1000000);
}

/* REAL.  `mask` is deliberately ignored and that is not a shortcut: the one
 * caller in this layer (syscall/event.c) installs the guest's temporary signal
 * mask into its own cpu state around the wait, because hl's signal delivery is
 * the engine's and not the host's.  A host sigmask here would mask host signals,
 * which is not the question the guest asked. */
static inline int ppoll(struct pollfd *entries, nfds_t count, const struct timespec *timeout, const void *mask) {
    int64_t budget = -1;
    (void)mask;
    if (entries == NULL && count != 0) {
        errno = EFAULT;
        return -1;
    }
    if (timeout != NULL) {
        if (timeout->tv_sec < 0 || timeout->tv_nsec < 0 || timeout->tv_nsec >= 1000000000L) {
            errno = EINVAL;
            return -1;
        }
        budget = (int64_t)timeout->tv_sec * 1000000000 + (int64_t)timeout->tv_nsec;
    }
    return hl_linux_poll_wait(entries, count, budget);
}

/*
 * REAL.  select(2) over the same scan, by projecting the three sets onto one
 * pollfd array and projecting the answers back.
 *
 * The arrangement is worth one line because it is the reverse of the usual one:
 * on a host with a native select this would be the primitive and poll the
 * wrapper.  Here poll is the primitive because the underlying question --
 * GetFileType plus PeekNamedPipe -- is per descriptor, and a per-descriptor
 * answer is what a pollfd array is.  The exception set is always answered empty:
 * this layer's descriptors are files and pipes, and neither has out-of-band data
 * or any other condition select(2) reports there.
 */
static inline int hl_linux_pselect_ns(int bound, fd_set *readable, fd_set *writable, fd_set *failing,
                                      int64_t timeout_ns) {
    struct pollfd entries[FD_SETSIZE];
    nfds_t count = 0;
    int descriptor;
    int ready;
    nfds_t index;
    if (bound < 0 || bound > FD_SETSIZE) {
        errno = EINVAL;
        return -1;
    }
    for (descriptor = 0; descriptor < bound; ++descriptor) {
        short events = 0;
        if (readable != NULL && FD_ISSET(descriptor, readable)) events |= POLLIN;
        if (writable != NULL && FD_ISSET(descriptor, writable)) events |= POLLOUT;
        if (failing != NULL && FD_ISSET(descriptor, failing)) events |= POLLPRI;
        if (events == 0) continue;
        entries[count].fd = descriptor;
        entries[count].events = events;
        entries[count].revents = 0;
        ++count;
    }
    if (readable != NULL) FD_ZERO(readable);
    if (writable != NULL) FD_ZERO(writable);
    if (failing != NULL) FD_ZERO(failing);
    (void)hl_linux_poll_wait(entries, count, timeout_ns);
    /* select(2) counts BITS, not descriptors: one descriptor ready both ways
       contributes two.  A descriptor poll reported invalid is EBADF for the
       whole call, which is select(2)'s answer where poll(2)'s is POLLNVAL. */
    ready = 0;
    for (index = 0; index < count; ++index) {
        short revents = entries[index].revents;
        if (revents & POLLNVAL) {
            errno = EBADF;
            return -1;
        }
        if (readable != NULL && (revents & (POLLIN | POLLHUP | POLLERR))) {
            FD_SET(entries[index].fd, readable);
            ++ready;
        }
        if (writable != NULL && (revents & (POLLOUT | POLLERR))) {
            FD_SET(entries[index].fd, writable);
            ++ready;
        }
    }
    return ready;
}

static inline int select(int bound, fd_set *readable, fd_set *writable, fd_set *failing, struct timeval *timeout) {
    int64_t budget = -1;
    if (timeout != NULL) {
        if (timeout->tv_sec < 0 || timeout->tv_usec < 0 || timeout->tv_usec >= 1000000L) {
            errno = EINVAL;
            return -1;
        }
        budget = (int64_t)timeout->tv_sec * 1000000000 + (int64_t)timeout->tv_usec * 1000;
    }
    return hl_linux_pselect_ns(bound, readable, writable, failing, budget);
}

static inline int pselect(int bound, fd_set *readable, fd_set *writable, fd_set *failing,
                          const struct timespec *timeout, const void *mask) {
    int64_t budget = -1;
    (void)mask; /* engine-owned, exactly as in ppoll above. */
    if (timeout != NULL) {
        if (timeout->tv_sec < 0 || timeout->tv_nsec < 0 || timeout->tv_nsec >= 1000000000L) {
            errno = EINVAL;
            return -1;
        }
        budget = (int64_t)timeout->tv_sec * 1000000000 + (int64_t)timeout->tv_nsec;
    }
    return hl_linux_pselect_ns(bound, readable, writable, failing, budget);
}

#endif /* _WIN32 */

#endif
