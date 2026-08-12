// hl/linux_abi -- guest mapping and Linux memory-lock state.
#include "gmap.h"

#include <errno.h>
#include <stdlib.h>
#if !defined(_WIN32)
#include <sys/mman.h>
#endif

#define MLK_N 1024
#define HL_GUEST_RLIMIT_MEMLOCK 8
#define HL_GMAP_INITIAL_CAPACITY 256u

struct hl_gmap_lock_range {
    uint64_t low;
    uint64_t high;
};

struct hl_gmap_context {
    hl_gmap_entry *mappings;
    size_t mapping_count;
    size_t mapping_capacity;
    struct hl_gmap_lock_range locks[MLK_N];
    size_t lock_count;
    int lock_all;
    int lock_future;
    const hl_limit_table *limits;
    const hl_host_services *host;
};

#define HL_EXEC_LOADER_SLOTS 32u
#define HL_EXEC_MAPPING_CAPACITY (16384u + HL_EXEC_LOADER_SLOTS)

typedef struct hl_exec_mapping {
    uint64_t address;
    uint64_t length;
    hl_host_handle handle;
} hl_exec_mapping;

static hl_exec_mapping g_exec_mappings[HL_EXEC_MAPPING_CAPACITY];
static size_t g_exec_mapping_count;

/* Teardown for a registry range the engine holds NO ownership handle for. Everything this registry mapped
 * itself is released through the seam -- discard() and release() above and below -- and only two populations
 * reach here: a provider mapping the provider placed at a fixed address and a loader range whose handle a
 * later MAP_FIXED already retired. The seam cannot express their release, because unmap_range() is keyed on
 * the owning handle and by construction there is none. An address-keyed teardown operation on
 * hl_host_memory_services is what would retire this, and that is an ABI decision for the seam and its four
 * backends, not something this file can settle.
 *
 * Windows never populates either case, because both of these arrive from paths the Windows host does not
 * run, so there is no range here to release and no reachable behaviour to preserve. */
static void gmap_release_unowned(uint64_t address, uint64_t length) {
#if defined(_WIN32)
    (void)address;
    (void)length;
#else
    (void)munmap((void *)(uintptr_t)address, (size_t)length);
#endif
}

/* Guest mlock/mlockall wiring, applied to real host pages so /proc residency is honest rather than merely
 * tracked. The seam has no wiring operation at all, on any host: mlock(2) is reached directly here.
 *
 * Windows spells the nearest thing VirtualLock, but that grows the process WORKING SET rather than pinning a
 * page against reclaim, and the two differ in whose limit applies and what a refusal means -- so it belongs
 * behind a host backend that can state which it implements, not behind a name substituted here. A refusal is
 * reported as one, and the caller's contract already covers it: mlockall wires best-effort and a range the
 * host declines is left pageable while the call still succeeds, which is how an exhausted RLIMIT_MEMLOCK
 * already behaves on Linux. The guest's lock STATE is registry bookkeeping and stays exact either way. */
static int gmap_wire(uint64_t address, uint64_t length) {
#if defined(_WIN32)
    (void)address;
    (void)length;
    return -1;
#else
    return mlock((void *)(uintptr_t)address, (size_t)length);
#endif
}

static void gmap_unwire(uint64_t address, uint64_t length) {
#if defined(_WIN32)
    (void)address;
    (void)length;
#else
    (void)munlock((void *)(uintptr_t)address, (size_t)length);
#endif
}

static struct hl_gmap_context g_gmap;

void hl_gmap_bind_limits(const hl_limit_table *limits) {
    g_gmap.limits = limits;
}

void hl_gmap_bind_host(const hl_host_services *host) {
    g_gmap.host = host;
}

hl_status hl_gmap_map_anonymous(uint64_t requested_address, uint64_t length, uint32_t protection, uint32_t flags,
                                uint64_t *address) {
    hl_host_memory_mapping mapping = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(mapping), 0, 0, 0, 0};
    hl_host_result mapped;
    if (address == NULL || length == 0 || g_gmap.host == NULL || g_gmap.host->memory == NULL ||
        g_gmap.host->memory->map_anonymous == NULL || g_gmap.host->memory->release == NULL)
        return HL_STATUS_INVALID_ARGUMENT;
    mapped = g_gmap.host->memory->map_anonymous(g_gmap.host->context, requested_address, length, protection, flags,
                                                &mapping);
    if (mapped.status != HL_STATUS_OK) return (hl_status)mapped.status;
    if (mapping.handle == HL_HOST_HANDLE_INVALID || mapping.address == 0 || mapping.mapped_size != length ||
        hl_exec_mapping_add(mapping.address, length, mapping.handle) != 0) {
        if (mapping.handle != HL_HOST_HANDLE_INVALID)
            (void)g_gmap.host->memory->release(g_gmap.host->context, mapping.handle);
        return HL_STATUS_PLATFORM_FAILURE;
    }
    hl_gmap_add(mapping.address, length);
    *address = mapping.address;
    return HL_STATUS_OK;
}

