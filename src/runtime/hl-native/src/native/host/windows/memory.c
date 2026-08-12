/*
 * hl_host_memory_services on Win32 placeholder reservations.
 *
 * The whole group rests on the Win10 1803 placeholder API. Without it neither
 * unmap_range nor map_file-at-a-fixed-address is implementable at all:
 * VirtualFree(MEM_RELEASE) frees an entire original reservation and
 * UnmapViewOfFile unmaps an entire view, whereas munmap carves a hole out of
 * the middle. With it, a reservation is explicitly splittable
 * (MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) and a view retires back into a
 * placeholder (UnmapViewOfFile2), which is exactly the mmap/munmap model.
 *
 * Two Windows facts shape every routine below, and both were measured on this
 * branch rather than assumed:
 *
 *   1. A placeholder-replacing call needs the EXACT bounds of one placeholder.
 *      Hence hl_windows_region: one record per NT allocation, so the backend can
 *      always name those bounds. Splitting a placeholder that still holds a live
 *      view is refused, so the order is always unmap, split, re-map.
 *   2. VirtualAlloc2 rounds a requested base address DOWN to the 64 KiB
 *      allocation granularity, silently, exactly as plain VirtualAlloc does.
 *      A guest fixed-address request is 4 KiB-granular, so it cannot be reserved
 *      directly -- see hl_windows_reserve_placeholder.
 */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

/* Windows has no write-without-read page protection. Neither does x86-64 paging,
 * so widening WRITE to READ|WRITE is unobservable here -- but it is a widening,
 * and it is recorded rather than left implicit. */
