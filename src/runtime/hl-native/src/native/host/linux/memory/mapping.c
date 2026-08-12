#ifndef _DEFAULT_SOURCE
#define _DEFAULT_SOURCE
#endif
#include "../../range.h"

#include <errno.h>
#include <sys/mman.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

size_t hl_host_page_size(void) {
    long value = sysconf(_SC_PAGESIZE);
    if (value <= 0 || ((size_t)value & ((size_t)value - 1)) != 0) return 0;
    return (size_t)value;
}

int hl_host_address_mapped(uintptr_t address) {
    size_t page_size = hl_host_page_size();
    unsigned char resident;
    uintptr_t page;
    if (page_size == 0) return 0;
    page = address & ~((uintptr_t)page_size - 1);
    // Only ENOMEM means unmapped. mincore() also reports EAGAIN when the kernel is transiently out of
    // resources, which under load turned live guest addresses into spurious EFAULTs.
    for (int attempt = 0; attempt < 64; attempt++) {
        errno = 0;
        if (mincore((void *)page, page_size, &resident) == 0) return 1;
        if (errno != EAGAIN) return errno != ENOMEM;
    }
    return 1;
}

int hl_host_region_query(uintptr_t address, hl_host_region *region) {
    FILE *maps;
    char line[512];
    if (region == NULL) return 0;
    maps = fopen("/proc/self/maps", "r");
    if (maps == NULL) return 0;
    while (fgets(line, sizeof line, maps) != NULL) {
        unsigned long long start;
        unsigned long long end;
        char protection[5] = {0};
        if (sscanf(line, "%llx-%llx %4s", &start, &end, protection) != 3 || end <= start || end <= address) continue;
        if (start > UINTPTR_MAX || end - start > SIZE_MAX) break;
        region->address = (uintptr_t)start;
        region->size = (size_t)(end - start);
        region->protection = (protection[0] == 'r' ? HL_HOST_REGION_READ : 0) |
                             (protection[1] == 'w' ? HL_HOST_REGION_WRITE : 0) |
                             (protection[2] == 'x' ? HL_HOST_REGION_EXECUTE : 0);
        fclose(maps);
        return 1;
    }
    fclose(maps);
    return 0;
}
static hl_host_result hl_linux_memory_reserve(void *context, uint64_t size, uint64_t alignment, uint32_t flags) {
    hl_host_linux *host = context;
    void *address;
    long page = sysconf(_SC_PAGESIZE);
    if (size == 0 || size > SIZE_MAX || page <= 0 || alignment > (uint64_t)page)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    address = mmap(NULL, (size_t)size, hl_linux_protection(flags), MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (address == MAP_FAILED) return hl_linux_errno_result();
    hl_host_result result = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, address, NULL, size, -1);
    if (result.status != HL_STATUS_OK) munmap(address, (size_t)size);
    return result;
}

static hl_host_result hl_linux_memory_protect(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size,
                                              uint32_t flags) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    result = mprotect((char *)entry->address + offset, (size_t)size, hl_linux_protection(flags));
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_release(void *context, hl_host_handle mapping) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int result;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    /* Unmap what the handle still holds, not the frame. A partial unmap can have given a subrange
     * back, and the address space is free to have handed that subrange to someone else since. With
     * no holes this is the one whole-frame munmap it has always been. */
    {
        uint64_t held_offset;
        uint64_t held_size;
        uint32_t part = 0;
        result = 0;
        while (result == 0 &&
               hl_host_hole_set_held_range(&entry->retired, entry->size, part, &held_offset, &held_size)) {
            result = munmap((char *)entry->address + held_offset, (size_t)held_size);
            ++part;
        }
    }
    if (result == 0 && entry->executable_address != NULL && entry->executable_address != entry->address)
        result = munmap(entry->executable_address, (size_t)entry->size);
    if (result == 0 && entry->descriptor >= 0) {
        // A dual-alias code mapping's fd was privatized (adopted) by hl_linux_memory_reserve_code; drop its
        // private-registry cell before closing (mirrors hl_linux_memory_repair_code). No-op for a plain
        // mapping whose fd was never adopted, so it is safe for every mapping kind.
        hl_host_process_fd_private_remove(entry->descriptor);
        result = close(entry->descriptor);
    }
    if (result == 0) {
        hl_linux_retire_mapping_locked(entry);
        entry->descriptor = -1;
    }
    pthread_mutex_unlock(&host->lock);
    return result == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_discard(void *context, hl_host_handle mapping) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    hl_linux_retire_mapping_locked(entry);
    entry->descriptor = -1;
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static int hl_linux_memory_repair_signal_page(void *context, uint64_t address, uint64_t size, uint32_t protection) {
    (void)context;
    if (address == 0 || address > UINTPTR_MAX || size != UINT64_C(4096) || (address & UINT64_C(4095)) != 0 ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0)
        return 0;
    void *page = (void *)(uintptr_t)address;
    int native_protection = hl_linux_protection(protection);
    if (mprotect(page, (size_t)size, native_protection) == 0) return 1;
#ifdef MAP_FIXED_NOREPLACE
    if (mmap(page, (size_t)size, native_protection, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0) == page)
        return 1;
    return mprotect(page, (size_t)size, native_protection) == 0;
#else
    return 0;
#endif
}

