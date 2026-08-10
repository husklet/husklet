#ifndef HL_LINUX_ABI_PAGE_H
#define HL_LINUX_ABI_PAGE_H

#include <stddef.h>
#if !defined(_WIN32)
#include <unistd.h> // getpagesize
#endif

/*
 * Linux guest ABI page size.  This is deliberately independent of the host
 * VM allocation granularity: Apple Silicon hosts map in 16 KiB units, while
 * the Linux ABI presented by both supported guest ISAs uses 4 KiB pages.
 * Host-facing mmap reconciliation must use hl_linux_host_map_granularity().
 */
#define HL_LINUX_GUEST_PAGE_SIZE 4096u

/*
 * The quantum at which the HOST will honour a fixed placement, or release a
 * subrange without disturbing what lies beside it.
 *
 * Every caller in this layer wants exactly one property, and it is not "page
 * size".  It is: given a guest request that is 4 KiB-granular, what is the
 * coarsest unit the host actually operates on, so that the partial head/tail
 * of the request can be left alone rather than rounded out over a live
 * neighbour?  syscall/mem.c's MAP_FIXED interior remap, its munmap
 * hole-carving, its MADV_DONTNEED edge handling and its mincore stride are all
 * built on that one question, and getting it wrong does not fail loudly -- it
 * unmaps or zeroes memory belonging to an object the guest still holds.
 *
 * On Linux and macOS the answer is the page size, so this is getpagesize() and
 * nothing changes (16 KiB on Apple Silicon, which is why the distinction was
 * already load-bearing before Windows existed).
 *
 * On Windows the two quantities genuinely differ and only one of them is the
 * right answer.  The page size is 4096 -- it is what VirtualProtect and commit
 * operate on.  The ALLOCATION GRANULARITY is 65536, and that is the unit that
 * governs where a reservation or a mapped view may begin, and therefore the
 * unit at which a fixed placement can be honoured and a subrange retired
 * without touching a neighbour.  Answering 4096 here would let this layer
 * believe it can place a mapping at, or carve a hole on, a 4 KiB boundary that
 * the host will refuse or will round out -- which is precisely the silent
 * placement corruption the edge-preserving code above exists to prevent.
 *
 * The 65536 is a constant here rather than a query, and that is a known
 * shortcoming rather than a claim.  It is what SYSTEM_INFO.dwAllocationGranularity
 * has reported on every Windows ABI this engine targets, but this layer should
 * not be asserting a host VM property at all: the host services seam should
 * carry it, next to the memory group's other placement rules, and this
 * definition should collapse into a call once it does.  Reading it here via
 * GetSystemInfo would mean pulling the Win32 headers into the Linux ABI layer,
 * whose guest-ABI vocabulary they collide with, and would still be the wrong
 * layer asking.
 */
static inline size_t hl_linux_host_map_granularity(void) {
#if defined(_WIN32)
    return 65536u;
#else
    return (size_t)getpagesize();
#endif
}

/*
 * The host's PAGE size -- the unit its own accounting is denominated in.
 *
 * Distinct from the granularity above, and the distinction is the whole reason
 * both exist.  This one answers "when the host reports a resident-set or
 * virtual size, what is a page?", which is what the /proc/[pid]/stat and
 * /proc/[pid]/statm synthesis needs to turn a byte count back into the page
 * count Linux publishes there, and what F_SETPIPE_SZ rounds against.  Nothing
 * about placement depends on it, so getting it wrong is a wrong number rather
 * than corrupted memory -- which is exactly why it must not be spelled the same
 * as the granularity and reached for by accident.
 *
 * On Linux and macOS it is sysconf(_SC_PAGESIZE), unchanged.  On Windows it is
 * 4096, which is the page size on every ABI this engine targets and is NOT the
 * 65536 allocation granularity above.
 */
static inline size_t hl_linux_host_page_size(void) {
#if defined(_WIN32)
    return 4096u;
#else
    long value = sysconf(_SC_PAGESIZE);
    return value > 0 ? (size_t)value : (size_t)HL_LINUX_GUEST_PAGE_SIZE;
#endif
}

#endif