static DWORD hl_windows_page_protection(uint32_t flags) {
    switch (flags & (uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) {
    case 0: return PAGE_NOACCESS;
    case HL_HOST_MEMORY_READ: return PAGE_READONLY;
    case HL_HOST_MEMORY_WRITE:
    case HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE: return PAGE_READWRITE;
    case HL_HOST_MEMORY_EXECUTE:
    case HL_HOST_MEMORY_READ | HL_HOST_MEMORY_EXECUTE: return PAGE_EXECUTE_READ;
    default: return PAGE_EXECUTE_READWRITE;
    }
}

static uint64_t hl_windows_round_up(uint64_t value, uint64_t granularity) {
    return (value + granularity - 1u) & ~(granularity - 1u);
}

static uint64_t hl_windows_round_down(uint64_t value, uint64_t granularity) {
    return value & ~(granularity - 1u);
}

static int hl_windows_protection_valid(uint32_t protection) {
    return (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) == 0;
}

/* --- placeholder primitives ----------------------------------------------- */

/* Split [base, base+size) out of the placeholder or committed region containing
 * it and hand the address space back. Two calls because MEM_PRESERVE_PLACEHOLDER
 * only performs the split; the resulting placeholder is its own allocation and
 * is then released on its own base with a zero size. */
static int hl_windows_split_release(void *base, uint64_t size) {
    if (!VirtualFree(base, (SIZE_T)size, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER)) return 0;
    return VirtualFree(base, 0, MEM_RELEASE) != 0;
}

/*
 * Reserve exactly [address, address+size) as a placeholder, or anywhere when
 * address is NULL.
 *
 * The fixed-address path is not a straight VirtualAlloc2 call because
 * VirtualAlloc2 rounds a requested base down to the 64 KiB allocation
 * granularity without saying so -- measured, and not something the host map
 * anticipated. Reserving the enclosing granule and splitting the head and tail
 * back off gets the exact bounds using only measured placeholder operations.
 *
 * Residual gap, deliberate and documented: when a neighbouring mapping already
 * owns part of the same 64 KiB granule the enclosing reservation fails and so
 * does this call, where Linux would have placed the mapping. Two guest mappings
 * that share a granule and are created independently are the case that loses.
 */
static void *hl_windows_reserve_placeholder(const hl_windows_kernelbase *api, void *address, uint64_t size,
                                            uint64_t alignment) {
    MEM_ADDRESS_REQUIREMENTS requirements;
    MEM_EXTENDED_PARAMETER parameter;
    char *low;
    char *high;
    void *reserved;
    if (address == NULL) {
        if (alignment <= HL_WINDOWS_ALLOCATION_GRANULARITY)
            return api->virtual_alloc2(NULL, NULL, (SIZE_T)size, MEM_RESERVE | MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS,
                                       NULL, 0);
        /* MEM_ADDRESS_REQUIREMENTS.Alignment gives arbitrary power-of-two
         * alignment in the kernel, retiring the over-reserve-and-trim dance the
         * Linux backend needs for the same job. */
        memset(&requirements, 0, sizeof(requirements));
        memset(&parameter, 0, sizeof(parameter));
        requirements.Alignment = (SIZE_T)alignment;
        parameter.Type = MemExtendedParameterAddressRequirements;
        parameter.Pointer = &requirements;
        return api->virtual_alloc2(NULL, NULL, (SIZE_T)size, MEM_RESERVE | MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS,
                                   &parameter, 1);
    }
    low = (char *)(uintptr_t)hl_windows_round_down((uint64_t)(uintptr_t)address, HL_WINDOWS_ALLOCATION_GRANULARITY);
    high =
        (char *)(uintptr_t)hl_windows_round_up((uint64_t)(uintptr_t)address + size, HL_WINDOWS_ALLOCATION_GRANULARITY);
    reserved = api->virtual_alloc2(NULL, low, (SIZE_T)(high - low), MEM_RESERVE | MEM_RESERVE_PLACEHOLDER,
                                   PAGE_NOACCESS, NULL, 0);
    if (reserved != low) {
        if (reserved != NULL) (void)VirtualFree(reserved, 0, MEM_RELEASE);
        return NULL;
    }
    if ((char *)address > low && !hl_windows_split_release(low, (uint64_t)((char *)address - low))) {
        (void)VirtualFree(low, 0, MEM_RELEASE);
        return NULL;
    }
    if (high > (char *)address + size &&
        !hl_windows_split_release((char *)address + size, (uint64_t)(high - (char *)address) - size)) {
        (void)VirtualFree(address, 0, MEM_RELEASE);
        return NULL;
    }
    return address;
}

/* --- region bookkeeping ---------------------------------------------------- */

static hl_status hl_windows_regions_grow(hl_windows_handle_entry *entry, uint32_t needed) {
    hl_windows_region *grown;
    uint32_t capacity = entry->region_capacity == 0 ? 4u : entry->region_capacity;
    if (needed <= entry->region_capacity) return HL_STATUS_OK;
    while (capacity < needed) {
        if (capacity > UINT32_MAX / 2u) return HL_STATUS_RESOURCE_LIMIT;
        capacity *= 2u;
    }
    grown = realloc(entry->regions, (size_t)capacity * sizeof(*grown));
    if (grown == NULL) return HL_STATUS_OUT_OF_MEMORY;
    entry->regions = grown;
    entry->region_capacity = capacity;
    return HL_STATUS_OK;
}

static hl_status hl_windows_regions_insert(hl_windows_handle_entry *entry, uint32_t index,
                                           const hl_windows_region *region) {
    hl_status status = hl_windows_regions_grow(entry, entry->region_count + 1u);
    if (status != HL_STATUS_OK) return status;
    memmove(&entry->regions[index + 1u], &entry->regions[index],
            (size_t)(entry->region_count - index) * sizeof(*entry->regions));
    entry->regions[index] = *region;
    entry->region_count++;
    return HL_STATUS_OK;
}

static void hl_windows_regions_remove(hl_windows_handle_entry *entry, uint32_t index) {
    memmove(&entry->regions[index], &entry->regions[index + 1u],
            (size_t)(entry->region_count - index - 1u) * sizeof(*entry->regions));
    entry->region_count--;
}

/* Every range of the mapping accounted for by exactly one region record? */
static int hl_windows_regions_cover(const hl_windows_handle_entry *entry, uint64_t offset, uint64_t size) {
    uint64_t cursor = offset;
    uint32_t index;
    for (index = 0; index < entry->region_count && cursor < offset + size; ++index) {
        const hl_windows_region *region = &entry->regions[index];
        if (region->offset > cursor) return 0;
        if (region->offset + region->size > cursor) cursor = region->offset + region->size;
    }
    return cursor >= offset + size;
}

/* --- carving --------------------------------------------------------------- */

static hl_status hl_windows_map_region_view(hl_host_windows *host, hl_windows_handle_entry *entry,
                                            const hl_windows_region *region) {
    void *base = (char *)entry->address + region->offset;
    void *mapped = host->api.map_view_of_file3(entry->section, GetCurrentProcess(), base, region->section_offset,
                                               (SIZE_T)region->size, MEM_REPLACE_PLACEHOLDER,
                                               hl_windows_page_protection(region->protection), NULL, 0);
    return mapped == base ? HL_STATUS_OK : hl_windows_status_from_error(GetLastError());
}

/*
 * Remove [offset, offset+size) from a mapping and return that address space to
 * the operating system. This is munmap.
 *
 * A region wholly inside the range is retired outright. A region only partly
 * inside must be taken apart in the one order NT permits: retire the whole
 * region first (a placeholder holding a live view cannot be split), split the
 * dead middle out, then restore the surviving head and tail. For private
 * committed pages the split is itself the hole punch and the surviving data is
 * untouched; for a view the surviving parts are re-mapped at their own section
 * offsets.
 */
static hl_status hl_windows_entry_carve(hl_host_windows *host, hl_windows_handle_entry *entry, uint64_t offset,
                                        uint64_t size) {
    char *base = entry->address;
    const uint64_t high = offset + size;
    hl_status status = HL_STATUS_OK;
    uint32_t index = 0;
    while (index < entry->region_count) {
        hl_windows_region *region = &entry->regions[index];
        const uint64_t region_end = region->offset + region->size;
        const uint64_t clip_low = region->offset > offset ? region->offset : offset;
        const uint64_t clip_high = region_end < high ? region_end : high;
        hl_windows_region head;
        hl_windows_region tail;
        if (clip_low >= clip_high) {
            ++index;
            continue;
        }
        if (clip_low == region->offset && clip_high == region_end) {
            if (region->state == HL_WINDOWS_REGION_VIEW) {
                if (!UnmapViewOfFile(base + region->offset)) return hl_windows_status_from_error(GetLastError());
            } else if (!VirtualFree(base + region->offset, 0, MEM_RELEASE)) {
                return hl_windows_status_from_error(GetLastError());
            }
            hl_windows_regions_remove(entry, index);
            continue;
        }
        head = *region;
        head.size = clip_low - region->offset;
        tail = *region;
        tail.offset = clip_high;
        tail.size = region_end - clip_high;
        tail.section_offset = region->section_offset + (clip_high - region->offset);
        if (region->state == HL_WINDOWS_REGION_VIEW &&
            !host->api.unmap_view_of_file2(GetCurrentProcess(), base + region->offset, MEM_PRESERVE_PLACEHOLDER))
            return hl_windows_status_from_error(GetLastError());
        if (!hl_windows_split_release(base + clip_low, clip_high - clip_low))
            return hl_windows_status_from_error(GetLastError());
        if (head.size != 0) {
            *region = head;
            if (head.state == HL_WINDOWS_REGION_VIEW) status = hl_windows_map_region_view(host, entry, &head);
            ++index;
        }
        if (tail.size != 0) {
            if (head.size == 0) {
                *region = tail;
            } else if (hl_windows_regions_insert(entry, index, &tail) != HL_STATUS_OK) {
                /* The address space is correct either way; only the record of the
                 * tail is lost, so report it rather than pretending success. */
                return HL_STATUS_OUT_OF_MEMORY;
            }
            if (tail.state == HL_WINDOWS_REGION_VIEW && status == HL_STATUS_OK)
                status = hl_windows_map_region_view(host, entry, &entry->regions[index]);
            ++index;
        }
        if (status != HL_STATUS_OK) return status;
    }
    return HL_STATUS_OK;
}

/* Release every native resource an entry owns. Called with the host lock held. */
static hl_status hl_windows_entry_teardown(hl_host_windows *host, hl_windows_handle_entry *entry) {
    hl_status status = hl_windows_entry_carve(host, entry, 0, entry->size);
    if (entry->executable_address != NULL && entry->executable_address != entry->address) {
        if (!UnmapViewOfFile(entry->executable_address) && status == HL_STATUS_OK)
            status = hl_windows_status_from_error(GetLastError());
        entry->executable_address = NULL;
    }
    if (entry->section != NULL && entry->section_owned) CloseHandle(entry->section);
    entry->section = NULL;
    entry->section_file = NULL;
    entry->section_owned = 0;
    free(entry->regions);
    entry->regions = NULL;
    entry->region_count = 0;
    entry->region_capacity = 0;
    return status;
}

void hl_windows_memory_destroy_entry(hl_host_windows *host, hl_windows_handle_entry *entry) {
    (void)hl_windows_entry_teardown(host, entry);
    hl_windows_clear_entry_locked(entry);
}

/*
 * mmap(MAP_FIXED) atomically replaces whatever occupies the range. Windows
 * cannot: the address space has to be vacated before it can be re-claimed, so
 * this retires the engine's own overlapping mappings first and leaves a window
 * in which the range is unmapped. A concurrent guest thread touching it faults.
 * That is a genuine, unrecoverable semantic loss and it is written here rather
 * than left to be discovered. Foreign (non-engine) occupants are not disturbed
 * and the subsequent reservation simply fails.
 */
static void hl_windows_release_overlapping_locked(hl_host_windows *host, hl_host_handle exclude, uintptr_t low,
                                                  uintptr_t high) {
    uint32_t index;
    for (index = 0; index < host->handle_capacity; ++index) {
        hl_windows_handle_entry *entry = &host->handles[index];
        uintptr_t entry_low;
        uintptr_t entry_high;
        if (entry->kind != HL_WINDOWS_HANDLE_MAPPING || entry->address == NULL) continue;
        if (hl_windows_encode_handle(index, entry->generation) == exclude) continue;
        entry_low = (uintptr_t)entry->address;
        entry_high = entry_low + (uintptr_t)entry->size;
        if (low >= entry_high || entry_low >= high) continue;
        (void)hl_windows_entry_carve(
            host, entry, low > entry_low ? (uint64_t)(low - entry_low) : 0,
            (uint64_t)((high < entry_high ? high : entry_high) - (low > entry_low ? low : entry_low)));
        if (entry->region_count == 0 && entry->executable_address == NULL) {
            if (entry->section != NULL && entry->section_owned) CloseHandle(entry->section);
            free(entry->regions);
            hl_windows_clear_entry_locked(entry);
        }
    }
}

/* --- anonymous memory ------------------------------------------------------ */

/*
 * Claim [address, address+size) (or anywhere, when address is 0) and back it
 * with either private committed pages or a pagefile section view. Shared by
 * reserve() and map_anonymous(); the only difference between them is that
 * reserve() never places at a fixed address.
 *
 * The extent is rounded up to whole pages before any Win32 call, because the
 * placeholder pair disagrees with itself about a partial one. Measured:
 * MEM_RESERVE_PLACEHOLDER silently rounds a 0x10D40 reservation up to an
 * 0x11000 NT region and reports success, while the MEM_REPLACE_PLACEHOLDER that
 * has to consume it insists on covering the placeholder exactly and answers
 * ERROR_INVALID_PARAMETER for the 0x10D40 the caller asked for. The
 * fixed-address arm fails one step earlier and just as hard: splitting the tail
 * off the enclosing granule at an unaligned bound is the same refusal.
 *
 * So an unrounded length made every anonymous mapping whose size was not a
 * multiple of the page size fail outright -- which is most of them, since a
 * caller allocating a struct or a TLS block asks for its exact size. mmap(2)
 * rounds the length up to a page and callers rely on that everywhere, so the
 * rounding belongs here rather than in each of them. The extra pages are real
 * and owned, so the entry records the rounded extent; only what is REPORTED
 * back stays the caller's own length, which is what mmap(2) does and what the
 * guest-map registry checks against.
 */
static hl_host_result hl_windows_claim_anonymous(hl_host_windows *host, hl_host_handle handle, uint64_t address,
                                                 uint64_t size, uint64_t alignment, uint32_t protection,
                                                 uint32_t placement, uint32_t sharing, void **out_address) {
    hl_windows_handle_entry *entry;
    hl_windows_region region;
    void *placeholder;
    void *claimed;
    HANDLE section = NULL;
    uint64_t extent = hl_windows_round_up(size, HL_WINDOWS_PAGE_SIZE);
    if (extent < size || extent > SIZE_MAX || (address != 0 && extent > (uint64_t)UINTPTR_MAX - address))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_lock(host);
    if (placement == HL_HOST_MEMORY_FIXED)
        hl_windows_release_overlapping_locked(host, handle, (uintptr_t)address, (uintptr_t)(address + extent));
    placeholder = hl_windows_reserve_placeholder(&host->api, (void *)(uintptr_t)address, extent, alignment);
    if (placeholder == NULL) {
        hl_host_result failure = hl_windows_last_error_result();
        hl_windows_unlock(host);
        return failure;
    }
    if (sharing == HL_HOST_MEMORY_SHARED) {
        /* A pagefile-backed section is the only anonymous memory two threads can
         * share by address on Windows. Maximum protection is PAGE_EXECUTE_READWRITE
         * because it is fixed at creation and a later protect() to RX would
         * otherwise fail. The section is created and fully mapped in this one
         * call, so the eager SEC_COMMIT charge that punishes unmapped sections
         * does not arise. */
        section = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_EXECUTE_READWRITE, (DWORD)(extent >> 32),
                                     (DWORD)(extent & UINT64_C(0xFFFFFFFF)), NULL);
        if (section == NULL) {
            hl_host_result failure = hl_windows_last_error_result();
            (void)VirtualFree(placeholder, 0, MEM_RELEASE);
            hl_windows_unlock(host);
            return failure;
        }
        claimed = host->api.map_view_of_file3(section, GetCurrentProcess(), placeholder, 0, (SIZE_T)extent,
                                              MEM_REPLACE_PLACEHOLDER, hl_windows_page_protection(protection), NULL, 0);
    } else {
        /* MEM_COMMIT pages read as zero, which is what the contract promises and
         * what hl_linux_memory_create relies on instead of memset()ing. */
        claimed = host->api.virtual_alloc2(NULL, placeholder, (SIZE_T)extent,
                                           MEM_RESERVE | MEM_COMMIT | MEM_REPLACE_PLACEHOLDER,
                                           hl_windows_page_protection(protection), NULL, 0);
    }
    if (claimed != placeholder) {
        hl_host_result failure = hl_windows_last_error_result();
        if (section != NULL) CloseHandle(section);
        (void)VirtualFree(placeholder, 0, MEM_RELEASE);
        hl_windows_unlock(host);
        return failure;
    }
    entry = hl_windows_lookup_locked(host, handle, HL_WINDOWS_HANDLE_MAPPING);
    region.offset = 0;
    region.size = extent;
    region.section_offset = 0;
    region.state = section != NULL ? (uint32_t)HL_WINDOWS_REGION_VIEW : (uint32_t)HL_WINDOWS_REGION_COMMIT;
    region.protection = protection;
    if (entry == NULL || hl_windows_regions_insert(entry, 0, &region) != HL_STATUS_OK) {
        if (section != NULL) {
            (void)UnmapViewOfFile(claimed);
            CloseHandle(section);
        } else {
            (void)VirtualFree(claimed, 0, MEM_RELEASE);
        }
        hl_windows_unlock(host);
        return hl_windows_result(entry == NULL ? HL_STATUS_INVALID_ARGUMENT : HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    entry->address = claimed;
    entry->size = extent;
    entry->section = section;
    entry->section_owned = section != NULL ? 1u : 0u;
    hl_windows_unlock(host);
    *out_address = claimed;
    return hl_windows_result(HL_STATUS_OK, handle, 0);
}

static hl_host_result hl_windows_memory_reserve(void *context, uint64_t size, uint64_t alignment, uint32_t flags) {
    hl_host_windows *host = context;
    hl_host_result registered;
    hl_host_result claimed;
    void *address = NULL;
    /* Windows over-delivers on alignment: every reservation base is 64 KiB
     * aligned, so the Linux backend's `alignment > page` rejection is kept only
     * to hold the contract's shape, not because the request is unsatisfiable. */
    if (size == 0 || size > SIZE_MAX || alignment > HL_WINDOWS_PAGE_SIZE || !hl_windows_protection_valid(flags))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_MAPPING);
    if (registered.status != HL_STATUS_OK) return registered;
    /* Commit charge is taken eagerly against the pagefile here, unlike Linux
     * overcommit: a large reservation Linux would hand out unconditionally can
     * fail on Windows. */
    claimed =
        hl_windows_claim_anonymous(host, registered.value, 0, size, 0, flags, 0, HL_HOST_MEMORY_PRIVATE, &address);
    if (claimed.status != HL_STATUS_OK) {
        hl_windows_lock(host);
        {
            hl_windows_handle_entry *entry =
                hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_MAPPING);
            if (entry != NULL) hl_windows_clear_entry_locked(entry);
        }
        hl_windows_unlock(host);
    }
    return claimed;
}

