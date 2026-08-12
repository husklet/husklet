/*
 * The engine-private descriptor band on a Windows host.
 *
 * On the POSIX hosts this group solves a problem created by fd number reuse:
 * the engine opens host descriptors of its own -- a code-cache file, a
 * forkserver socket, a directory watch -- and a guest that calls dup2(x, 7)
 * would clobber them if they sat in the low range the guest gets to name. So
 * the engine hoists its own descriptors above HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM
 * with F_DUPFD, records which numbers are its own, and derives the
 * guest-visible RLIMIT_NOFILE below that band.
 *
 * Both mechanisms it rests on are absent here:
 *
 *   - F_DUPFD. The whole fcntl(2) command space has no Windows counterpart --
 *     the CRT has no per-descriptor metadata channel at all -- so there is no
 *     way to say "duplicate this to the lowest free number at or above N". _dup
 *     picks the lowest free number, full stop.
 *   - RLIMIT_NOFILE. There is no per-process descriptor rlimit to read. The CRT
 *     has a fixed table (measured: 8192 entries on this runtime), which is a
 *     constant, not a limit a caller can query or raise.
 *
 * So the tracking calls are REFUSALS. They are safe refusals rather than
 * dangerous ones because of what the band is FOR: failing to hoist a descriptor
 * means the engine's own descriptors sit where a guest could name them, which
 * is a collision hazard, not a correctness claim -- and every caller already
 * handles the hoist failing, because it can fail on POSIX too when the table is
 * full.
 *
 * hl_engine_guest_fd_limit() is the one entry point that must NOT refuse, and
 * it is REAL: it is the guest-visible RLIMIT_NOFILE that /proc/self/limits and
 * getrlimit(2) report, and returning zero would tell a guest it may open no
 * files at all. The value is the same golden-stable number the POSIX hosts
 * produce -- HL_LINUX_FD_LIMIT capped -- because it is a property of the guest
 * ABI, not of the host: goldens must match on every runner. It is stated here
 * as a constant rather than derived, since the quantity it would be derived
 * from does not exist on this host.
 */

#include "../system.h"
/* The public declaration of hl_engine_guest_fd_limit, which is defined below.
 * Included so the definition is checked against the prototype callers see. */
#include "hl/engine.h"

#include <errno.h>
#include <stdint.h>

/* Matches the ceiling the Linux engine reports (linux_abi's HL_LINUX_FD_LIMIT).
 * Spelled here rather than included because src/host must not depend on the
 * Linux ABI layer; the value is pinned by the goldens either way. */
#define HL_WINDOWS_GUEST_FD_LIMIT UINT32_C(65536)

void hl_host_private_init(void) {
}

uint32_t hl_engine_guest_fd_limit(void) {
    return HL_WINDOWS_GUEST_FD_LIMIT;
}

/* REFUSAL. -1 is "no private band", which every caller reads as "do not hoist"
 * rather than as descriptor -1. */
int hl_host_process_fd_private_floor(void) {
    return -1;
}

int hl_host_process_fd_private_add(int descriptor) {
    (void)descriptor;
    return -1;
}

/* REFUSAL, and deliberately one that does NOT take ownership: the contract says
 * a failure leaves the input open and returns a negative errno, so the caller
 * keeps using the descriptor it already had, unhoisted. Closing it here would
 * turn a missing optimisation into a lost descriptor. */
int hl_host_process_fd_private_adopt(int descriptor) {
    (void)descriptor;
    return -ENOSYS;
}

void hl_host_process_fd_private_remove(int descriptor) {
    (void)descriptor;
}

/* Nothing was ever recorded as private, so nothing is. Answering yes would make
 * a caller refuse to hand a perfectly ordinary descriptor to a guest. */
int hl_host_process_fd_private_is(int64_t pid, uint64_t start_ns, int descriptor) {
    (void)pid;
    (void)start_ns;
    (void)descriptor;
    return 0;
}

int hl_host_process_fd_private_current(int descriptor) {
    (void)descriptor;
    return 0;
}

/* The fork hooks re-seat the private band across a fork. There is no fork on
 * this host, so there is nothing to re-seat and reporting success is the honest
 * answer -- unlike the calls above, this one is not withholding anything. */
int hl_host_process_fd_private_fork_prepare(void) {
    return 0;
}

int hl_host_process_fd_private_fork_complete(int child) {
    (void)child;
    return 0;
}

void hl_host_process_fd_private_cleanup(void) {
}
