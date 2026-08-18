#include "arena.h"

#include <errno.h>
#include <stddef.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
#ifndef MEM_RESERVE_PLACEHOLDER
#define MEM_RESERVE_PLACEHOLDER 0x00040000
#endif
#else
#include <pthread.h>
#include <sys/mman.h>
#include <unistd.h>
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_vm.h>
#endif
#endif

static uint64_t arena_owner(void) {
#if defined(_WIN32)
    return ((uint64_t)GetCurrentProcessId() << 32) | (uint64_t)GetCurrentThreadId();
#else
    pthread_t thread = pthread_self();
    uint64_t bits = 0;
    size_t copied = sizeof(thread) < sizeof(bits) ? sizeof(thread) : sizeof(bits);
    memcpy(&bits, &thread, copied);
    uint32_t fingerprint = (uint32_t)(bits ^ (bits >> 32));
    if (fingerprint == 0) fingerprint = 1;
    return ((uint64_t)(uint32_t)getpid() << 32) | fingerprint;
#endif
}

static void arena_lock(hl_arena_authority *authority) {
    const uint64_t self = arena_owner();
    for (;;) {
        uint64_t current = atomic_load_explicit(&authority->owner, memory_order_acquire);
        if (current != 0 && (uint32_t)(current >> 32) != (uint32_t)(self >> 32)) {
            (void)atomic_compare_exchange_weak_explicit(&authority->owner, &current, 0, memory_order_acq_rel,
                                                        memory_order_acquire);
            continue;
        }
        uint64_t empty = 0;
        if (atomic_compare_exchange_weak_explicit(&authority->owner, &empty, self, memory_order_acq_rel,
                                                  memory_order_acquire))
            return;
    }
}

static uint64_t arena_host_granule(void) {
#if defined(_WIN32)
    SYSTEM_INFO information;
    GetSystemInfo(&information);
    return information.dwAllocationGranularity;
#elif defined(__APPLE__)
    vm_size_t page = 0;
    return host_page_size(mach_host_self(), &page) == KERN_SUCCESS ? page : 0;
#else
    long page = sysconf(_SC_PAGESIZE);
    return page > 0 ? (uint64_t)page : 0;
#endif
}

static void arena_unlock(hl_arena_authority *authority) {
    atomic_store_explicit(&authority->owner, 0, memory_order_release);
}