static hl_host_result hl_windows_memory_map_anonymous(void *context, uint64_t requested_address, uint64_t size,
                                                      uint32_t protection, uint32_t flags,
                                                      hl_host_memory_mapping *output) {
    hl_host_windows *host = context;
    hl_host_result registered;
    hl_host_result claimed;
    void *address = NULL;
    const uint32_t placement = flags & (uint32_t)(HL_HOST_MEMORY_FIXED | HL_HOST_MEMORY_FIXED_NOREPLACE);
    const uint32_t sharing = flags & (uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE);
    if (output == NULL || output->abi != HL_HOST_MEMORY_MAPPING_ABI || output->size < sizeof(*output) || size == 0 ||
        size > SIZE_MAX || requested_address > UINTPTR_MAX ||
        (requested_address != 0 && requested_address % HL_WINDOWS_PAGE_SIZE != 0) ||
        !hl_windows_protection_valid(protection) ||
        (flags & ~(uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED |
                             HL_HOST_MEMORY_FIXED_NOREPLACE)) != 0 ||
        (sharing != HL_HOST_MEMORY_PRIVATE && sharing != HL_HOST_MEMORY_SHARED) ||
        (placement != 0 && placement != HL_HOST_MEMORY_FIXED && placement != HL_HOST_MEMORY_FIXED_NOREPLACE) ||
        (placement != 0 && requested_address == 0) ||
        (requested_address != 0 && size > (uint64_t)UINTPTR_MAX - requested_address))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_MAPPING);
    if (registered.status != HL_STATUS_OK) return registered;
    claimed = hl_windows_claim_anonymous(host, registered.value, placement != 0 ? requested_address : 0, size, 0,
                                         protection, placement, sharing, &address);
    if (claimed.status != HL_STATUS_OK) {
        hl_windows_lock(host);
        {
            hl_windows_handle_entry *entry =
                hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_MAPPING);
            if (entry != NULL) hl_windows_clear_entry_locked(entry);
        }
        hl_windows_unlock(host);
        return claimed;
    }
    *output = (hl_host_memory_mapping){
        HL_HOST_MEMORY_MAPPING_ABI, sizeof(*output), registered.value, (uint64_t)(uintptr_t)address, size, 0};
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- file mappings ---------------------------------------------------------- */

/*
 * The section's maximum protection is fixed at creation and cannot be raised, so
 * ask for the widest the file's granted access allows and fall back. A later
 * protect() to RX over a section created PAGE_READWRITE fails outright, which is
 * why this ladder exists rather than a single call with the requested protection.
 */
