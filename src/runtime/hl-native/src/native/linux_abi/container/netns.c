// hl/linux_abi/container -- network namespace implementation assembled from cohesive capability fragments.
/* WHY THIS INCLUDE IS GUARDED AND WHY WINDOWS IS STILL RED BEHIND IT.
 *
 * netns/loopback.c's identity-ticket arena is POSIX-shaped in three ways at once:
 * geteuid() ownership validation, flock() exclusion around first-time sizing, and a
 * MAP_SHARED mmap of a file in the engine-owned identity directory. Only the middle
 * one has a Windows answer -- toolchain/msvc-posix implements flock() over a
 * whole-range LockFileEx, see the header for what that does and does not preserve.
 * geteuid() has no declaration and no implementation for this target, so the
 * x86_64-pc-windows-msvc build fails to compile loopback.c at the ownership checks
 * (loopback.c:462 and :509) whatever happens to this line.
 *
 * Dropping the guard would therefore trade a compile error naming flock for none at
 * all while leaving the geteuid errors, AND would silently adopt LockFileEx semantics
 * for a security-relevant mechanism 8ee364a5c deliberately hardened. Deciding what
 * "the euid that owns this identity table" means on a host with no euid is a design
 * decision for the netns/identity owner, not a portability edit, so the guard stays
 * and the Windows job stays honestly red until that owner answers it.
 */
#if !defined(_WIN32)
#include <sys/file.h> // flock: serializes first-time sizing of the shared identity-ticket table
#endif

#include "netns/unix_compat.c"
#include "netns/ancillary.c"
#include "netns/loopback.c"
#include "netns/bridge_udp.c"
#include "netns/address.c"
#include "netns/netlink.c"
#include "netns/services.c"