static hl_host_result hl_linux_memory_map_file(void *context, hl_host_handle file, uint64_t requested_address,
                                               uint64_t offset, uint64_t size, uint32_t protection, uint32_t flags,
                                               hl_host_file_mapping *output) {
    hl_host_linux *host = context;
    hl_host_result registered;
    void *address;
    int descriptor;
    long page = sysconf(_SC_PAGESIZE);
    int native_flags;
    uint32_t placement = flags & (HL_HOST_MEMORY_FIXED | HL_HOST_MEMORY_FIXED_NOREPLACE);
    uint32_t sharing = flags & (HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE);
    if (output == NULL || output->abi != HL_HOST_FILE_MAPPING_ABI || output->size < sizeof(*output) || size == 0 ||
        size > SIZE_MAX || offset > INT64_MAX || page <= 0 || offset % (uint64_t)page != 0 ||
        requested_address > UINTPTR_MAX || (requested_address != 0 && requested_address % (uint64_t)page != 0) ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        (flags & ~(uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED |
                             HL_HOST_MEMORY_FIXED_NOREPLACE)) != 0 ||
        (sharing != HL_HOST_MEMORY_SHARED && sharing != HL_HOST_MEMORY_PRIVATE) ||
        (placement != 0 && placement != HL_HOST_MEMORY_FIXED && placement != HL_HOST_MEMORY_FIXED_NOREPLACE) ||
        (placement != 0 && requested_address == 0) ||
        (requested_address != 0 && size > (uint64_t)UINTPTR_MAX - requested_address))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, NULL, NULL, size, -1);
    if (registered.status != HL_STATUS_OK) return registered;
    pthread_mutex_lock(&host->lock);
    descriptor = hl_linux_descriptor(host, file, HL_LINUX_HANDLE_FILE, HL_LINUX_HANDLE_SHARED_MEMORY);
    pthread_mutex_unlock(&host->lock);
    if (descriptor < 0) {
        (void)hl_linux_memory_discard(context, registered.value);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    native_flags = sharing == HL_HOST_MEMORY_SHARED ? MAP_SHARED : MAP_PRIVATE;
    if (placement == HL_HOST_MEMORY_FIXED) native_flags |= MAP_FIXED;
#ifdef MAP_FIXED_NOREPLACE
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) native_flags |= MAP_FIXED_NOREPLACE;
#else
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) {
        (void)hl_linux_memory_discard(context, registered.value);
        return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
#endif
    address = mmap((void *)(uintptr_t)requested_address, (size_t)size, hl_linux_protection(protection), native_flags,
                   descriptor, (off_t)offset);
    if (address == MAP_FAILED) {
        hl_host_result failure = hl_linux_errno_result();
        (void)hl_linux_memory_discard(context, registered.value);
        return failure;
    }
    /* MAP_FIXED replaced these VMAs atomically. Retire stale ownership handles without unmapping the
     * new VMA -- but only the ones that still held a byte of it. A handle whose overlap with this
     * range is entirely inside a hole it already gave back kept nothing here to go stale. */
    if (placement == HL_HOST_MEMORY_FIXED) {
        uintptr_t low = (uintptr_t)address, high = low + (uintptr_t)size;
        pthread_mutex_lock(&host->lock);
        for (uint32_t index = 0; index < host->handle_capacity; ++index) {
            hl_linux_handle_entry *entry = &host->handles[index];
            if (hl_linux_encode_handle(index, entry->generation) != registered.value &&
                hl_linux_entry_holds_locked(entry, low, high))
                hl_linux_retire_mapping_locked(entry);
        }
        pthread_mutex_unlock(&host->lock);
    }
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *owned = hl_linux_lookup_locked(host, registered.value, HL_LINUX_HANDLE_MAPPING);
    if (owned != NULL) owned->address = address;
    pthread_mutex_unlock(&host->lock);
    output->handle = registered.value;
    output->address = (uint64_t)(uintptr_t)address;
    output->mapped_size = size;
    output->reserved = 0;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_memory_map_anonymous(void *context, uint64_t requested_address, uint64_t size,
                                                    uint32_t protection, uint32_t flags,
                                                    hl_host_memory_mapping *output) {
    hl_host_linux *host = context;
    hl_host_result registered;
    void *address;
    long page = sysconf(_SC_PAGESIZE);
    uint32_t placement = flags & (HL_HOST_MEMORY_FIXED | HL_HOST_MEMORY_FIXED_NOREPLACE);
    uint32_t sharing = flags & (HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE);
    if (output == NULL || output->abi != HL_HOST_MEMORY_MAPPING_ABI || output->size < sizeof(*output) || size == 0 ||
        size > SIZE_MAX || page <= 0 || requested_address > UINTPTR_MAX ||
        (requested_address != 0 && requested_address % (uint64_t)page != 0) ||
        (protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        (flags & ~(uint32_t)(HL_HOST_MEMORY_SHARED | HL_HOST_MEMORY_PRIVATE | HL_HOST_MEMORY_FIXED |
                             HL_HOST_MEMORY_FIXED_NOREPLACE)) != 0 ||
        (sharing != HL_HOST_MEMORY_PRIVATE && sharing != HL_HOST_MEMORY_SHARED) ||
        (placement != 0 && placement != HL_HOST_MEMORY_FIXED && placement != HL_HOST_MEMORY_FIXED_NOREPLACE) ||
        (placement != 0 && requested_address == 0) ||
        (requested_address != 0 && size > (uint64_t)UINTPTR_MAX - requested_address))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    registered = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, NULL, NULL, size, -1);
    if (registered.status != HL_STATUS_OK) return registered;
    int native_flags = (sharing == HL_HOST_MEMORY_SHARED ? MAP_SHARED : MAP_PRIVATE) | MAP_ANONYMOUS;
    if (placement == HL_HOST_MEMORY_FIXED) native_flags |= MAP_FIXED;
#ifdef MAP_FIXED_NOREPLACE
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) native_flags |= MAP_FIXED_NOREPLACE;
#else
    if (placement == HL_HOST_MEMORY_FIXED_NOREPLACE) {
        (void)hl_linux_memory_discard(context, registered.value);
        return hl_linux_result(HL_STATUS_NOT_SUPPORTED, 0, 0);
    }
#endif
    address =
        mmap((void *)(uintptr_t)requested_address, (size_t)size, hl_linux_protection(protection), native_flags, -1, 0);
    if (address == MAP_FAILED) {
        hl_host_result failure = hl_linux_errno_result();
        (void)hl_linux_memory_discard(context, registered.value);
        return failure;
    }
    if (placement == HL_HOST_MEMORY_FIXED) {
        uintptr_t low = (uintptr_t)address, high = low + (uintptr_t)size;
        pthread_mutex_lock(&host->lock);
        for (uint32_t index = 0; index < host->handle_capacity; ++index) {
            hl_linux_handle_entry *entry = &host->handles[index];
            if (hl_linux_encode_handle(index, entry->generation) != registered.value &&
                hl_linux_entry_holds_locked(entry, low, high))
                hl_linux_retire_mapping_locked(entry);
        }
        pthread_mutex_unlock(&host->lock);
    }
    pthread_mutex_lock(&host->lock);
    hl_linux_handle_entry *owned = hl_linux_lookup_locked(host, registered.value, HL_LINUX_HANDLE_MAPPING);
    if (owned != NULL) owned->address = address;
    pthread_mutex_unlock(&host->lock);
    *output = (hl_host_memory_mapping){
        HL_HOST_MEMORY_MAPPING_ABI, sizeof(*output), registered.value, (uint64_t)(uintptr_t)address, size, 0};
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_memory_sync(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int status;
    if (size == 0 || size > SIZE_MAX) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = msync((char *)entry->address + offset, (size_t)size, MS_SYNC);
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_unmap_range(void *context, hl_host_handle mapping, uint64_t offset,
                                                  uint64_t size) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    int status;
    long page = sysconf(_SC_PAGESIZE);
    /* Linux requires the start address (and therefore the mapping-relative offset) to be page aligned,
     * but accepts an arbitrary positive length and rounds its end up internally. File mappings commonly
     * retain their exact byte length in the engine handle; memmap2 then passes that same non-page-aligned
     * length to munmap. Rejecting it here incorrectly surfaced EINVAL to an otherwise valid Linux guest. */
    if (size == 0 || size > SIZE_MAX || page <= 0 || offset % (uint64_t)page != 0)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    status = munmap((char *)entry->address + offset, (size_t)size);
    if (status == 0) {
        /* A full-range unmap consumes the handle. A partial one keeps it, so the subrange it just
         * gave back has to leave the handle's coverage too -- otherwise the handle goes on claiming
         * a hole the address space is free to hand to someone else. When repeated partial unmaps
         * finally leave nothing held, the handle is consumed exactly as a single full one would
         * have consumed it: a live mapping handle always holds at least one byte.
         *
         * If the record cannot grow, the subrange stays claimed. That refuses a reuse that would
         * have been legal, which is recoverable; the other direction is not. The same reasoning
         * applies to the tail of a non-page-aligned length: the kernel rounds it up and this does
         * not, so the handle keeps claiming those last bytes rather than guessing them away. */
        if ((offset == 0 && size == entry->size) || (hl_host_hole_set_retire(&entry->retired, offset, size) &&
                                                     !hl_host_hole_set_holds(&entry->retired, 0, entry->size)))
            hl_linux_retire_mapping_locked(entry);
    }
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

/* True while any live mapping handle still holds a byte of [low, high). */
static int hl_linux_range_owned_locked(hl_host_linux *host, uintptr_t low, uintptr_t high) {
    for (uint32_t index = 0; index < host->handle_capacity; ++index)
        if (hl_linux_entry_holds_locked(&host->handles[index], low, high)) return 1;
    return 0;
}

/* Reject before acting, so a refused range is left exactly as it was found. */
static hl_host_result hl_linux_memory_unmap_address(void *context, uint64_t address, uint64_t size) {
    hl_host_linux *host = context;
    long page = sysconf(_SC_PAGESIZE);
    uintptr_t low;
    int status;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX ||
        address % (uint64_t)page != 0 || size % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    low = (uintptr_t)address;
    pthread_mutex_lock(&host->lock);
    if (hl_linux_range_owned_locked(host, low, low + (uintptr_t)size)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_BUSY, 0, 0);
    }
    status = munmap((void *)low, (size_t)size);
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

/* True while any live CODE mapping handle still holds a byte of [low, high). A code mapping is the
 * one whose protection an address-keyed caller must not touch: the writable and executable views
 * are a pair, the per-thread write gate flips between them, and a caller holding only an address
 * cannot put back what it changed because it does not hold the handle that knows the other view. */
static int hl_linux_code_range_owned_locked(hl_host_linux *host, uintptr_t low, uintptr_t high) {
    for (uint32_t index = 0; index < host->handle_capacity; ++index) {
        const hl_linux_handle_entry *entry = &host->handles[index];
        if (entry->executable_address != NULL && hl_linux_entry_holds_locked(entry, low, high)) return 1;
    }
    return 0;
}

/* Page-align an address-keyed span the way mprotect(2) and msync(2) do: the address must already be
 * aligned and the length is rounded up. Returns zero when the request cannot be expressed. */
static int hl_linux_address_span(uint64_t address, uint64_t size, uintptr_t *low, size_t *span) {
    long page = sysconf(_SC_PAGESIZE);
    uint64_t rounded;
    if (address == 0 || size == 0 || page <= 0 || address > UINTPTR_MAX || address % (uint64_t)page != 0) return 0;
    rounded = size + ((uint64_t)page - 1u);
    if (rounded < size) return 0;
    rounded -= rounded % (uint64_t)page;
    if (rounded > SIZE_MAX || rounded > (uint64_t)UINTPTR_MAX - address) return 0;
    *low = (uintptr_t)address;
    *span = (size_t)rounded;
    return 1;
}

/* Reject before acting, so a refused range is left exactly as it was found. */
static hl_host_result hl_linux_memory_protect_address(void *context, uint64_t address, uint64_t size,
                                                      uint32_t protection) {
    hl_host_linux *host = context;
    uintptr_t low;
    size_t span;
    int status;
    if ((protection & ~(uint32_t)(HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE | HL_HOST_MEMORY_EXECUTE)) != 0 ||
        !hl_linux_address_span(address, size, &low, &span))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    pthread_mutex_lock(&host->lock);
    if (hl_linux_code_range_owned_locked(host, low, low + (uintptr_t)span)) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_BUSY, 0, 0);
    }
    status = mprotect((void *)low, span, hl_linux_protection(protection));
    pthread_mutex_unlock(&host->lock);
    return status == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

static hl_host_result hl_linux_memory_sync_address(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    uintptr_t low;
    size_t span;
    int native_flags;
    (void)context;
    if ((flags & ~(uint32_t)(HL_HOST_MEMORY_SYNC_ASYNC | HL_HOST_MEMORY_SYNC_INVALIDATE)) != 0 ||
        !hl_linux_address_span(address, size, &low, &span))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    native_flags = (flags & HL_HOST_MEMORY_SYNC_ASYNC) != 0 ? MS_ASYNC : MS_SYNC;
    if ((flags & HL_HOST_MEMORY_SYNC_INVALIDATE) != 0) native_flags |= MS_INVALIDATE;
    /* No ownership question is asked. Flushing takes nothing away from a handle that covers the
     * range: the mapping, its protection and its contents are all exactly as they were. */
    return msync((void *)low, span, native_flags) == 0 ? hl_linux_result(HL_STATUS_OK, 0, 0) : hl_linux_errno_result();
}

/* mlock(2) pins against reclaim and charges RLIMIT_MEMLOCK, so this host reports HL_HOST_WIRE_RESIDENT.
 * The length follows mlock's own rule and is rounded up to whole pages by the kernel. */
static hl_host_result hl_linux_memory_wire_range(void *context, uint64_t address, uint64_t size, uint32_t flags) {
    long page = sysconf(_SC_PAGESIZE);
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX || flags != 0 ||
        address % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (mlock((void *)(uintptr_t)address, (size_t)size) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_RESIDENT);
}

static hl_host_result hl_linux_memory_unwire_range(void *context, uint64_t address, uint64_t size) {
    long page = sysconf(_SC_PAGESIZE);
    (void)context;
    if (address == 0 || size == 0 || size > SIZE_MAX || page <= 0 || address > UINTPTR_MAX ||
        address % (uint64_t)page != 0 || size > (uint64_t)UINTPTR_MAX - address)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    if (munlock((void *)(uintptr_t)address, (size_t)size) != 0) return hl_linux_errno_result();
    return hl_linux_result(HL_STATUS_OK, 0, (uint64_t)HL_HOST_WIRE_RESIDENT);
}

static hl_host_result hl_linux_memory_publish(void *context, hl_host_handle mapping, uint64_t offset, uint64_t size) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || offset > entry->size || size > entry->size - offset) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    char *address = entry->executable_address != NULL ? entry->executable_address : entry->address;
    __builtin___clear_cache(address + offset, address + offset + size);
    pthread_mutex_unlock(&host->lock);
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static hl_host_result hl_linux_memory_code_write(void *context) {
    (void)context;
    return hl_linux_result(HL_STATUS_OK, 0, 0);
}