static HANDLE hl_windows_create_file_section(HANDLE file, uint32_t sharing) {
    static const DWORD shared_ladder[] = {PAGE_EXECUTE_READWRITE, PAGE_READWRITE, PAGE_EXECUTE_READ, PAGE_READONLY};
    static const DWORD private_ladder[] = {PAGE_EXECUTE_WRITECOPY, PAGE_WRITECOPY, PAGE_EXECUTE_READ, PAGE_READONLY};
    const DWORD *ladder = sharing == HL_HOST_MEMORY_SHARED ? shared_ladder : private_ladder;
    size_t index;
    for (index = 0; index < 4u; ++index) {
        /* Size zero means "the current file size": CreateFileMappingW EXTENDS the
         * file when given a larger size, which mmap never does. A mapping whose
         * tail runs past EOF is therefore refused rather than silently growing
         * the file. */
        HANDLE section = CreateFileMappingW(file, NULL, ladder[index], 0, 0, NULL);
        if (section != NULL) return section;
    }
    return NULL;
}

static hl_host_result hl_windows_memory_map_file(void *context, hl_host_handle file, uint64_t requested_address,
                                                 uint64_t offset, uint64_t size, uint32_t protection, uint32_t flags,
                                                 hl_host_file_mapping *output) {
    hl_host_windows *host = context;
    hl_host_result registered;
    hl_windows_handle_entry *entry;
    hl_windows_region region;
    HANDLE object;
    HANDLE section;
    uint32_t section_owned;
    void *placeholder;
    void *view;
    const uint32_t placement = flags & (uint32_t)(HL_HOST_MEMORY_FIXED | HL_HOST_MEMORY_FIXED_NOREPLACE);
    const uint32_t sharing = flags & (uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE);
    if (output == NULL || output->abi != HL_HOST_FILE_MAPPING_ABI || output->size < sizeof(*output) || size == 0 ||
        size > SIZE_MAX || offset > INT64_MAX || offset % HL_WINDOWS_PAGE_SIZE != 0 ||
        requested_address > UINTPTR_MAX || (requested_address != 0 && requested_address % HL_WINDOWS_PAGE_SIZE != 0) ||
        !hl_windows_protection_valid(protection) ||
        (flags & ~(uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED |
                             HL_HOST_MEMORY_FIXED_NOREPLACE)) != 0 ||
        (sharing != HL_HOST_MEMORY_SHARED && sharing != HL_HOST_MEMORY_PRIVATE) ||
        (placement != 0 && placement != HL_HOST_MEMORY_FIXED && placement != HL_HOST_MEMORY_FIXED_NOREPLACE) ||
        (placement != 0 && requested_address == 0) ||
        (requested_address != 0 && size > (uint64_t)UINTPTR_MAX - requested_address))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_MAPPING);
    if (registered.status != HL_STATUS_OK) return registered;

    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, file, HL_WINDOWS_HANDLE_FILE);
    if (entry == NULL) {
        /* A shared-memory object already IS a section on Windows, so it is used
         * directly instead of being wrapped in a second one. */
        entry = hl_windows_lookup_locked(host, file, HL_WINDOWS_HANDLE_SHARED_MEMORY);
        object = entry != NULL ? entry->object : NULL;
        section = object;
        section_owned = 0;
    } else {
        object = entry->object;
        section = hl_windows_create_file_section(object, sharing);
        section_owned = 1;
    }
    if (object == NULL || section == NULL) {
        hl_host_result failure =
            object == NULL ? hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0) : hl_windows_last_error_result();
        hl_windows_handle_entry *owned = hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_MAPPING);
        if (owned != NULL) hl_windows_clear_entry_locked(owned);
        hl_windows_unlock(host);
        return failure;
    }
    if (placement == HL_HOST_MEMORY_FIXED)
        hl_windows_release_overlapping_locked(host, registered.value, (uintptr_t)requested_address,
                                              (uintptr_t)(requested_address + size));
    placeholder = hl_windows_reserve_placeholder(&host->api,
                                                 (void *)(uintptr_t)(placement != 0 ? requested_address : 0), size, 0);
    view = placeholder == NULL
               ? NULL
               : host->api.map_view_of_file3(section, GetCurrentProcess(), placeholder, offset, (SIZE_T)size,
                                             MEM_REPLACE_PLACEHOLDER, hl_windows_page_protection(protection), NULL, 0);
    if (view != placeholder) {
        hl_host_result failure = hl_windows_last_error_result();
        hl_windows_handle_entry *owned;
        if (placeholder != NULL) (void)VirtualFree(placeholder, 0, MEM_RELEASE);
        if (section_owned) CloseHandle(section);
        owned = hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_MAPPING);
        if (owned != NULL) hl_windows_clear_entry_locked(owned);
        hl_windows_unlock(host);
        return failure;
    }
    region.offset = 0;
    region.size = size;
    region.section_offset = offset;
    region.state = HL_WINDOWS_REGION_VIEW;
    region.protection = protection;
    entry = hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL || hl_windows_regions_insert(entry, 0, &region) != HL_STATUS_OK) {
        (void)UnmapViewOfFile(view);
        if (section_owned) CloseHandle(section);
        if (entry != NULL) hl_windows_clear_entry_locked(entry);
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    entry->address = view;
    entry->size = size;
    entry->section = section;
    entry->section_file = section_owned ? object : NULL;
    entry->section_owned = section_owned;
    hl_windows_unlock(host);
    output->handle = registered.value;
    output->address = (uint64_t)(uintptr_t)view;
    output->mapped_size = size;
    output->reserved = 0;
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* --- the rest of the group --------------------------------------------------- */

static hl_host_result hl_windows_memory_protect(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size,
                                                uint32_t flags) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    const DWORD native = hl_windows_page_protection(flags);
    hl_status status = HL_STATUS_OK;
    uint32_t index;
    if (!hl_windows_protection_valid(flags)) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset ||
        !hl_windows_regions_cover(entry, offset, size)) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* VirtualProtect cannot span two distinct regions, and placeholder splitting
     * makes every carved subrange its own region, so this iterates where mprotect
     * is one call. A protect that covers only part of a region is applied to the
     * pages but is not recorded per-page: if that region is later partly carved,
     * the surviving view halves are restored at the region's recorded protection. */
    for (index = 0; index < entry->region_count && status == HL_STATUS_OK; ++index) {
        hl_windows_region *region = &entry->regions[index];
        const uint64_t region_end = region->offset + region->size;
        const uint64_t clip_low = region->offset > offset ? region->offset : offset;
        const uint64_t clip_high = region_end < offset + size ? region_end : offset + size;
        DWORD previous = 0;
        if (clip_low >= clip_high) continue;
        if (!VirtualProtect((char *)entry->address + clip_low, (SIZE_T)(clip_high - clip_low), native, &previous))
            status = hl_windows_status_from_error(GetLastError());
        else if (clip_low == region->offset && clip_high == region_end)
            region->protection = flags;
    }
    hl_windows_unlock(host);
    return hl_windows_result(status, 0, 0);
}

static hl_host_result hl_windows_memory_release(void *context, hl_host_handle mapping) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_status status;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* The ordering is fixed and the reverse of the Linux backend's: views go
     * before their placeholders, and the section handle goes last. */
    status = hl_windows_entry_teardown(host, entry);
    if (status == HL_STATUS_OK) hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    return hl_windows_result(status, 0, 0);
}

static hl_host_result hl_windows_memory_discard(void *context, hl_host_handle mapping) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* Ownership only. The address space is deliberately left exactly as it is. */
    if (entry->section != NULL && entry->section_owned) CloseHandle(entry->section);
    free(entry->regions);
    hl_windows_clear_entry_locked(entry);
    hl_windows_unlock(host);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_memory_unmap_range(void *context, hl_host_handle mapping, uint64_t offset,
                                                    uint64_t size) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_status status;
    uint64_t rounded;
    if (size == 0 || size > SIZE_MAX || offset % HL_WINDOWS_PAGE_SIZE != 0)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    /* Linux rounds a non-page-aligned length up internally and the Linux backend
     * deliberately permits that; Windows demands exact placeholder bounds, so the
     * rounding is done here instead. */
    rounded = hl_windows_round_up(size, HL_WINDOWS_PAGE_SIZE);
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    if (rounded > entry->size - offset) rounded = entry->size - offset;
    status = hl_windows_entry_carve(host, entry, offset, rounded);
    if (status == HL_STATUS_OK && offset == 0 && rounded == entry->size) {
        if (entry->section != NULL && entry->section_owned) CloseHandle(entry->section);
        free(entry->regions);
        hl_windows_clear_entry_locked(entry);
    }
    hl_windows_unlock(host);
    return hl_windows_result(status, 0, 0);
}

