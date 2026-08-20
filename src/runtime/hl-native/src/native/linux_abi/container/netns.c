// hl/linux_abi/container -- network namespace implementation assembled from cohesive capability fragments.
/* WHY THIS INCLUDE IS NO LONGER GUARDED.
 *
 * netns/loopback.c's identity-ticket arena is POSIX-shaped in three ways at once:
 * geteuid() ownership validation, flock() exclusion around first-time sizing, and a
 * MAP_SHARED mmap of a file in the engine-owned identity directory. This line used to
 * be wrapped in #if !defined(_WIN32) so a Windows build would fail loudly on flock
 * rather than quietly adopt LockFileEx byte-range semantics for a mechanism 8ee364a5c
 * hardened against a guest-forgeable identity. That guard was standing in for a
 * decision nobody had taken.
 *
 * The decision is now taken, in sock_identity_directory(): a host with no euid cannot
 * host the engine-private identity namespace, so Windows refuses the capability whole
 * and the callers fail closed on the NULL that refusal already produced. Because that
 * gate is structural and sits before sock_identity_ticket_arena_attach() opens
 * anything, the flock() call site below is unreachable on Windows -- nothing there
 * adopts LockFileEx semantics for the identity table, because nothing there reaches
 * the identity table.
 *
 * So the guard has no job left, and keeping it would leave a compile error standing in
 * for an answered question. toolchain/msvc-posix/include/sys/file.h carries the
 * declaration and toolchain/msvc-posix/compatibility.c the definition (whole-range
 * LockFileEx/UnlockFileEx); linux_abi/host_fd.h already defines LOCK_SH/EX/NB/UN with
 * the same values on this host, so the include adds the declaration and nothing else.
 */
#include <sys/file.h> // flock: serializes first-time sizing of the shared identity-ticket table

#include "netns/unix_compat.c"
#include "netns/ancillary.c"
#include "netns/loopback.c"
#include "netns/bridge_udp.c"
#include "netns/address.c"
#include "netns/netlink.c"
#include "netns/services.c"