size_t hl_gmap_count(void) {
    return g_gmap.mapping_count;
}

int hl_gmap_get(size_t index, hl_gmap_entry *entry) {
    if (!entry || index >= g_gmap.mapping_count) return 0;
    *entry = g_gmap.mappings[index];
    return 1;
}

static void hl_gmap_reserve_one(void) {
    size_t capacity;
    hl_gmap_entry *mappings;
    if (g_gmap.mapping_count < g_gmap.mapping_capacity) return;
    capacity = g_gmap.mapping_capacity != 0 ? g_gmap.mapping_capacity * 2 : HL_GMAP_INITIAL_CAPACITY;
    if (capacity < g_gmap.mapping_capacity || capacity > SIZE_MAX / sizeof(*mappings)) abort();
    mappings = realloc(g_gmap.mappings, capacity * sizeof(*mappings));
    if (mappings == NULL) abort();
    g_gmap.mappings = mappings;
    g_gmap.mapping_capacity = capacity;
}

static void hl_gmap_split_range(uint64_t start, uint64_t end);

void hl_gmap_add(uint64_t address, uint64_t length) {
    hl_gmap_add_physical(address, length, address, length);
}

void hl_gmap_add_physical(uint64_t address, uint64_t length, uint64_t physical_address, uint64_t physical_length) {
    hl_gmap_entry *entry;
    if (!address || !length || address > UINT64_MAX - length || !physical_address || !physical_length ||
        physical_address > UINT64_MAX - physical_length || physical_address > address ||
        address + length > physical_address + physical_length)
        return;
    hl_gmap_split_range(address, address + length);
    hl_gmap_reserve_one();
    entry = &g_gmap.mappings[g_gmap.mapping_count++];
    entry->address = address;
    entry->length = length;
    entry->guest_length = length;
    entry->physical_address = physical_address;
    entry->physical_length = physical_length;
}

void hl_gmap_set_guest_length(uint64_t address, uint64_t guest_length) {
    // Callers set the guest length immediately after hl_gmap_add appends the mapping, so the target is
    // the most recently added entry. Scan from the tail: O(1) for that append pattern, avoiding an O(n)
    // front-to-back scan on every mmap (which made an N-mapping guest O(n^2)). Behavior is identical --
    // a live mapping's start address is unique, so tail-first finds the same entry a front scan would.
    for (size_t index = g_gmap.mapping_count; index-- > 0;)
        if (g_gmap.mappings[index].address == address) {
            g_gmap.mappings[index].guest_length = guest_length;
            return;
        }
}

void hl_gmap_remove(uint64_t address) {
    for (size_t index = 0; index < g_gmap.mapping_count; index++)
        if (g_gmap.mappings[index].address == address) {
            hl_exec_mapping_discard_range(address, g_gmap.mappings[index].length);
            g_gmap.mappings[index] = g_gmap.mappings[--g_gmap.mapping_count];
            return;
        }
}

uint64_t hl_gmap_find_length(uint64_t address) {
    for (size_t index = 0; index < g_gmap.mapping_count; index++)
        if (g_gmap.mappings[index].address == address) return g_gmap.mappings[index].length;
    return 0;
}

uint64_t hl_gmap_find_guest_length(uint64_t address) {
    for (size_t index = 0; index < g_gmap.mapping_count; index++)
        if (g_gmap.mappings[index].address == address) return g_gmap.mappings[index].guest_length;
    return 0;
}

int hl_gmap_find_physical(uint64_t address, uint64_t *physical_address, uint64_t *physical_length) {
    if (physical_address == NULL || physical_length == NULL) return 0;
    for (size_t index = 0; index < g_gmap.mapping_count; index++)
        if (g_gmap.mappings[index].address == address) {
            *physical_address = g_gmap.mappings[index].physical_address;
            *physical_length = g_gmap.mappings[index].physical_length;
            return 1;
        }
    return 0;
}

/* Is every page of [address, address+length) mapped?  The UNION of the entries answers this, not any single
   one: mprotect, mlock, mremap and mincore all operate across adjacent VMAs on Linux, and the registry
   splits an entry whenever a MAP_FIXED or a partial munmap divides it -- so a single-entry test rejected a
   range the guest had mapped in two pieces (mincore over a region MAP_FIXED'd in the middle returned
   ENOMEM).  Strictly wider than the old test, so nothing a single entry covered stops being covered. */