static hl_host_result hl_windows_memory_sync(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_status status = HL_STATUS_OK;
    uint32_t index;
    if (size == 0 || size > SIZE_MAX) return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    for (index = 0; index < entry->region_count && status == HL_STATUS_OK; ++index) {
        hl_windows_region *region = &entry->regions[index];
        const uint64_t region_end = region->offset + region->size;
        const uint64_t clip_low = region->offset > offset ? region->offset : offset;
        const uint64_t clip_high = region_end < offset + size ? region_end : offset + size;
        if (region->state != HL_WINDOWS_REGION_VIEW || clip_low >= clip_high) continue;
        if (!FlushViewOfFile((char *)entry->address + clip_low, (SIZE_T)(clip_high - clip_low)))
            status = hl_windows_status_from_error(GetLastError());
    }
    /* FlushViewOfFile only queues the writes to the filesystem; msync(MS_SYNC)
     * promises durability, so the file handle is flushed too. A pagefile-backed
     * section has no file and the second half is skipped. */
    if (status == HL_STATUS_OK && entry->section_file != NULL && !FlushFileBuffers(entry->section_file))
        status = hl_windows_status_from_error(GetLastError());
    hl_windows_unlock(host);
    return hl_windows_result(status, 0, 0);
}

static hl_host_result hl_windows_memory_publish(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    char *address;
    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    address = entry->executable_address != NULL ? entry->executable_address : entry->address;
    /* A no-op on coherent x86-64 I-caches, but documented as required for
     * executable regions created through VirtualAlloc2 and load-bearing for any
     * future ARM64 Windows host. */
    if (!FlushInstructionCache(GetCurrentProcess(), address + offset, (SIZE_T)size)) {
        hl_host_result failure = hl_windows_last_error_result();
        hl_windows_unlock(host);
        return failure;
    }
    hl_windows_unlock(host);
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

/* Dual-alias host: the writable alias is always writable, so the W^X gate has
 * nothing to open or close. The contract permits this in so many words. */
static hl_host_result hl_windows_memory_code_write(void *context) {
    (void)context;
    return hl_windows_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_windows_memory_reserve_code(void *context, uint64_t size, uint64_t alignment, uint32_t flags,
                                                     hl_host_code_mapping *output) {
    hl_host_windows *host = context;
    hl_host_result registered;
    hl_windows_handle_entry *entry;
    hl_windows_region region;
    HANDLE section = NULL;
    void *writable = NULL;
    void *executable = NULL;
    if (output == NULL || size == 0 || size > SIZE_MAX || size > INT64_MAX || alignment == 0 ||
        (alignment & (alignment - 1u)) != 0 || alignment < HL_WINDOWS_PAGE_SIZE)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(output, 0, sizeof(*output));
    registered = hl_windows_allocate_handle(host, HL_WINDOWS_HANDLE_MAPPING);
    if (registered.status != HL_STATUS_OK) return registered;

    hl_windows_lock(host);
    if ((flags & HL_HOST_CODE_DUAL_ALIAS) != 0) {
        /* The section's MAXIMUM protection must be PAGE_EXECUTE_READWRITE or the
         * RX view is refused, and it cannot be raised afterwards. Two independent
         * placeholders carry the two aliases; MEM_ADDRESS_REQUIREMENTS.Alignment
         * places each one, so there is no over-reserve-and-trim step. */
        section = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_EXECUTE_READWRITE, (DWORD)(size >> 32),
                                     (DWORD)(size & UINT64_C(0xFFFFFFFF)), NULL);
        if (section != NULL) {
            void *writable_placeholder = hl_windows_reserve_placeholder(&host->api, NULL, size, alignment);
            void *executable_placeholder =
                writable_placeholder == NULL ? NULL : hl_windows_reserve_placeholder(&host->api, NULL, size, alignment);
            if (executable_placeholder != NULL) {
                writable = host->api.map_view_of_file3(section, GetCurrentProcess(), writable_placeholder, 0,
                                                       (SIZE_T)size, MEM_REPLACE_PLACEHOLDER, PAGE_READWRITE, NULL, 0);
                executable =
                    host->api.map_view_of_file3(section, GetCurrentProcess(), executable_placeholder, 0, (SIZE_T)size,
                                                MEM_REPLACE_PLACEHOLDER, PAGE_EXECUTE_READ, NULL, 0);
            }
            if (writable == NULL && writable_placeholder != NULL)
                (void)VirtualFree(writable_placeholder, 0, MEM_RELEASE);
            if (executable == NULL && executable_placeholder != NULL)
                (void)VirtualFree(executable_placeholder, 0, MEM_RELEASE);
        }
    } else {
        void *placeholder = hl_windows_reserve_placeholder(&host->api, NULL, size, alignment);
        if (placeholder != NULL) {
            writable = host->api.virtual_alloc2(NULL, placeholder, (SIZE_T)size,
                                                MEM_RESERVE | MEM_COMMIT | MEM_REPLACE_PLACEHOLDER,
                                                PAGE_EXECUTE_READWRITE, NULL, 0);
            if (writable == NULL) (void)VirtualFree(placeholder, 0, MEM_RELEASE);
        }
        executable = writable;
    }
    if (writable == NULL || executable == NULL) {
        hl_host_result failure = hl_windows_last_error_result();
        hl_windows_handle_entry *owned;
        if (writable != NULL && section != NULL) (void)UnmapViewOfFile(writable);
        if (section != NULL) CloseHandle(section);
        owned = hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_MAPPING);
        if (owned != NULL) hl_windows_clear_entry_locked(owned);
        hl_windows_unlock(host);
        /* Arbitrary Code Guard (ProcessDynamicCodePolicy) forbids PAGE_EXECUTE*
         * outright; a process that enables it cannot host the JIT at all. */
        return failure;
    }
    region.offset = 0;
    region.size = size;
    region.section_offset = 0;
    region.state = section != NULL ? (uint32_t)HL_WINDOWS_REGION_VIEW : (uint32_t)HL_WINDOWS_REGION_COMMIT;
    region.protection = HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE;
    entry = hl_windows_lookup_locked(host, registered.value, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL || hl_windows_regions_insert(entry, 0, &region) != HL_STATUS_OK) {
        if (section != NULL) {
            (void)UnmapViewOfFile(writable);
            (void)UnmapViewOfFile(executable);
            CloseHandle(section);
        } else {
            (void)VirtualFree(writable, 0, MEM_RELEASE);
        }
        if (entry != NULL) hl_windows_clear_entry_locked(entry);
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    entry->address = writable;
    entry->executable_address = executable;
    entry->size = size;
    entry->section = section;
    entry->section_owned = section != NULL ? 1u : 0u;
    hl_windows_unlock(host);
    output->abi = 1;
    output->size = sizeof(*output);
    output->handle = registered.value;
    output->writable_address = (uint64_t)(uintptr_t)writable;
    output->executable_address = (uint64_t)(uintptr_t)executable;
    output->mapped_size = size;
    return hl_windows_result(HL_STATUS_OK, registered.value, 0);
}

/*
 * The fork child's code arena.
 *
 * This runs exactly once per guest fork(2), in the child, before it executes a
 * single translated instruction -- which makes it the first host call a cloned
 * process makes, and that is why the registry lock is re-initialised here rather
 * than somewhere more obvious. A peer thread of the parent may have held that
 * lock at the clone instant and no thread survives to release it, so the only
 * correct move is to overwrite it. Safe for the same reason it is necessary: a
 * clone carries exactly one thread, so nothing else in this process can be
 * inside the registry. The POSIX backends do the identical thing to their
 * registry mutex at the identical point.
 *
 * Only preserve == 0 is served, and the refusal of preserve != 0 is a
 * correctness statement rather than an omission. The arena is a pagefile-backed
 * SECTION mapped twice, and a section view survives this host's clone genuinely
 * shared -- measured, in both directions. So the child does not inherit a
 * private copy of the parent's code cache; it inherits the parent's code cache
 * itself, and the first block it translates would be written into memory the
 * parent is executing. A fresh section is therefore mandatory, and once the
 * backing object is new the old contents are worthless: their host addresses are
 * gone, which is precisely what preserve == 0 means.
 *
 * (The alias COUPLING does survive the clone -- the child can emit through the
 * inherited RW view and execute the result through the inherited RX view -- so
 * nothing here is working around a broken clone. What is being avoided is two
 * processes sharing one writable code cache.)
 *
 * The inherited views are unmapped rather than torn down through
 * hl_windows_entry_teardown, because teardown closes the section handle and this
 * child does not have one: a section created with default security is not
 * inheritable, so the numeric slot the parent's handle occupied is EMPTY here.
 * Closing it would at best fail and at worst close whatever a later CreateFileW
 * was given that value.
 */
static hl_host_result hl_windows_memory_repair_code(void *context, hl_host_code_mapping *mapping, uint32_t preserve) {
    hl_host_windows *host = context;
    hl_windows_handle_entry *entry;
    hl_windows_region region;
    HANDLE section = NULL;
    void *writable = NULL;
    void *executable = NULL;
    void *inherited_writable;
    void *inherited_executable;
    uint64_t size;
    uint64_t alignment;

    if (mapping == NULL || mapping->abi != 1 || mapping->size < sizeof(*mapping))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    InitializeSRWLock(&host->lock);
    if (preserve != 0) return hl_windows_result(HL_STATUS_NOT_SUPPORTED, 0, 0);

    hl_windows_lock(host);
    entry = hl_windows_lookup_locked(host, mapping->handle, HL_WINDOWS_HANDLE_MAPPING);
    if (entry == NULL || entry->size == 0 || entry->address == NULL) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    size = entry->size;
    inherited_writable = entry->address;
    inherited_executable = entry->executable_address;

    if (inherited_executable == NULL || inherited_executable == inherited_writable) {
        /* The single-alias fallback arena is private committed memory, not a
         * section view, so the clone gave this child its own copy-on-write copy
         * at the same address. Nothing is shared and nothing needs rebuilding;
         * the caller re-binds to the addresses it already had and starts with an
         * empty block map. */
        hl_windows_unlock(host);
        mapping->writable_address = (uint64_t)(uintptr_t)inherited_writable;
        mapping->executable_address = (uint64_t)(uintptr_t)inherited_writable;
        mapping->mapped_size = size;
        return hl_windows_result(HL_STATUS_OK, mapping->handle, 0);
    }

    /* The alignment the arena was reserved with is not recorded on the entry.
     * It does not need to be: the translator asks only for host-page alignment,
     * so that the two aliases differ by whole pages and their delta is a
     * constant, and every request at or below the reservation granule takes the
     * same plain VirtualAlloc2 path in the reservation helper. Naming the
     * granule therefore reproduces the create path exactly rather than
     * approximating it. */
    alignment = HL_WINDOWS_ALLOCATION_GRANULARITY;
    section = CreateFileMappingW(INVALID_HANDLE_VALUE, NULL, PAGE_EXECUTE_READWRITE, (DWORD)(size >> 32),
                                 (DWORD)(size & UINT64_C(0xFFFFFFFF)), NULL);
    if (section != NULL) {
        void *writable_placeholder = hl_windows_reserve_placeholder(&host->api, NULL, size, alignment);
        void *executable_placeholder =
            writable_placeholder == NULL ? NULL : hl_windows_reserve_placeholder(&host->api, NULL, size, alignment);
        if (executable_placeholder != NULL) {
            writable = host->api.map_view_of_file3(section, GetCurrentProcess(), writable_placeholder, 0, (SIZE_T)size,
                                                   MEM_REPLACE_PLACEHOLDER, PAGE_READWRITE, NULL, 0);
            executable = host->api.map_view_of_file3(section, GetCurrentProcess(), executable_placeholder, 0,
                                                     (SIZE_T)size, MEM_REPLACE_PLACEHOLDER, PAGE_EXECUTE_READ, NULL, 0);
        }
        if (writable == NULL && writable_placeholder != NULL) (void)VirtualFree(writable_placeholder, 0, MEM_RELEASE);
        if (executable == NULL && executable_placeholder != NULL)
            (void)VirtualFree(executable_placeholder, 0, MEM_RELEASE);
    }
    if (writable == NULL || executable == NULL) {
        const hl_host_result failure = hl_windows_last_error_result();
        if (writable != NULL) (void)UnmapViewOfFile(writable);
        if (section != NULL) CloseHandle(section);
        hl_windows_unlock(host);
        /* The inherited arena is left exactly as it was. It is the parent's and
         * this child must not use it, but the caller treats a failure here as
         * fatal to the child, so leaving it mapped is strictly safer than
         * leaving a hole a stale pointer could be dereferenced through. */
        return failure;
    }

    region.offset = 0;
    region.size = size;
    region.section_offset = 0;
    region.state = (uint32_t)HL_WINDOWS_REGION_VIEW;
    region.protection = HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE;
    /* The inherited region vector is this child's own copy-on-write copy of the
     * parent's, so it can be freed and rebuilt like any private allocation. */
    free(entry->regions);
    entry->regions = NULL;
    entry->region_count = 0;
    entry->region_capacity = 0;
    if (hl_windows_regions_insert(entry, 0, &region) != HL_STATUS_OK) {
        (void)UnmapViewOfFile(writable);
        (void)UnmapViewOfFile(executable);
        CloseHandle(section);
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_OUT_OF_MEMORY, 0, 0);
    }
    entry->address = writable;
    entry->executable_address = executable;
    entry->section = section;
    entry->section_owned = 1u;
    entry->size = size;
    hl_windows_unlock(host);

    /* Last, and outside nothing that could still fail: drop this child's views
     * of the PARENT's section. Until these two calls the parent's code cache is
     * writable from here. */
    (void)UnmapViewOfFile(inherited_executable);
    (void)UnmapViewOfFile(inherited_writable);

    mapping->writable_address = (uint64_t)(uintptr_t)writable;
    mapping->executable_address = (uint64_t)(uintptr_t)executable;
    mapping->mapped_size = size;
    return hl_windows_result(HL_STATUS_OK, mapping->handle, 0);
}

