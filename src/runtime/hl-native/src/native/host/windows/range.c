/*
 * Address-space queries on a Windows host: the two primitives src/host/range.c
 * builds hl_host_range_mapped() and hl_host_page_neighbor_mapped() out of.
 *
 * VirtualQuery is the whole implementation, and it is a better fit here than
 * the equivalent on either POSIX host. mincore(2) answers "is this page
 * RESIDENT", which is a different question that happens to correlate, and
 * mach_vm_region is a Mach message round trip. VirtualQuery is a single
 * syscall that reports the state of the region containing the address -- which
 * is the question -- and it distinguishes MEM_FREE (nothing there) from
 * MEM_RESERVE (address space claimed but no pages) from MEM_COMMIT (backed).
 *
 * Only MEM_COMMIT counts as mapped. A reserved-but-uncommitted region has an
 * address that VirtualQuery describes and that no other allocation may take,
 * but touching it faults exactly as an unmapped address does -- and this
 * predicate exists so callers can decide whether a touch is safe. Counting a
 * reservation as mapped would make the engine's lazy page grower judge a wild
 * pointer into its own reserved arena "already there" and skip the repair.
 *
 * PAGE_NOACCESS is likewise NOT mapped, matching the POSIX arms: a probe read of
 * a PROT_NONE page faults, so the layer above treats it as absent and reports
 * EFAULT, which is what Linux's copy_from_user() would have done.
 */

#include "../range.h"
#include "win32.h"

/*
 * The x86-64 and ARM64 Windows page size is 4 KiB and cannot be configured, so
 * this could be a constant. It asks the system anyway, once, because the value
 * feeds a page-walk loop in the shared range.c and a wrong constant there is a
 * silent correctness bug rather than a performance one -- and because the
 * ALLOCATION GRANULARITY on this host is 64 KiB, which is a different quantity
 * that is easy to reach for by mistake. dwPageSize is the one this predicate
 * wants: it is the granularity at which protection and commitment actually
 * change, and therefore the granularity at which "is it mapped" can differ.
 *
 * Racing initialisers write the same value, so no lock is owed.
 */
size_t hl_host_page_size(void) {
    static size_t cached;
    SYSTEM_INFO info;
    if (cached != 0) return cached;
    GetSystemInfo(&info);
    cached = (size_t)info.dwPageSize;
    return cached;
}

int hl_host_address_mapped(uintptr_t address) {
    MEMORY_BASIC_INFORMATION region;
    if (VirtualQuery((LPCVOID)address, &region, sizeof(region)) != sizeof(region)) return 0;
    if (region.State != MEM_COMMIT) return 0;
    return (region.Protect & PAGE_NOACCESS) == 0;
}

int hl_host_region_query(uintptr_t address, hl_host_region *region) {
    MEMORY_BASIC_INFORMATION info;
    uintptr_t cursor = address;
    if (region == NULL) return 0;
    /* VirtualQuery describes the region containing the address whatever its
     * state, so a free region is a step forward rather than an answer: walk
     * until a committed one is found, exactly as the contract ("containing
     * address or beginning above it") requires. The walk terminates because
     * BaseAddress + RegionSize strictly increases and the query fails once the
     * cursor leaves the user address space. */
    for (;;) {
        if (VirtualQuery((LPCVOID)cursor, &info, sizeof(info)) != sizeof(info)) return 0;
        if (info.State == MEM_COMMIT && (info.Protect & PAGE_NOACCESS) == 0) break;
        cursor = (uintptr_t)info.BaseAddress + (uintptr_t)info.RegionSize;
        if (cursor <= (uintptr_t)info.BaseAddress) return 0;
    }
    region->address = (uintptr_t)info.BaseAddress;
    region->size = (size_t)info.RegionSize;
    region->protection = 0;
    if (info.Protect & (PAGE_READONLY | PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE |
                        PAGE_EXECUTE_WRITECOPY))
        region->protection |= HL_HOST_REGION_READ;
    if (info.Protect & (PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY))
        region->protection |= HL_HOST_REGION_WRITE;
    if (info.Protect & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY))
        region->protection |= HL_HOST_REGION_EXECUTE;
    return 1;
}