int hl_gmap_contains(uint64_t address, uint64_t length) {
    uint64_t end = address + length, at = address;
    if (end < address) return 0;
    for (int extended = 1; at < end && extended;) {
        extended = 0;
        for (size_t index = 0; index < g_gmap.mapping_count; index++) {
            const hl_gmap_entry *entry = &g_gmap.mappings[index];
            if (entry->address <= at && at < entry->address + entry->length) {
                at = entry->address + entry->length;
                extended = 1;
                break;
            }
        }
    }
    return at >= end;
}

/* MAP_FIXED replaces whatever the range held, so the registry entries it overlapped must be split the way
   the kernel splits the VMAs -- otherwise the superseded reservation stays whole and /proc/[pid]/maps emits
   overlapping rows (ld.so's whole-span reservation plus every segment it MAP_FIXED inside it), violating the
   ascending non-overlapping invariant the file's readers assume.  Ledger only: the host mapping was replaced
   in place by the caller's own mmap, so the exec-mapping handles must NOT be discarded here. */
void hl_gmap_supersede_range(uint64_t start, uint64_t end) {
    if (end <= start) return;
    hl_gmap_split_range(start, end);
}

void hl_gmap_unmap_range(uint64_t start, uint64_t end) {
    if (end > start) hl_exec_mapping_discard_range(start, end - start);
    hl_gmap_split_range(start, end);
}

static void hl_gmap_split_range(uint64_t start, uint64_t end) {
    for (size_t index = 0; index < g_gmap.mapping_count;) {
        hl_gmap_entry *entry = &g_gmap.mappings[index];
        uint64_t base = entry->address;
        uint64_t mapped_end = base + entry->length;
        uint64_t guest_end = base + (entry->guest_length < entry->length ? entry->guest_length : entry->length);
        uint64_t physical_start = entry->physical_address;
        uint64_t physical_end = physical_start + entry->physical_length;
        if (end <= base || start >= mapped_end) {
            index++;
            continue;
        }
        int keep_head = base < start;
        int keep_tail = end < mapped_end;
        if (!keep_head && !keep_tail) {
            *entry = g_gmap.mappings[--g_gmap.mapping_count];
            continue;
        }
        if (keep_head) {
            uint64_t head_length = start - base;
            entry->length = head_length;
            entry->guest_length = guest_end > base ? guest_end - base : 0;
            if (entry->guest_length > head_length) entry->guest_length = head_length;
            entry->physical_address = physical_start;
            entry->physical_length = start - physical_start;
        } else {
            uint64_t tail_length = mapped_end - end;
            entry->address = end;
            entry->length = tail_length;
            entry->guest_length = guest_end > end ? guest_end - end : 0;
            if (entry->guest_length > tail_length) entry->guest_length = tail_length;
            entry->physical_address = end;
            entry->physical_length = physical_end - end;
        }
        if (keep_head && keep_tail) {
            uint64_t tail_length = mapped_end - end;
            uint64_t tail_guest_length = guest_end > end ? guest_end - end : 0;
            if (tail_guest_length > tail_length) tail_guest_length = tail_length;
            hl_gmap_add_physical(end, tail_length, end, physical_end - end);
            hl_gmap_set_guest_length(end, tail_guest_length);
        }
        index++;
    }
}

static int hl_exec_mapping_owns(uint64_t address, uint64_t length) {
    if (length == 0 || address > UINT64_MAX - length) return 0;
    for (size_t index = 0; index < g_exec_mapping_count; ++index) {
        const hl_exec_mapping *entry = &g_exec_mappings[index];
        if (entry->handle != HL_HOST_HANDLE_INVALID && address >= entry->address &&
            address + length <= entry->address + entry->length)
            return 1;
    }
    return 0;
}

void hl_gmap_reset(void) {
    for (size_t index = 0; index < g_gmap.mapping_count; index++) {
        const hl_gmap_entry *entry = &g_gmap.mappings[index];
        if (!hl_exec_mapping_owns(entry->address, entry->length))
            gmap_release_unowned(entry->physical_address, entry->physical_length);
    }
    hl_exec_mapping_reset();
    g_gmap.mapping_count = 0;
}