/*
 * Signal-context VM repair. This runs from a vectored exception handler on the
 * faulting thread, so it takes no lock, allocates nothing, logs nothing, touches
 * no ownership registry and branches on return values only -- never on
 * GetLastError(). VirtualProtect and VirtualAlloc are thin kernel32 -> ntdll ->
 * syscall stubs that touch no CRT state and take no loader lock, and both are
 * ordinary statically bound imports, so calling them cannot trigger the first
 * resolution of a delay-loaded thunk while the loader lock is held.
 *
 * The safety argument is structural rather than a list of async-signal-safe
 * calls: a vectored handler is only ever entered synchronously, from a faulting
 * instruction, on the faulting thread, so there is no "interrupted mid-malloc"
 * hazard to be safe against. What remains is same-thread lock recursion, which
 * is why the ban on locks is kept verbatim even though the POSIX reason for it
 * does not apply.
 *
 * The one Win32-only obligation: every call here overwrites the thread's last
 * error, which is a TEB field the faulting instruction stream may be about to
 * read. It is saved and restored around the whole ladder. (The handler boundary
 * does the same, so this is belt and braces for a caller reaching the callback
 * by some other route.)
 *
 * The ladder is the exact analogue of the POSIX one. Step two claims a vacant
 * range without replacement: VirtualAlloc with MEM_RESERVE over an already
 * reserved range fails (measured, 487), which IS MAP_FIXED_NOREPLACE semantics.
 * Its reservation is rounded down to the 64 KiB granule while the commit covers
 * only the requested page, so step three normalizes the page protection.
 */