static void *hl_linux_map_aligned(int descriptor, uint64_t size, uint64_t alignment, int protection, int flags) {
    size_t reserve_size;
    void *reservation;
    uintptr_t base;
    uintptr_t aligned;
    if (alignment <= (uint64_t)sysconf(_SC_PAGESIZE)) return mmap(NULL, (size_t)size, protection, flags, descriptor, 0);
    if (size > SIZE_MAX - alignment) {
        errno = ENOMEM;
        return MAP_FAILED;
    }
    reserve_size = (size_t)(size + alignment);
    reservation = mmap(NULL, reserve_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (reservation == MAP_FAILED) return MAP_FAILED;
    base = (uintptr_t)reservation;
    aligned = (base + (uintptr_t)alignment - 1) & ~((uintptr_t)alignment - 1);
    if (aligned != base) (void)munmap((void *)base, (size_t)(aligned - base));
    if (base + reserve_size != aligned + size)
        (void)munmap((void *)(aligned + size), (size_t)(base + reserve_size - aligned - size));
    reservation = mmap((void *)aligned, (size_t)size, protection, flags | MAP_FIXED, descriptor, 0);
    return reservation;
}

static hl_host_result hl_linux_memory_reserve_code(void *context, uint64_t size, uint64_t alignment, uint32_t flags,
                                                   hl_host_code_mapping *output) {
    hl_host_linux *host = context;
    long page = sysconf(_SC_PAGESIZE);
    int descriptor;
    void *writable;
    void *executable;
    hl_host_result handle;
    if (output == NULL || size == 0 || size > SIZE_MAX || size > INT64_MAX || page <= 0 || alignment == 0 ||
        (alignment & (alignment - 1)) != 0 || alignment < (uint64_t)page)
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    memset(output, 0, sizeof(*output));
    if ((flags & HL_HOST_CODE_DUAL_ALIAS) == 0) {
        writable =
            hl_linux_map_aligned(-1, size, alignment, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS);
        if (writable == MAP_FAILED) return hl_linux_errno_result();
        handle = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, -1, writable, writable, size, -1);
        if (handle.status != HL_STATUS_OK) {
            munmap(writable, (size_t)size);
            return handle;
        }
        output->abi = 1;
        output->size = sizeof(*output);
        output->handle = handle.value;
        output->writable_address = (uint64_t)(uintptr_t)writable;
        output->executable_address = (uint64_t)(uintptr_t)writable;
        output->mapped_size = size;
        return handle;
    }
    descriptor = memfd_create("hl-code", MFD_CLOEXEC);
    if (descriptor < 0) return hl_linux_errno_result();
    if (ftruncate(descriptor, (off_t)size) != 0) {
        close(descriptor);
        return hl_linux_errno_result();
    }
    writable = hl_linux_map_aligned(descriptor, size, alignment, PROT_READ | PROT_WRITE, MAP_SHARED);
    if (writable == MAP_FAILED) {
        close(descriptor);
        return hl_linux_errno_result();
    }
    executable = hl_linux_map_aligned(descriptor, size, alignment, PROT_READ | PROT_EXEC, MAP_SHARED);
    if (executable == MAP_FAILED) {
        munmap(writable, (size_t)size);
        close(descriptor);
        return hl_linux_errno_result();
    }
    handle = hl_linux_allocate_handle(host, HL_LINUX_HANDLE_MAPPING, descriptor, writable, executable, size, -1);
    if (handle.status != HL_STATUS_OK) {
        munmap(executable, (size_t)size);
        munmap(writable, (size_t)size);
        close(descriptor);
        return handle;
    }
    output->abi = 1;
    output->size = sizeof(*output);
    output->handle = handle.value;
    output->writable_address = (uint64_t)(uintptr_t)writable;
    output->executable_address = (uint64_t)(uintptr_t)executable;
    output->mapped_size = size;
    return hl_linux_result(HL_STATUS_OK, handle.value, 0);
}