int hl_exec_mapping_add(uint64_t address, uint64_t length, hl_host_handle host_mapping) {
    if (!address || !length || address > UINT64_MAX - length || host_mapping == HL_HOST_HANDLE_INVALID ||
        g_exec_mapping_count >= HL_EXEC_MAPPING_CAPACITY)
        return -1;
    uint64_t end = address + length;
    for (size_t index = 0; index < g_exec_mapping_count; ++index) {
        uint64_t old_end = g_exec_mappings[index].address + g_exec_mappings[index].length;
        if (address < old_end && g_exec_mappings[index].address < end) return -1;
    }
    g_exec_mappings[g_exec_mapping_count++] = (hl_exec_mapping){address, length, host_mapping};
    return 0;
}

void hl_exec_mapping_discard_range(uint64_t address, uint64_t length) {
    if (!length || address > UINT64_MAX - length) return;
    uint64_t end = address + length;
    for (size_t index = 0; index < g_exec_mapping_count;) {
        hl_exec_mapping *entry = &g_exec_mappings[index];
        if (end <= entry->address || address >= entry->address + entry->length) {
            ++index;
            continue;
        }
        uint64_t old_address = entry->address;
        uint64_t old_end = old_address + entry->length;
        if (entry->handle != HL_HOST_HANDLE_INVALID && g_gmap.host != NULL && g_gmap.host->memory != NULL &&
            g_gmap.host->memory->discard != NULL) {
            hl_host_result discarded = g_gmap.host->memory->discard(g_gmap.host->context, entry->handle);
            /* INVALID_ARGUMENT means a successful MAP_FIXED provider operation
             * already retired the overlapped handle. Any other failure leaves
             * dangerous live ownership and is an internal contract breach. */
            if (discarded.status != HL_STATUS_OK && discarded.status != HL_STATUS_INVALID_ARGUMENT) abort();
        }
        entry->handle = HL_HOST_HANDLE_INVALID;
        int keep_head = old_address < address;
        int keep_tail = end < old_end;
        if (!keep_head && !keep_tail) {
            *entry = g_exec_mappings[--g_exec_mapping_count];
            continue;
        }
        if (keep_head) {
            entry->length = address - old_address;
        } else {
            entry->address = end;
            entry->length = old_end - end;
        }
        if (keep_head && keep_tail) {
            /* Every split corresponds to a live guest VMA split and therefore
             * cannot exceed the same fixed capacity as the VMA registry. */
            if (g_exec_mapping_count >= HL_EXEC_MAPPING_CAPACITY) abort();
            g_exec_mappings[g_exec_mapping_count++] = (hl_exec_mapping){end, old_end - end, HL_HOST_HANDLE_INVALID};
        }
        ++index;
    }
}

void hl_exec_mapping_reset(void) {
    for (size_t index = 0; index < g_exec_mapping_count; ++index) {
        hl_exec_mapping *entry = &g_exec_mappings[index];
        if (entry->handle != HL_HOST_HANDLE_INVALID) {
            if (g_gmap.host != NULL && g_gmap.host->memory != NULL && g_gmap.host->memory->release != NULL)
                (void)g_gmap.host->memory->release(g_gmap.host->context, entry->handle);
        } else {
            gmap_release_unowned(entry->address, entry->length);
        }
        entry->handle = HL_HOST_HANDLE_INVALID;
    }
    g_exec_mapping_count = 0;
}

void hl_gmap_lock_remove(uint64_t address, uint64_t length) {
    if (!length || !g_gmap.lock_count) return;
    uint64_t remove_low = address & ~UINT64_C(0xfff);
    uint64_t remove_high = (address + length + UINT64_C(0xfff)) & ~UINT64_C(0xfff);
    for (size_t index = 0; index < g_gmap.lock_count;) {
        uint64_t low = g_gmap.locks[index].low;
        uint64_t high = g_gmap.locks[index].high;
        if (remove_high <= low || remove_low >= high) {
            index++;
            continue;
        }
        int keep_head = low < remove_low;
        int keep_tail = remove_high < high;
        if (!keep_head && !keep_tail) {
            g_gmap.locks[index] = g_gmap.locks[--g_gmap.lock_count];
            continue;
        }
        if (keep_head)
            g_gmap.locks[index].high = remove_low;
        else
            g_gmap.locks[index].low = remove_high;
        if (keep_head && keep_tail && g_gmap.lock_count < MLK_N) {
            g_gmap.locks[g_gmap.lock_count].low = remove_high;
            g_gmap.locks[g_gmap.lock_count].high = high;
            g_gmap.lock_count++;
        }
        index++;
    }
}