static int hl_windows_memory_repair_signal_page(void *context, uint64_t address, uint64_t size, uint32_t protection) {
    DWORD saved_error;
    DWORD native;
    DWORD previous = 0;
    void *page;
    int repaired;
    (void)context;
    if (address == 0 || address > UINTPTR_MAX || size != HL_WINDOWS_PAGE_SIZE ||
        (address & (HL_WINDOWS_PAGE_SIZE - 1u)) != 0 || !hl_windows_protection_valid(protection))
        return 0;
    saved_error = GetLastError();
    page = (void *)(uintptr_t)address;
    native = hl_windows_page_protection(protection);
    if (VirtualProtect(page, (SIZE_T)size, native, &previous)) {
        repaired = 1;
    } else if (VirtualAlloc(page, (SIZE_T)size, MEM_RESERVE | MEM_COMMIT, native) == NULL) {
        repaired = 0;
    } else {
        repaired = VirtualProtect(page, (SIZE_T)size, native, &previous) != 0;
    }
    SetLastError(saved_error);
    return repaired;
}

/* --- address-keyed operations ------------------------------------------------ */

/* True while any live mapping handle covers a byte of [low, high). Both aliases
 * of a dual-alias code mapping count, because releasing either one out from
 * under its owner is exactly the failure this guard exists to prevent. */
static int hl_windows_range_owned_locked(const hl_host_windows *host, uintptr_t low, uintptr_t high) {
    uint32_t index;
    for (index = 0; index < host->handle_capacity; ++index) {
        const hl_windows_handle_entry *entry = &host->handles[index];
        if (entry->kind != HL_WINDOWS_HANDLE_MAPPING || entry->size == 0) continue;
        if (entry->address != NULL) {
            const uintptr_t owned = (uintptr_t)entry->address;
            if (low < owned + (uintptr_t)entry->size && owned < high) return 1;
        }
        if (entry->executable_address != NULL) {
            const uintptr_t owned = (uintptr_t)entry->executable_address;
            if (low < owned + (uintptr_t)entry->size && owned < high) return 1;
        }
    }
    return 0;
}

/*
 * Bounds and type of the one NT allocation containing `inside`, or 0 when that
 * address is free. VirtualQuery reports one protection run at a time and only
 * AllocationBase ties the runs of a single allocation together, so the end has
 * to be walked for rather than read off. MEM_RELEASE is defined on an
 * allocation, which is precisely why the caller needs these bounds.
 */
static uint64_t hl_windows_allocation_extent(void *inside, char **out_base, DWORD *out_type) {
    MEMORY_BASIC_INFORMATION info;
    char *base;
    char *cursor;
    if (VirtualQuery(inside, &info, sizeof(info)) != sizeof(info) || info.State == MEM_FREE ||
        info.AllocationBase == NULL)
        return 0;
    base = info.AllocationBase;
    *out_type = info.Type;
    cursor = base;
    for (;;) {
        char *next;
        if (VirtualQuery(cursor, &info, sizeof(info)) != sizeof(info)) break;
        if (info.State == MEM_FREE || (char *)info.AllocationBase != base) break;
        next = (char *)info.BaseAddress + info.RegionSize;
        if (next <= cursor) break; /* a zero-length run would spin; NT never reports one */
        cursor = next;
    }
    *out_base = base;
    return (uint64_t)(cursor - base);
}

/*
 * munmap over a range the engine holds no mapping handle for.
 *
 * The ownership refusal is decided over every live handle BEFORE anything is
 * unmapped, so a busy range is left exactly as it was found rather than partly
 * retired -- an address-keyed caller can never strand an owning handle over a
 * hole. A range with no mapping at all succeeds, which is what munmap does.
 *
 * Retiring is then per NT allocation. An allocation covered whole is released
 * outright -- UnmapViewOfFile for a section view, MEM_RELEASE for private pages.
 * An allocation covered only in part is split with MEM_PRESERVE_PLACEHOLDER and
 * the split-off placeholder released, which is the hole punch for private pages.
 * NT refuses that split on a mapped view, and there is no way around it here: a
 * view can only be taken apart by an owner that still knows its section handle
 * and the section offsets of the surviving head and tail, and by construction
 * this entry point has neither. That refusal is reported as the mapped Win32
 * error class -- it is never rounded up to success.
 */
static hl_host_result hl_windows_memory_unmap_address(void *context, uint64_t address, uint64_t size) {
    hl_host_windows *host = context;
    hl_status status = HL_STATUS_OK;
    char *cursor;
    char *end;
    if (address == 0 || size == 0 || size > SIZE_MAX || address > UINTPTR_MAX || address % HL_WINDOWS_PAGE_SIZE != 0 ||
        size % HL_WINDOWS_PAGE_SIZE != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    cursor = (char *)(uintptr_t)address;
    end = cursor + (uintptr_t)size;
    hl_windows_lock(host);
    if (hl_windows_range_owned_locked(host, (uintptr_t)cursor, (uintptr_t)end)) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_BUSY, 0, 0);
    }
    while (cursor < end && status == HL_STATUS_OK) {
        char *base = NULL;
        DWORD type = 0;
        const uint64_t extent = hl_windows_allocation_extent(cursor, &base, &type);
        char *clip_high;
        if (extent == 0) {
            /* Vacant. Step over the whole free run rather than a page at a time. */
            MEMORY_BASIC_INFORMATION info;
            char *next;
            if (VirtualQuery(cursor, &info, sizeof(info)) != sizeof(info)) break;
            next = (char *)info.BaseAddress + info.RegionSize;
            if (next <= cursor) break;
            cursor = next;
            continue;
        }
        clip_high = base + extent < end ? base + extent : end;
        if (cursor == base && clip_high == base + extent) {
            const BOOL released = type == MEM_MAPPED ? UnmapViewOfFile(base) : VirtualFree(base, 0, MEM_RELEASE);
            if (!released) status = hl_windows_status_from_error(GetLastError());
        } else if (!hl_windows_split_release(cursor, (uint64_t)(clip_high - cursor))) {
            status = hl_windows_status_from_error(GetLastError());
        }
        cursor = base + extent;
    }
    hl_windows_unlock(host);
    return hl_windows_result(status, 0, 0);
}

/* True while any live CODE mapping (either alias of a dual-alias pair) covers a
 * byte of [low, high). Narrower than hl_windows_range_owned_locked on purpose:
 * an address-keyed re-protect is refused only over code, where the protection is
 * an engine invariant made of the alias pair plus the W^X gate and the caller
 * holds neither handle. Every other owned range is the ordinary mprotect case. */
static int hl_windows_code_range_owned_locked(const hl_host_windows *host, uintptr_t low, uintptr_t high) {
    uint32_t index;
    for (index = 0; index < host->handle_capacity; ++index) {
        const hl_windows_handle_entry *entry = &host->handles[index];
        if (entry->kind != HL_WINDOWS_HANDLE_MAPPING || entry->size == 0 || entry->executable_address == NULL) continue;
        if (entry->address != NULL) {
            const uintptr_t owned = (uintptr_t)entry->address;
            if (low < owned + (uintptr_t)entry->size && owned < high) return 1;
        }
        {
            const uintptr_t owned = (uintptr_t)entry->executable_address;
            if (low < owned + (uintptr_t)entry->size && owned < high) return 1;
        }
    }
    return 0;
}

/* Page-align an address-keyed span the way mprotect(2) and msync(2) do: the
 * address must already be aligned, the length is rounded up. Zero when the
 * request cannot be expressed. Deliberately unlike unmap_address's exact-multiple
 * rule -- rounding an unmap up destroys pages the caller never named, rounding a
 * protect or a flush up only touches pages it was going to leave intact. */
static int hl_windows_address_span(uint64_t address, uint64_t size, uintptr_t *low, uint64_t *span) {
    uint64_t rounded;
    if (address == 0 || size == 0 || address > UINTPTR_MAX || address % HL_WINDOWS_PAGE_SIZE != 0) return 0;
    rounded = size + (HL_WINDOWS_PAGE_SIZE - 1u);
    if (rounded < size) return 0;
    rounded = hl_windows_round_down(rounded, HL_WINDOWS_PAGE_SIZE);
    if (rounded > SIZE_MAX || rounded > (uint64_t)UINTPTR_MAX - address) return 0;
    *low = (uintptr_t)address;
    *span = rounded;
    return 1;
}