static hl_host_result hl_linux_memory_repair_code(void *context, hl_host_code_mapping *mapping, uint32_t preserve) {
    hl_host_linux *host = context;
    hl_linux_handle_entry *entry;
    hl_linux_handle_entry inherited;
    int descriptor = -1;
    void *writable = MAP_FAILED;
    void *executable = MAP_FAILED;
    if (mapping == NULL || mapping->abi != 1 || mapping->size < sizeof(*mapping))
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);

    /* This entry point is called only in the fork child. A sibling may have
       owned the process-private registry lock when fork cloned the caller, so
       its inherited pthread state cannot be acquired or destroyed safely. */
    {
        pthread_mutex_t fresh = PTHREAD_MUTEX_INITIALIZER;
        memcpy(&host->lock, &fresh, sizeof(fresh));
    }
    pthread_mutex_lock(&host->lock);
    entry = hl_linux_lookup_locked(host, mapping->handle, HL_LINUX_HANDLE_MAPPING);
    if (entry == NULL || entry->executable_address == NULL) {
        pthread_mutex_unlock(&host->lock);
        return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);
    }
    inherited = *entry;
    pthread_mutex_unlock(&host->lock);
    if (mapping->content_size > inherited.size) return hl_linux_result(HL_STATUS_INVALID_ARGUMENT, 0, 0);

    if (inherited.executable_address != inherited.address) {
        descriptor = memfd_create("hl-code", MFD_CLOEXEC);
        if (descriptor < 0) return hl_linux_errno_result();
        if (ftruncate(descriptor, (off_t)inherited.size) != 0) goto fresh_failed;
        writable = mmap(NULL, (size_t)inherited.size, PROT_READ | PROT_WRITE, MAP_SHARED, descriptor, 0);
        if (writable == MAP_FAILED) goto fresh_failed;
        executable = mmap(NULL, (size_t)inherited.size, PROT_READ | PROT_EXEC, MAP_SHARED, descriptor, 0);
        if (executable == MAP_FAILED) goto fresh_failed;

        if (preserve != 0) {
            /* Linux inherits MAP_SHARED memfd pages as genuinely process-shared
               pages. Give the child a private backing object while retaining
               the exact cache addresses and bytes expected by every map entry,
               chain, and inline-cache pointer. */
            memcpy(writable, inherited.address, (size_t)mapping->content_size);
            if (mmap(inherited.address, (size_t)inherited.size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_FIXED,
                     descriptor, 0) == MAP_FAILED ||
                mmap(inherited.executable_address, (size_t)inherited.size, PROT_READ | PROT_EXEC,
                     MAP_SHARED | MAP_FIXED, descriptor, 0) == MAP_FAILED)
                goto fresh_failed;
            (void)munmap(executable, (size_t)inherited.size);
            (void)munmap(writable, (size_t)inherited.size);
            writable = inherited.address;
            executable = inherited.executable_address;
        }

        /* Publish the replacement under the same opaque handle and generation.
           The inherited VMAs remain mapped until publication is complete, so a
           new alias can never be accidentally removed through a reused VA. */
        int adopted = hl_host_process_fd_private_adopt(descriptor);
        if (adopted < 0) goto fresh_failed;
        descriptor = adopted;
        pthread_mutex_lock(&host->lock);
        entry = hl_linux_lookup_locked(host, mapping->handle, HL_LINUX_HANDLE_MAPPING);
        if (entry == NULL || entry->descriptor != inherited.descriptor || entry->address != inherited.address ||
            entry->executable_address != inherited.executable_address || entry->size != inherited.size) {
            pthread_mutex_unlock(&host->lock);
            hl_host_process_fd_private_remove(descriptor);
            goto fresh_failed;
        }
        entry->descriptor = descriptor;
        entry->address = writable;
        entry->executable_address = executable;
        pthread_mutex_unlock(&host->lock);

        hl_host_process_fd_private_remove(inherited.descriptor);
        if (preserve == 0) {
            (void)munmap(inherited.executable_address, (size_t)inherited.size);
            (void)munmap(inherited.address, (size_t)inherited.size);
        }
        if (inherited.descriptor >= 0) (void)close(inherited.descriptor);
    } else {
        writable = inherited.address;
        executable = inherited.executable_address;
    }

    mapping->writable_address = (uint64_t)(uintptr_t)writable;
    mapping->executable_address = (uint64_t)(uintptr_t)executable;
    mapping->mapped_size = inherited.size;
    return hl_linux_result(HL_STATUS_OK, mapping->handle, 0);

fresh_failed: {
    int error = errno;
    if (executable != MAP_FAILED) (void)munmap(executable, (size_t)inherited.size);
    if (writable != MAP_FAILED) (void)munmap(writable, (size_t)inherited.size);
    if (descriptor >= 0) (void)close(descriptor);
    errno = error;
    return hl_linux_errno_result();
}
}