void hl_gmap_lock_add(uint64_t address, uint64_t length) {
    if (!length) return;
    uint64_t low = address & ~UINT64_C(0xfff);
    uint64_t high = (address + length + UINT64_C(0xfff)) & ~UINT64_C(0xfff);
    if (high <= low) return;
    hl_gmap_lock_remove(low, high - low);
    if (g_gmap.lock_count >= MLK_N) return;
    g_gmap.locks[g_gmap.lock_count].low = low;
    g_gmap.locks[g_gmap.lock_count].high = high;
    g_gmap.lock_count++;
}

void hl_gmap_lock_reset(void) {
    g_gmap.lock_count = 0;
    g_gmap.lock_all = 0;
    g_gmap.lock_future = 0;
}

int hl_gmap_lock_wire_current(void) {
    int failed = 0;
    for (size_t index = 0; index < g_gmap.mapping_count; index++) {
        hl_gmap_entry *entry = &g_gmap.mappings[index];
        if (!entry->address || !entry->length) continue;
        if (gmap_wire(entry->address, entry->length) != 0) failed++;
    }
    return failed;
}

void hl_gmap_lock_unwire_all(void) {
    for (size_t index = 0; index < g_gmap.mapping_count; index++) {
        hl_gmap_entry *entry = &g_gmap.mappings[index];
        if (!entry->address || !entry->length) continue;
        gmap_unwire(entry->address, entry->length);
    }
}

uint64_t hl_gmap_lock_region_bytes(uint64_t low, uint64_t high) {
    if (high <= low) return 0;
    if (g_gmap.lock_all) return high - low;
    uint64_t sum = 0;
    for (size_t index = 0; index < g_gmap.lock_count; index++) {
        uint64_t start = g_gmap.locks[index].low > low ? g_gmap.locks[index].low : low;
        uint64_t end = g_gmap.locks[index].high < high ? g_gmap.locks[index].high : high;
        if (end > start) sum += end - start;
    }
    return sum;
}

uint64_t hl_gmap_lock_total_bytes(void) {
    uint64_t sum = 0;
    if (g_gmap.lock_all) {
        for (size_t index = 0; index < g_gmap.mapping_count; index++)
            sum += g_gmap.mappings[index].guest_length;
        return sum;
    }
    for (size_t index = 0; index < g_gmap.lock_count; index++)
        sum += g_gmap.locks[index].high - g_gmap.locks[index].low;
    return sum;
}

static uint64_t hl_gmap_memlock_limit(void) {
    uint64_t current = UINT64_MAX;
    if (g_gmap.limits) hl_limit_table_get(g_gmap.limits, HL_GUEST_RLIMIT_MEMLOCK, &current, NULL);
    return current;
}

static uint64_t hl_gmap_locked_bytes(void) {
    uint64_t sum = 0;
    for (size_t index = 0; index < g_gmap.lock_count; index++)
        sum += g_gmap.locks[index].high - g_gmap.locks[index].low;
    return sum;
}

static uint64_t hl_gmap_uncounted_bytes(uint64_t address, uint64_t length) {
    if (!length) return 0;
    uint64_t low = address & ~UINT64_C(0xfff);
    uint64_t high = (address + length + UINT64_C(0xfff)) & ~UINT64_C(0xfff);
    if (high <= low) return 0;
    uint64_t already = 0;
    for (size_t index = 0; index < g_gmap.lock_count; index++) {
        uint64_t start = g_gmap.locks[index].low > low ? g_gmap.locks[index].low : low;
        uint64_t end = g_gmap.locks[index].high < high ? g_gmap.locks[index].high : high;
        if (end > start) already += end - start;
    }
    return (high - low) - already;
}

int hl_gmap_lock_limit_range(uint64_t address, uint64_t length) {
    uint64_t limit = hl_gmap_memlock_limit();
    if (limit == UINT64_MAX) return 0;
    if (!limit) return -EPERM;
    if (!length) return 0;
    if (hl_gmap_locked_bytes() + hl_gmap_uncounted_bytes(address, length) > limit) return -ENOMEM;
    return 0;
}

int hl_gmap_lock_limit_all(void) {
    uint64_t limit = hl_gmap_memlock_limit();
    if (limit == UINT64_MAX) return 0;
    if (!limit) return -EPERM;
    uint64_t total = 0;
    for (size_t index = 0; index < g_gmap.mapping_count; index++)
        total += g_gmap.mappings[index].guest_length;
    return total > limit ? -ENOMEM : 0;
}

int hl_gmap_lock_future(void) {
    return g_gmap.lock_future;
}

void hl_gmap_lock_all(int future) {
    if (future) g_gmap.lock_future = 1;
    g_gmap.lock_all = 1;
}