/*
 * mprotect over a range the engine holds no mapping handle for.
 *
 * VirtualProtect refuses a range that crosses two NT allocations, where mprotect
 * spans anything, so the range is walked one allocation at a time -- the same
 * walk unmap_address does, and for the same reason: only AllocationBase ties the
 * protection runs of one allocation together. A sub-range that is entirely free
 * is skipped rather than failed, because there is nothing there whose protection
 * could be wrong; mprotect over a hole is ENOMEM on Linux, but this entry point
 * exists for callers holding only an address and its Linux counterpart is
 * likewise permissive about the tail of a partially-mapped request.
 */
static hl_host_result hl_windows_memory_protect_address(void *context, uint64_t address, uint64_t size,
                                                        uint32_t protection) {
    hl_host_windows *host = context;
    hl_status status = HL_STATUS_OK;
    DWORD native;
    uintptr_t low;
    uint64_t span;
    char *cursor;
    char *end;
    if (!hl_windows_protection_valid(protection) || !hl_windows_address_span(address, size, &low, &span))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    native = hl_windows_page_protection(protection);
    cursor = (char *)low;
    end = cursor + (uintptr_t)span;
    hl_windows_lock(host);
    /* Decided over every live code mapping BEFORE anything changes, so a refused
     * range is left exactly as it was found. */
    if (hl_windows_code_range_owned_locked(host, low, (uintptr_t)end)) {
        hl_windows_unlock(host);
        return hl_windows_result(HL_STATUS_BUSY, 0, 0);
    }
    while (cursor < end && status == HL_STATUS_OK) {
        MEMORY_BASIC_INFORMATION info;
        char *next;
        char *clip_high;
        DWORD previous = 0;
        if (VirtualQuery(cursor, &info, sizeof(info)) != sizeof(info)) {
            status = hl_windows_status_from_error(GetLastError());
            break;
        }
        next = (char *)info.BaseAddress + info.RegionSize;
        if (next <= cursor) break; /* NT never reports a zero-length run; a spin here would not end */
        clip_high = next < end ? next : end;
        if (info.State != MEM_FREE && info.State != MEM_RESERVE &&
            !VirtualProtect(cursor, (SIZE_T)(clip_high - cursor), native, &previous))
            status = hl_windows_status_from_error(GetLastError());
        cursor = clip_high;
    }
    hl_windows_unlock(host);
    return hl_windows_result(status, 0, 0);
}

/*
 * msync over a range the engine holds no mapping handle for.
 *
 * Only section views carry anything to write back, so the walk flushes the
 * MEM_MAPPED runs and steps over the rest -- private and free pages are a
 * success with nothing done, which is what the contract permits. A durable flush
 * additionally waits on the backing file, and the file handle is only reachable
 * through a live mapping entry that covers the run; a view this process does not
 * own gets the queued write-back FlushViewOfFile already performed. INVALIDATE
 * has no Windows counterpart: a view is coherent with the section by
 * construction, so there is no stale copy to drop.
 */
static hl_host_result hl_windows_memory_sync_address(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    hl_host_windows *host = context;
    hl_status status = HL_STATUS_OK;
    uintptr_t low;
    uint64_t span;
    char *cursor;
    char *end;
    if ((flags & ~(uint32_t)(HL_HOST_MEMORY_SYNC_ASYNC | HL_HOST_MEMORY_SYNC_INVALIDATE)) != 0 ||
        !hl_windows_address_span(address, size, &low, &span))
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    cursor = (char *)low;
    end = cursor + (uintptr_t)span;
    hl_windows_lock(host);
    while (cursor < end && status == HL_STATUS_OK) {
        MEMORY_BASIC_INFORMATION info;
        char *next;
        char *clip_high;
        if (VirtualQuery(cursor, &info, sizeof(info)) != sizeof(info)) {
            status = hl_windows_status_from_error(GetLastError());
            break;
        }
        next = (char *)info.BaseAddress + info.RegionSize;
        if (next <= cursor) break;
        clip_high = next < end ? next : end;
        if (info.State == MEM_COMMIT && info.Type == MEM_MAPPED &&
            !FlushViewOfFile(cursor, (SIZE_T)(clip_high - cursor)))
            status = hl_windows_status_from_error(GetLastError());
        cursor = clip_high;
    }
    if (status == HL_STATUS_OK && (flags & HL_HOST_MEMORY_SYNC_ASYNC) == 0) {
        uint32_t index;
        for (index = 0; index < host->handle_capacity && status == HL_STATUS_OK; ++index) {
            const hl_windows_handle_entry *entry = &host->handles[index];
            uintptr_t owned;
            if (entry->kind != HL_WINDOWS_HANDLE_MAPPING || entry->size == 0 || entry->section_file == NULL ||
                entry->address == NULL)
                continue;
            owned = (uintptr_t)entry->address;
            if (low >= owned + (uintptr_t)entry->size || owned >= (uintptr_t)end) continue;
            if (!FlushFileBuffers(entry->section_file)) status = hl_windows_status_from_error(GetLastError());
        }
    }
    hl_windows_unlock(host);
    return hl_windows_result(status, 0, 0);
}

/*
 * VirtualLock is the nearest primitive Windows has to mlock, and it is NOT the
 * same operation: it faults the range in and adds it to the process working set,
 * charged against the working-set minimum, but it does not pin the pages against
 * reclaim -- the memory manager may still trim them. So this reports
 * HL_HOST_WIRE_WORKING_SET rather than HL_HOST_WIRE_RESIDENT. Saying RESIDENT
 * here would be the one thing the kind field exists to prevent: the caller would
 * read a guarantee out of a success that this host never made.
 *
 * The size follows VirtualLock's own rule and covers every page the range
 * touches. The usual failure is ERROR_WORKING_SET_QUOTA when the requested range
 * exceeds the process working-set minimum; it is reported rather than papered
 * over with SetProcessWorkingSetSize, because silently enlarging the caller's
 * working set is a policy decision this layer does not get to make.
 */
static hl_host_result hl_windows_memory_wire_range(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || address > UINTPTR_MAX || flags != 0 ||
        address % HL_WINDOWS_PAGE_SIZE != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!VirtualLock((void *)(uintptr_t)address, (SIZE_T)size)) return hl_windows_last_error_result();
    return hl_windows_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_WORKING_SET);
}

/* The inverse, and it reports the same kind: what is being undone is a working-set
 * addition, not a residency pin. */
static hl_host_result hl_windows_memory_unwire_range(void *context, uint64_t address, uint64_t size) {
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || address > UINTPTR_MAX || address % HL_WINDOWS_PAGE_SIZE != 0 ||
        size > (uint64_t)UINTPTR_MAX - address)
        return hl_windows_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (!VirtualUnlock((void *)(uintptr_t)address, (SIZE_T)size)) return hl_windows_last_error_result();
    return hl_windows_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_WORKING_SET);
}

const hl_host_memory_services hl_windows_memory_services = {HL_HOST_MEMORY_ABI,
                                                            sizeof(hl_host_memory_services),
                                                            hl_windows_memory_reserve,
                                                            hl_windows_memory_protect,
                                                            hl_windows_memory_release,
                                                            hl_windows_memory_publish,
                                                            hl_windows_memory_reserve_code,
                                                            hl_windows_memory_repair_code,
                                                            hl_windows_memory_code_write,
                                                            hl_windows_memory_code_write,
                                                            hl_windows_memory_map_file,
                                                            hl_windows_memory_sync,
                                                            hl_windows_memory_unmap_range,
                                                            hl_windows_memory_map_anonymous,
                                                            hl_windows_memory_discard,
                                                            hl_windows_memory_repair_signal_page,
                                                            hl_windows_memory_unmap_address,
                                                            hl_windows_memory_wire_range,
                                                            hl_windows_memory_unwire_range,
                                                            hl_windows_memory_protect_address,
                                                            hl_windows_memory_sync_address};