static int arena_claim(uint64_t base, uint64_t limit) {
    uint64_t length = limit - base;
#if defined(_WIN32)
    typedef PVOID(WINAPI * virtual_alloc2_fn)(HANDLE, PVOID, SIZE_T, ULONG, ULONG, PVOID, ULONG);
    HMODULE kernel = GetModuleHandleW(L"kernelbase.dll");
    virtual_alloc2_fn virtual_alloc2 =
        kernel != NULL ? (virtual_alloc2_fn)(uintptr_t)GetProcAddress(kernel, "VirtualAlloc2") : NULL;
    if (virtual_alloc2 == NULL) {
        errno = ENOTSUP;
        return -1;
    }
    void *address = virtual_alloc2(GetCurrentProcess(), (void *)(uintptr_t)base, (SIZE_T)length,
                                   MEM_RESERVE | MEM_RESERVE_PLACEHOLDER, PAGE_NOACCESS, NULL, 0);
    if (address != (void *)(uintptr_t)base) {
        if (address != NULL) (void)VirtualFree(address, 0, MEM_RELEASE);
        errno = EEXIST;
        return -1;
    }
#elif defined(__APPLE__)
    mach_vm_address_t address = (mach_vm_address_t)base;
    kern_return_t status = mach_vm_allocate(mach_task_self(), &address, (mach_vm_size_t)length, VM_FLAGS_FIXED);
    if (status != KERN_SUCCESS || address != (mach_vm_address_t)base) {
        if (status == KERN_SUCCESS) (void)mach_vm_deallocate(mach_task_self(), address, (mach_vm_size_t)length);
        errno = EEXIST;
        return -1;
    }
    if (mach_vm_protect(mach_task_self(), address, (mach_vm_size_t)length, FALSE, VM_PROT_NONE) != KERN_SUCCESS) {
        (void)mach_vm_deallocate(mach_task_self(), address, (mach_vm_size_t)length);
        errno = EACCES;
        return -1;
    }
#else
#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif
    void *address = mmap((void *)(uintptr_t)base, (size_t)length, PROT_NONE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (address != (void *)(uintptr_t)base) {
        if (address != MAP_FAILED) (void)munmap(address, (size_t)length);
        return -1;
    }
#endif
    return 0;
}

static void arena_release(uint64_t base, uint64_t limit) {
    uint64_t length = limit - base;
#if defined(_WIN32)
    (void)VirtualFree((void *)(uintptr_t)base, 0, MEM_RELEASE);
#elif defined(__APPLE__)
    (void)mach_vm_deallocate(mach_task_self(), (mach_vm_address_t)base, (mach_vm_size_t)length);
#else
    (void)munmap((void *)(uintptr_t)base, (size_t)length);
#endif
}

static int aligned_range(uint64_t base, uint64_t limit, uint64_t granule) {
    return granule != 0 && (granule & (granule - 1)) == 0 && base < limit && base % granule == 0 &&
           limit % granule == 0;
}

int hl_arena_manifest_valid(const hl_arena_manifest *manifest) {
    return manifest != NULL && manifest->magic == HL_ARENA_MANIFEST_MAGIC &&
           manifest->version == HL_ARENA_MANIFEST_VERSION && manifest->size == sizeof(*manifest) &&
           aligned_range(manifest->normal_base, manifest->normal_limit, manifest->granule) &&
           aligned_range(manifest->low32_base, manifest->low32_limit, manifest->granule) &&
           manifest->normal_cursor >= manifest->normal_base && manifest->normal_cursor <= manifest->normal_limit &&
           manifest->low32_cursor >= manifest->low32_base && manifest->low32_cursor <= manifest->low32_limit &&
           manifest->normal_cursor % manifest->granule == 0 && manifest->low32_cursor % manifest->granule == 0 &&
           (manifest->normal_limit <= manifest->low32_base || manifest->low32_limit <= manifest->normal_base) &&
           manifest->low32_limit <= UINT64_C(0x100000000) && manifest->generation != 0 &&
           manifest->next_identity != 0 && manifest->reservation_count <= HL_ARENA_MAX_RESERVATIONS;
}

int hl_arena_authority_init(hl_arena_authority *authority, const hl_arena_config *config) {
    uint64_t host_granule = arena_host_granule();
    if (authority == NULL || config == NULL || host_granule == 0 || config->granule < host_granule ||
        config->granule % host_granule != 0 ||
        !aligned_range(config->normal_base, config->normal_limit, config->granule) ||
        !aligned_range(config->low32_base, config->low32_limit, config->granule) ||
        !(config->normal_limit <= config->low32_base || config->low32_limit <= config->normal_base) ||
        config->low32_limit > UINT64_C(0x100000000)) {
        errno = EINVAL;
        return -1;
    }
    memset(authority, 0, sizeof(*authority));
    atomic_init(&authority->owner, 0);
    if (arena_claim(config->normal_base, config->normal_limit) != 0) return -1;
    if (arena_claim(config->low32_base, config->low32_limit) != 0) {
        arena_release(config->normal_base, config->normal_limit);
        return -1;
    }
    authority->manifest = (hl_arena_manifest){HL_ARENA_MANIFEST_MAGIC,
                                              HL_ARENA_MANIFEST_VERSION,
                                              sizeof(hl_arena_manifest),
                                              config->granule,
                                              config->normal_base,
                                              config->normal_limit,
                                              config->normal_base,
                                              config->low32_base,
                                              config->low32_limit,
                                              config->low32_base,
                                              1,
                                              1,
                                              0,
                                              0};
    authority->claimed_normal_base = config->normal_base;
    authority->claimed_normal_limit = config->normal_limit;
    authority->claimed_low32_base = config->low32_base;
    authority->claimed_low32_limit = config->low32_limit;
    authority->initialized = 1;
    return 0;
}

void hl_arena_authority_destroy(hl_arena_authority *authority) {
    uint64_t normal_base, normal_limit, low32_base, low32_limit;
    if (authority == NULL) return;
    arena_lock(authority);
    if (!authority->initialized) {
        arena_unlock(authority);
        return;
    }
    normal_base = authority->claimed_normal_base;
    normal_limit = authority->claimed_normal_limit;
    low32_base = authority->claimed_low32_base;
    low32_limit = authority->claimed_low32_limit;
    authority->initialized = 0;
    authority->reservation_count = 0;
    authority->active_transaction = 0;
    memset(&authority->manifest, 0, sizeof(authority->manifest));
    memset(authority->reservations, 0, sizeof(authority->reservations));
    arena_unlock(authority);
    arena_release(low32_base, low32_limit);
    arena_release(normal_base, normal_limit);
}

int hl_arena_manifest_get(hl_arena_authority *authority, hl_arena_manifest *manifest) {
    if (authority == NULL || manifest == NULL) return (errno = EINVAL, -1);
    arena_lock(authority);
    if (!authority->initialized) {
        arena_unlock(authority);
        return (errno = EINVAL, -1);
    }
    *manifest = authority->manifest;
    arena_unlock(authority);
    return hl_arena_manifest_valid(manifest) ? 0 : (errno = EINVAL, -1);
}

static uint64_t arena_checksum(const hl_arena_persisted_state *state) {
    const unsigned char *bytes = (const unsigned char *)state;
    const size_t length = offsetof(hl_arena_persisted_state, checksum);
    uint64_t checksum = UINT64_C(1469598103934665603);
    for (size_t index = 0; index < length; ++index)
        checksum = (checksum ^ bytes[index]) * UINT64_C(1099511628211);
    return checksum;
}

int hl_arena_persisted_state_valid(const hl_arena_persisted_state *state) {
    uint64_t normal_cursor;
    uint64_t low32_cursor;
    if (state == NULL || !hl_arena_manifest_valid(&state->manifest) || state->checksum != arena_checksum(state))
        return 0;
    normal_cursor = state->manifest.normal_base;
    low32_cursor = state->manifest.low32_base;
    for (uint32_t index = 0; index < state->manifest.reservation_count; ++index) {
        const hl_arena_reservation *reservation = &state->reservations[index];
        uint64_t base = reservation->zone == HL_ARENA_LOW32 ? state->manifest.low32_base : state->manifest.normal_base;
        uint64_t limit =
            reservation->zone == HL_ARENA_LOW32 ? state->manifest.low32_limit : state->manifest.normal_limit;
        if ((reservation->zone != HL_ARENA_NORMAL && reservation->zone != HL_ARENA_LOW32) ||
            reservation->state != HL_ARENA_RESERVATION_OWNED || reservation->identity == 0 ||
            reservation->identity >= state->manifest.next_identity || reservation->length == 0 ||
            reservation->address < base || reservation->address % state->manifest.granule != 0 ||
            reservation->length % state->manifest.granule != 0 || reservation->address > limit ||
            reservation->length > limit - reservation->address)
            return 0;
        for (uint32_t previous = 0; previous < index; ++previous) {
            const hl_arena_reservation *other = &state->reservations[previous];
            if (other->identity == reservation->identity) return 0;
            if (other->zone == reservation->zone && reservation->address < other->address + other->length &&
                other->address < reservation->address + reservation->length)
                return 0;
        }
        if (reservation->zone == HL_ARENA_LOW32) {
            if (reservation->address != low32_cursor) return 0;
            low32_cursor += reservation->length;
        } else {
            if (reservation->address != normal_cursor) return 0;
            normal_cursor += reservation->length;
        }
    }
    return normal_cursor == state->manifest.normal_cursor && low32_cursor == state->manifest.low32_cursor;
}

int hl_arena_persisted_state_get(hl_arena_authority *authority, hl_arena_persisted_state *state) {
    if (authority == NULL || state == NULL) return (errno = EINVAL, -1);
    arena_lock(authority);
    if (!authority->initialized || authority->active_transaction) {
        arena_unlock(authority);
        return (errno = EBUSY, -1);
    }
    memset(state, 0, sizeof(*state));
    state->manifest = authority->manifest;
    memcpy(state->reservations, authority->reservations, authority->reservation_count * sizeof(hl_arena_reservation));
    state->checksum = arena_checksum(state);
    arena_unlock(authority);
    return 0;
}

int hl_arena_transaction_begin(hl_arena_authority *authority, hl_arena_transaction *transaction) {
    if (authority == NULL || transaction == NULL) return (errno = EINVAL, -1);
    memset(transaction, 0, sizeof(*transaction));
    arena_lock(authority);
    if (!authority->initialized || !hl_arena_manifest_valid(&authority->manifest) ||
        authority->active_transaction != 0) {
        arena_unlock(authority);
        return (errno = EBUSY, -1);
    }
    if (authority->manifest.generation == UINT64_MAX) {
        arena_unlock(authority);
        return (errno = EOVERFLOW, -1);
    }
    authority->active_transaction = 1;
    authority->transaction_normal_cursor = authority->manifest.normal_cursor;
    authority->transaction_low32_cursor = authority->manifest.low32_cursor;
    authority->transaction_reservation_count = authority->reservation_count;
    authority->manifest.generation++;
    transaction->authority = authority;
    transaction->generation = authority->manifest.generation;
    transaction->active = 1;
    arena_unlock(authority);
    return 0;
}

int hl_arena_transaction_reserve(hl_arena_transaction *transaction, hl_arena_zone zone, uint64_t length,
                                 hl_arena_reservation *reservation) {
    hl_arena_authority *authority;
    uint64_t *cursor;
    uint64_t limit;
    uint64_t rounded;
    if (transaction == NULL || reservation == NULL || !transaction->active ||
        (zone != HL_ARENA_NORMAL && zone != HL_ARENA_LOW32))
        return (errno = EINVAL, -1);
    authority = transaction->authority;
    arena_lock(authority);
    if (!authority->active_transaction || transaction->generation != authority->manifest.generation || length == 0 ||
        length > UINT64_MAX - (authority->manifest.granule - 1) ||
        authority->reservation_count == HL_ARENA_MAX_RESERVATIONS) {
        arena_unlock(authority);
        return (errno = EINVAL, -1);
    }
    rounded = (length + authority->manifest.granule - 1) & ~(authority->manifest.granule - 1);
    cursor = zone == HL_ARENA_LOW32 ? &authority->manifest.low32_cursor : &authority->manifest.normal_cursor;
    limit = zone == HL_ARENA_LOW32 ? authority->manifest.low32_limit : authority->manifest.normal_limit;
    if (*cursor > limit || rounded > limit - *cursor) {
        arena_unlock(authority);
        return (errno = ENOMEM, -1);
    }
    if (authority->manifest.next_identity == UINT64_MAX) {
        arena_unlock(authority);
        return (errno = EOVERFLOW, -1);
    }
    *reservation = (hl_arena_reservation){authority->manifest.next_identity++, *cursor, rounded, (uint32_t)zone,
                                          HL_ARENA_RESERVATION_OWNED};
    authority->reservations[authority->reservation_count++] = *reservation;
    authority->manifest.reservation_count = authority->reservation_count;
    *cursor += rounded;
    arena_unlock(authority);
    return 0;
}

int hl_arena_transaction_commit(hl_arena_transaction *transaction) {
    if (transaction == NULL || !transaction->active) return (errno = EINVAL, -1);
    arena_lock(transaction->authority);
    if (!transaction->authority->active_transaction ||
        transaction->generation != transaction->authority->manifest.generation) {
        arena_unlock(transaction->authority);
        return (errno = EINVAL, -1);
    }
    transaction->authority->active_transaction = 0;
    transaction->active = 0;
    arena_unlock(transaction->authority);
    return 0;
}

void hl_arena_transaction_rollback(hl_arena_transaction *transaction) {
    hl_arena_authority *authority;
    if (transaction == NULL || !transaction->active) return;
    authority = transaction->authority;
    arena_lock(authority);
    if (authority->active_transaction && transaction->generation == authority->manifest.generation) {
        authority->manifest.normal_cursor = authority->transaction_normal_cursor;
        authority->manifest.low32_cursor = authority->transaction_low32_cursor;
        memset(&authority->reservations[authority->transaction_reservation_count], 0,
               (authority->reservation_count - authority->transaction_reservation_count) *
                   sizeof(hl_arena_reservation));
        authority->reservation_count = authority->transaction_reservation_count;
        authority->manifest.reservation_count = authority->reservation_count;
        authority->active_transaction = 0;
    }
    transaction->active = 0;
    arena_unlock(authority);
}

int hl_arena_reservation_owned(hl_arena_authority *authority, const hl_arena_reservation *reservation) {
    int owned = 0;
    if (authority == NULL || reservation == NULL || reservation->identity == 0) return 0;
    arena_lock(authority);
    for (uint32_t index = 0; index < authority->reservation_count; ++index) {
        const hl_arena_reservation *candidate = &authority->reservations[index];
        if (candidate->identity == reservation->identity && candidate->address == reservation->address &&
            candidate->length == reservation->length && candidate->zone == reservation->zone &&
            candidate->state == HL_ARENA_RESERVATION_OWNED) {
            owned = 1;
            break;
        }
    }
    arena_unlock(authority);
    return owned;
}
