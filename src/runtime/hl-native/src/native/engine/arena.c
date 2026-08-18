#include "arena.h"

#include <errno.h>
#include <stddef.h>
#include <string.h>

#if defined(_WIN32)
#include <windows.h>
#include <bcrypt.h>
#ifndef MEM_RESERVE_PLACEHOLDER
#define MEM_RESERVE_PLACEHOLDER 0x00040000
#endif
#ifndef MEM_REPLACE_PLACEHOLDER
#define MEM_REPLACE_PLACEHOLDER 0x00004000
#endif
#ifndef MEM_PRESERVE_PLACEHOLDER
#define MEM_PRESERVE_PLACEHOLDER 0x00000002
#endif
#else
#include <pthread.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>
#if defined(__linux__)
#include <sys/random.h>
#endif
#if defined(__APPLE__)
#include <mach/mach.h>
#include <mach/mach_vm.h>
#endif
#endif

#if ATOMIC_INT_LOCK_FREE != 2
#error "arena lifecycle and fork recovery require lock-free 32-bit atomics"
#endif
#if (defined(_WIN32) && ATOMIC_LLONG_LOCK_FREE != 2) || (!defined(_WIN32) && ATOMIC_LONG_LOCK_FREE != 2)
#error "arena identity and fork recovery require lock-free 64-bit atomics"
#endif

static _Atomic uint64_t arena_authority_sequence = 1;
static _Atomic uint64_t arena_fork_sequence = 1;
static _Atomic uint64_t arena_nonce_process;
static _Atomic uint64_t arena_nonce_initializer_process;
static _Atomic uint64_t arena_process_nonce;
#if defined(HL_NATIVE_TEST_HOOKS)
static _Atomic uint32_t arena_test_restore_failure;
#endif

static void arena_lock(hl_arena_authority *authority);
static void arena_unlock(hl_arena_authority *authority);
static int arena_placeholder_restore(uint64_t address, uint64_t length);

static uint64_t arena_process_id(void) {
#if defined(_WIN32)
    return (uint64_t)GetCurrentProcessId();
#else
    return (uint64_t)(uint32_t)getpid();
#endif
}

static int arena_random_u64(uint64_t *value) {
#if defined(_WIN32)
    return BCryptGenRandom(NULL, (PUCHAR)value, (ULONG)sizeof(*value), BCRYPT_USE_SYSTEM_PREFERRED_RNG) == 0 ? 0 : -1;
#elif defined(__APPLE__)
    arc4random_buf(value, sizeof(*value));
    return 0;
#else
    size_t offset = 0;
    while (offset < sizeof(*value)) {
        ssize_t count = getrandom((unsigned char *)value + offset, sizeof(*value) - offset, 0);
        if (count > 0) {
            offset += (size_t)count;
            continue;
        }
        if (count < 0 && errno == EINTR) continue;
        return -1;
    }
    return 0;
#endif
}

static int arena_identity(uint64_t *nonce, uint64_t *identity) {
    const uint64_t process = arena_process_id();
    for (;;) {
        uint64_t owner = atomic_load_explicit(&arena_nonce_process, memory_order_acquire);
        if (owner == process) break;
        if (owner == UINT64_MAX) {
            if (atomic_load_explicit(&arena_nonce_initializer_process, memory_order_acquire) != process) {
                uint64_t initializing = UINT64_MAX;
                (void)atomic_compare_exchange_strong_explicit(&arena_nonce_process, &initializing, 0,
                                                              memory_order_acq_rel, memory_order_acquire);
            }
            continue;
        }
        atomic_store_explicit(&arena_nonce_initializer_process, process, memory_order_release);
        if (atomic_compare_exchange_weak_explicit(&arena_nonce_process, &owner, UINT64_MAX, memory_order_acq_rel,
                                                  memory_order_acquire)) {
            uint64_t generated = 0;
            do {
                if (arena_random_u64(&generated) != 0) {
                    atomic_store_explicit(&arena_nonce_initializer_process, 0, memory_order_relaxed);
                    atomic_store_explicit(&arena_nonce_process, 0, memory_order_release);
                    return (errno = EIO, -1);
                }
            } while (generated == 0);
            atomic_store_explicit(&arena_process_nonce, generated, memory_order_relaxed);
            atomic_store_explicit(&arena_authority_sequence, 1, memory_order_relaxed);
            atomic_store_explicit(&arena_fork_sequence, 1, memory_order_relaxed);
            atomic_store_explicit(&arena_nonce_initializer_process, 0, memory_order_relaxed);
            atomic_store_explicit(&arena_nonce_process, process, memory_order_release);
            break;
        }
    }
    *nonce = atomic_load_explicit(&arena_process_nonce, memory_order_acquire);
    uint64_t current = atomic_load_explicit(&arena_authority_sequence, memory_order_relaxed);
    for (;;) {
        if (current == UINT64_MAX) return (errno = EOVERFLOW, -1);
        if (atomic_compare_exchange_weak_explicit(&arena_authority_sequence, &current, current + 1,
                                                  memory_order_relaxed, memory_order_relaxed)) {
            *identity = current;
            return 0;
        }
    }
}

static int arena_sequence_take(_Atomic uint64_t *sequence, uint64_t *value) {
    uint64_t current = atomic_load_explicit(sequence, memory_order_relaxed);
    for (;;) {
        if (current == UINT64_MAX) return (errno = EOVERFLOW, -1);
        if (atomic_compare_exchange_weak_explicit(sequence, &current, current + 1, memory_order_relaxed,
                                                  memory_order_relaxed)) {
            *value = current;
            return 0;
        }
    }
}

static uint64_t arena_nonce_derive(uint64_t nonce, uint64_t sequence) {
    uint64_t value = nonce ^ sequence ^ UINT64_C(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)) * UINT64_C(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)) * UINT64_C(0x94d049bb133111eb);
    value ^= value >> 31;
    return value == 0 ? UINT64_MAX : value;
}

static int arena_ready_lock(hl_arena_authority *authority) {
    if (atomic_load_explicit(&authority->lifecycle, memory_order_acquire) != HL_ARENA_READY)
        return (errno = EINVAL, -1);
    arena_lock(authority);
    if (atomic_load_explicit(&authority->lifecycle, memory_order_acquire) != HL_ARENA_READY) {
        arena_unlock(authority);
        return (errno = EINVAL, -1);
    }
    return 0;
}

static void arena_lock(hl_arena_authority *authority) {
    uint32_t unlocked = 0;
    while (!atomic_compare_exchange_weak_explicit(&authority->lock, &unlocked, 1, memory_order_acq_rel,
                                                  memory_order_relaxed))
        unlocked = 0;
}

uint64_t hl_arena_host_granule(void) {
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
    atomic_store_explicit(&authority->lock, 0, memory_order_release);
}

#if defined(HL_NATIVE_TEST_HOOKS)
void hl_arena_test_lock(hl_arena_authority *authority) {
    arena_lock(authority);
}

void hl_arena_test_unlock(hl_arena_authority *authority) {
    arena_unlock(authority);
}

void hl_arena_test_identity_sequence(uint64_t next) {
    atomic_store_explicit(&arena_authority_sequence, next, memory_order_relaxed);
}

void hl_arena_test_generation(hl_arena_authority *authority, uint64_t generation) {
    arena_lock(authority);
    authority->manifest.generation = generation;
    arena_unlock(authority);
}

void hl_arena_test_fail_next_placeholder_restore(void) {
    atomic_store_explicit(&arena_test_restore_failure, 1, memory_order_relaxed);
}
#endif

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
#if defined(_WIN32)
    (void)limit;
    (void)VirtualFree((void *)(uintptr_t)base, 0, MEM_RELEASE);
#elif defined(__APPLE__)
    uint64_t length = limit - base;
    (void)mach_vm_deallocate(mach_task_self(), (mach_vm_address_t)base, (mach_vm_size_t)length);
#else
    uint64_t length = limit - base;
    (void)munmap((void *)(uintptr_t)base, (size_t)length);
#endif
}

static int arena_materialize_anonymous(uint64_t address, uint64_t length, uint32_t protection) {
#if defined(_WIN32)
    typedef PVOID(WINAPI * virtual_alloc2_fn)(HANDLE, PVOID, SIZE_T, ULONG, ULONG, PVOID, ULONG);
    HMODULE kernel = GetModuleHandleW(L"kernelbase.dll");
    virtual_alloc2_fn virtual_alloc2 =
        kernel != NULL ? (virtual_alloc2_fn)(uintptr_t)GetProcAddress(kernel, "VirtualAlloc2") : NULL;
    DWORD native_protection;
    if (virtual_alloc2 == NULL) return (errno = ENOTSUP, -1);
    if (!VirtualFree((void *)(uintptr_t)address, (SIZE_T)length, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER))
        return (errno = EIO, -1);
    if ((protection & HL_ARENA_PROTECTION_EXECUTE) != 0)
        native_protection = (protection & HL_ARENA_PROTECTION_WRITE) != 0 ? PAGE_EXECUTE_READWRITE : PAGE_EXECUTE_READ;
    else
        native_protection = (protection & HL_ARENA_PROTECTION_WRITE) != 0 ? PAGE_READWRITE : PAGE_READONLY;
    void *mapped = virtual_alloc2(GetCurrentProcess(), (void *)(uintptr_t)address, (SIZE_T)length,
                                  MEM_RESERVE | MEM_COMMIT | MEM_REPLACE_PLACEHOLDER, native_protection, NULL, 0);
    if (mapped == (void *)(uintptr_t)address) return 0;
    return (errno = EIO, -1);
#elif defined(__APPLE__)
    vm_prot_t native_protection = VM_PROT_NONE;
    if ((protection & HL_ARENA_PROTECTION_READ) != 0) native_protection |= VM_PROT_READ;
    if ((protection & HL_ARENA_PROTECTION_WRITE) != 0) native_protection |= VM_PROT_WRITE;
    if ((protection & HL_ARENA_PROTECTION_EXECUTE) != 0) native_protection |= VM_PROT_EXECUTE;
    mach_vm_address_t mapped = (mach_vm_address_t)address;
    kern_return_t status =
        mach_vm_map(mach_task_self(), &mapped, (mach_vm_size_t)length, 0, VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE,
                    MEMORY_OBJECT_NULL, 0, FALSE, native_protection, native_protection, VM_INHERIT_COPY);
    return status == KERN_SUCCESS && mapped == (mach_vm_address_t)address ? 0 : (errno = EIO, -1);
#else
    int native_protection = 0;
    if ((protection & HL_ARENA_PROTECTION_READ) != 0) native_protection |= PROT_READ;
    if ((protection & HL_ARENA_PROTECTION_WRITE) != 0) native_protection |= PROT_WRITE;
    if ((protection & HL_ARENA_PROTECTION_EXECUTE) != 0) native_protection |= PROT_EXEC;
    void *mapped = mmap((void *)(uintptr_t)address, (size_t)length, native_protection,
                        MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    return mapped == (void *)(uintptr_t)address ? 0 : -1;
#endif
}

static int arena_placeholder_restore(uint64_t address, uint64_t length) {
#if defined(HL_NATIVE_TEST_HOOKS)
    if (atomic_exchange_explicit(&arena_test_restore_failure, 0, memory_order_relaxed) != 0) return (errno = EIO, -1);
#endif
#if defined(_WIN32)
    (void)length;
    return VirtualFree((void *)(uintptr_t)address, 0, MEM_RELEASE | MEM_PRESERVE_PLACEHOLDER) ? 0 : (errno = EIO, -1);
#elif defined(__APPLE__)
    mach_vm_address_t mapped = (mach_vm_address_t)address;
    kern_return_t status =
        mach_vm_map(mach_task_self(), &mapped, (mach_vm_size_t)length, 0, VM_FLAGS_FIXED | VM_FLAGS_OVERWRITE,
                    MEMORY_OBJECT_NULL, 0, FALSE, VM_PROT_NONE, VM_PROT_NONE, VM_INHERIT_COPY);
    return status == KERN_SUCCESS && mapped == (mach_vm_address_t)address ? 0 : (errno = EIO, -1);
#else
    void *mapped =
        mmap((void *)(uintptr_t)address, (size_t)length, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    return mapped == (void *)(uintptr_t)address ? 0 : -1;
#endif
}

static int aligned_range(uint64_t base, uint64_t limit, uint64_t granule) {
    return granule != 0 && (granule & (granule - 1)) == 0 && base < limit && base % granule == 0 &&
           limit % granule == 0;
}

int hl_arena_manifest_valid(const hl_arena_manifest *manifest) {
    uint64_t host_granule = hl_arena_host_granule();
    return manifest != NULL && manifest->magic == HL_ARENA_MANIFEST_MAGIC &&
           manifest->version == HL_ARENA_MANIFEST_VERSION && manifest->size == sizeof(*manifest) && host_granule != 0 &&
           manifest->granule >= host_granule && manifest->granule % host_granule == 0 &&
           aligned_range(manifest->normal_base, manifest->normal_limit, manifest->granule) &&
           aligned_range(manifest->low32_base, manifest->low32_limit, manifest->granule) &&
           manifest->normal_cursor >= manifest->normal_base && manifest->normal_cursor <= manifest->normal_limit &&
           manifest->low32_cursor >= manifest->low32_base && manifest->low32_cursor <= manifest->low32_limit &&
           manifest->normal_cursor % manifest->granule == 0 && manifest->low32_cursor % manifest->granule == 0 &&
           (manifest->normal_limit <= manifest->low32_base || manifest->low32_limit <= manifest->normal_base) &&
           manifest->low32_limit <= UINT64_C(0x100000000) && manifest->generation != 0 &&
           manifest->authority_nonce != 0 && manifest->authority_identity != 0 && manifest->next_identity != 0 &&
           manifest->reservation_count <= HL_ARENA_MAX_RESERVATIONS && manifest->reserved == 0;
}

int hl_arena_authority_init(hl_arena_authority *authority, const hl_arena_config *config) {
    uint64_t host_granule = hl_arena_host_granule();
    uint64_t nonce;
    uint64_t identity;
    if (authority == NULL || config == NULL || host_granule == 0 || config->granule < host_granule ||
        config->granule % host_granule != 0 ||
        !aligned_range(config->normal_base, config->normal_limit, config->granule) ||
        !aligned_range(config->low32_base, config->low32_limit, config->granule) ||
        !(config->normal_limit <= config->low32_base || config->low32_limit <= config->normal_base) ||
        config->low32_limit > UINT64_C(0x100000000)) {
        errno = EINVAL;
        return -1;
    }
    uint32_t empty = HL_ARENA_EMPTY;
    if (!atomic_compare_exchange_strong_explicit(&authority->lifecycle, &empty, HL_ARENA_INITIALIZING,
                                                 memory_order_acq_rel, memory_order_acquire))
        return (errno = EALREADY, -1);
    memset(&authority->manifest, 0, sizeof(authority->manifest));
    memset(authority->reservations, 0, sizeof(authority->reservations));
    authority->reservation_count = 0;
    authority->active_transaction = 0;
    authority->materialization_count = 0;
    authority->transaction_materialization_count = 0;
    memset(authority->materialized_identities, 0, sizeof(authority->materialized_identities));
    atomic_store_explicit(&authority->fork_phase, 0, memory_order_relaxed);
    authority->fork_process = 0;
    if (arena_claim(config->normal_base, config->normal_limit) != 0) {
        atomic_store_explicit(&authority->lifecycle, HL_ARENA_EMPTY, memory_order_release);
        return -1;
    }
    if (arena_claim(config->low32_base, config->low32_limit) != 0) {
        arena_release(config->normal_base, config->normal_limit);
        atomic_store_explicit(&authority->lifecycle, HL_ARENA_EMPTY, memory_order_release);
        return -1;
    }
    if (arena_identity(&nonce, &identity) != 0) {
        int identity_error = errno;
        /* These unpublished claims can still be released safely. */
        arena_release(config->low32_base, config->low32_limit);
        arena_release(config->normal_base, config->normal_limit);
        atomic_store_explicit(&authority->lifecycle, HL_ARENA_EMPTY, memory_order_release);
        errno = identity_error;
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
                                              nonce,
                                              identity,
                                              1,
                                              0,
                                              0};
    authority->claimed_normal_base = config->normal_base;
    authority->claimed_normal_limit = config->normal_limit;
    authority->claimed_low32_base = config->low32_base;
    authority->claimed_low32_limit = config->low32_limit;
    atomic_store_explicit(&authority->lifecycle, HL_ARENA_READY, memory_order_release);
    return 0;
}

int hl_arena_authority_destroy(hl_arena_authority *authority) {
    if (authority == NULL) return (errno = EINVAL, -1);
    if (arena_ready_lock(authority) != 0) return -1;
    if (authority->active_transaction) {
        arena_unlock(authority);
        return (errno = EBUSY, -1);
    }
    atomic_store_explicit(&authority->lifecycle, HL_ARENA_RETIRED, memory_order_release);
    authority->reservation_count = 0;
    authority->active_transaction = 0;
    authority->materialization_count = 0;
    authority->transaction_materialization_count = 0;
    memset(&authority->manifest, 0, sizeof(authority->manifest));
    memset(authority->reservations, 0, sizeof(authority->reservations));
    memset(authority->materialized_identities, 0, sizeof(authority->materialized_identities));
    arena_unlock(authority);
    return 0;
}

int hl_arena_authority_fork_prepare(hl_arena_authority *authority) {
    uint32_t idle = 0;
    if (authority == NULL) return (errno = EINVAL, -1);
    if (!atomic_compare_exchange_strong_explicit(&authority->fork_phase, &idle, 1, memory_order_acq_rel,
                                                 memory_order_acquire))
        return (errno = EALREADY, -1);
    if (arena_ready_lock(authority) != 0) {
        atomic_store_explicit(&authority->fork_phase, 0, memory_order_release);
        return -1;
    }
    if (authority->active_transaction &&
        authority->materialization_count != authority->transaction_materialization_count) {
        arena_unlock(authority);
        atomic_store_explicit(&authority->fork_phase, 0, memory_order_release);
        return (errno = EBUSY, -1);
    }
    authority->fork_process = arena_process_id();
    atomic_store_explicit(&authority->fork_phase, 2, memory_order_release);
    return 0;
}

int hl_arena_authority_fork_parent(hl_arena_authority *authority) {
    if (authority == NULL || atomic_load_explicit(&authority->fork_phase, memory_order_acquire) != 2 ||
        authority->fork_process != arena_process_id())
        return (errno = EINVAL, -1);
    authority->fork_process = 0;
    atomic_store_explicit(&authority->fork_phase, 0, memory_order_release);
    arena_unlock(authority);
    return 0;
}

int hl_arena_authority_fork_child(hl_arena_authority *authority) {
    int result = 0;
    if (authority == NULL) return (errno = EINVAL, -1);
    /* fork_prepare left this lock held by the calling thread. The child owns
     * the copied lock and journal, so recovery is allocation-free and observes
     * a state that was quiescent at fork. */
    if (atomic_load_explicit(&authority->fork_phase, memory_order_acquire) != 2 ||
        authority->fork_process == arena_process_id())
        return (errno = EINVAL, -1);
    if (atomic_load_explicit(&authority->lifecycle, memory_order_acquire) != HL_ARENA_READY) {
        authority->fork_process = 0;
        atomic_store_explicit(&authority->fork_phase, 0, memory_order_release);
        arena_unlock(authority);
        return (errno = EINVAL, -1);
    }
    if (authority->active_transaction) {
        authority->manifest.normal_cursor = authority->transaction_normal_cursor;
        authority->manifest.low32_cursor = authority->transaction_low32_cursor;
        memset(&authority->reservations[authority->transaction_reservation_count], 0,
               (authority->reservation_count - authority->transaction_reservation_count) *
                   sizeof(hl_arena_reservation));
        authority->reservation_count = authority->transaction_reservation_count;
        authority->manifest.reservation_count = authority->reservation_count;
        authority->active_transaction = 0;
        if (authority->manifest.generation == UINT64_MAX) {
            result = -1;
            errno = EOVERFLOW;
        } else {
            authority->manifest.generation++;
        }
    }
    authority->fork_process = 0;
    atomic_store_explicit(&authority->fork_phase, 0, memory_order_release);
    arena_unlock(authority);
    return result;
}

int hl_arena_fork_context_prepare(hl_arena_fork_context *context) {
    uint64_t sequence;
    uint64_t nonce;
    if (context == NULL) return (errno = EINVAL, -1);
    memset(context, 0, sizeof(*context));
    if (atomic_load_explicit(&arena_nonce_process, memory_order_acquire) != arena_process_id())
        return (errno = EINVAL, -1);
    nonce = atomic_load_explicit(&arena_process_nonce, memory_order_acquire);
    if (nonce == 0 || arena_sequence_take(&arena_fork_sequence, &sequence) != 0) return -1;
    context->parent_process = arena_process_id();
    context->child_nonce = arena_nonce_derive(nonce, sequence);
    context->active = 1;
    return 0;
}

int hl_arena_fork_context_parent(hl_arena_fork_context *context) {
    if (context == NULL || !context->active || context->parent_process != arena_process_id())
        return (errno = EINVAL, -1);
    context->active = 0;
    return 0;
}

int hl_arena_after_fork_child(hl_arena_fork_context *context) {
    uint64_t process = arena_process_id();
    if (context == NULL || !context->active || context->parent_process == process || context->child_nonce == 0)
        return (errno = EINVAL, -1);
    atomic_store_explicit(&arena_process_nonce, context->child_nonce, memory_order_relaxed);
    atomic_store_explicit(&arena_authority_sequence, 1, memory_order_relaxed);
    atomic_store_explicit(&arena_fork_sequence, 1, memory_order_relaxed);
    atomic_store_explicit(&arena_nonce_initializer_process, 0, memory_order_relaxed);
    atomic_store_explicit(&arena_nonce_process, process, memory_order_release);
    context->active = 0;
    return 0;
}

int hl_arena_manifest_get(hl_arena_authority *authority, hl_arena_manifest *manifest) {
    if (authority == NULL || manifest == NULL) return (errno = EINVAL, -1);
    if (arena_ready_lock(authority) != 0) return -1;
    if (authority->active_transaction) {
        arena_unlock(authority);
        return (errno = EBUSY, -1);
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
            reservation->state != HL_ARENA_RESERVATION_OWNED ||
            reservation->authority_nonce != state->manifest.authority_nonce ||
            reservation->authority_identity != state->manifest.authority_identity || reservation->identity == 0 ||
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
    for (uint32_t index = state->manifest.reservation_count; index < HL_ARENA_MAX_RESERVATIONS; ++index) {
        const unsigned char *bytes = (const unsigned char *)&state->reservations[index];
        for (size_t offset = 0; offset < sizeof(state->reservations[index]); ++offset)
            if (bytes[offset] != 0) return 0;
    }
    return normal_cursor == state->manifest.normal_cursor && low32_cursor == state->manifest.low32_cursor;
}

int hl_arena_persisted_state_get(hl_arena_authority *authority, hl_arena_persisted_state *state) {
    if (authority == NULL || state == NULL) return (errno = EINVAL, -1);
    if (arena_ready_lock(authority) != 0) return -1;
    if (authority->active_transaction) {
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
    if (arena_ready_lock(authority) != 0) return -1;
    if (!hl_arena_manifest_valid(&authority->manifest) || authority->active_transaction != 0) {
        arena_unlock(authority);
        return (errno = EBUSY, -1);
    }
    if (authority->manifest.generation == UINT64_MAX) {
        arena_unlock(authority);
        return (errno = EOVERFLOW, -1);
    }
    authority->transaction_normal_cursor = authority->manifest.normal_cursor;
    authority->transaction_low32_cursor = authority->manifest.low32_cursor;
    authority->transaction_reservation_count = authority->reservation_count;
    authority->transaction_materialization_count = authority->materialization_count;
    authority->manifest.generation++;
    /* Publish the active flag last. A fork child that observes it is then
     * guaranteed to observe a complete rollback journal. */
    authority->active_transaction = 1;
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
    if (arena_ready_lock(authority) != 0) return -1;
    if (!authority->active_transaction || transaction->generation != authority->manifest.generation || length == 0) {
        arena_unlock(authority);
        return (errno = EINVAL, -1);
    }
    if (length > UINT64_MAX - (authority->manifest.granule - 1)) {
        arena_unlock(authority);
        return (errno = EOVERFLOW, -1);
    }
    if (authority->reservation_count == HL_ARENA_MAX_RESERVATIONS) {
        arena_unlock(authority);
        return (errno = ENOSPC, -1);
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
    *reservation = (hl_arena_reservation){authority->manifest.authority_nonce,
                                          authority->manifest.authority_identity,
                                          authority->manifest.next_identity++,
                                          *cursor,
                                          rounded,
                                          (uint32_t)zone,
                                          HL_ARENA_RESERVATION_OWNED};
    authority->reservations[authority->reservation_count++] = *reservation;
    authority->manifest.reservation_count = authority->reservation_count;
    *cursor += rounded;
    arena_unlock(authority);
    return 0;
}

int hl_arena_transaction_materialize_anonymous(hl_arena_transaction *transaction,
                                               const hl_arena_reservation *reservation, uint32_t protection) {
    hl_arena_authority *authority;
    int found = 0;
    const uint32_t allowed = HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE | HL_ARENA_PROTECTION_EXECUTE;
    if (transaction == NULL || reservation == NULL || !transaction->active || (protection & ~allowed) != 0 ||
        (protection & HL_ARENA_PROTECTION_READ) == 0)
        return (errno = EINVAL, -1);
    authority = transaction->authority;
    if (arena_ready_lock(authority) != 0) return -1;
    if (!authority->active_transaction || transaction->generation != authority->manifest.generation) {
        arena_unlock(authority);
        return (errno = EINVAL, -1);
    }
    for (uint32_t index = 0; index < authority->reservation_count; ++index) {
        const hl_arena_reservation *candidate = &authority->reservations[index];
        if (candidate->authority_nonce == reservation->authority_nonce &&
            candidate->authority_identity == reservation->authority_identity &&
            candidate->identity == reservation->identity && candidate->address == reservation->address &&
            candidate->length == reservation->length && candidate->zone == reservation->zone &&
            candidate->state == HL_ARENA_RESERVATION_OWNED) {
            found = 1;
            break;
        }
    }
    if (!found) {
        arena_unlock(authority);
        return (errno = EACCES, -1);
    }
    for (uint32_t index = 0; index < authority->materialization_count; ++index) {
        if (authority->materialized_identities[index] == reservation->identity) {
            arena_unlock(authority);
            return (errno = EALREADY, -1);
        }
    }
    if (authority->materialization_count == HL_ARENA_MAX_RESERVATIONS) {
        arena_unlock(authority);
        return (errno = ENOSPC, -1);
    }
    /* The authority lock spans validation and the platform replacement. No
     * caller can obtain an ownership answer and race a separate MAP_FIXED. */
    if (arena_materialize_anonymous(reservation->address, reservation->length, protection) != 0) {
        arena_unlock(authority);
        return -1;
    }
    authority->materialized_identities[authority->materialization_count++] = reservation->identity;
    arena_unlock(authority);
    return 0;
}

int hl_arena_transaction_commit(hl_arena_transaction *transaction) {
    if (transaction == NULL || !transaction->active) return (errno = EINVAL, -1);
    if (arena_ready_lock(transaction->authority) != 0) return -1;
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

int hl_arena_transaction_rollback(hl_arena_transaction *transaction) {
    hl_arena_authority *authority;
    int rollback_error = 0;
    if (transaction == NULL || !transaction->active) return (errno = EINVAL, -1);
    authority = transaction->authority;
    if (arena_ready_lock(authority) != 0) {
        transaction->active = 0;
        return -1;
    }
    if (!authority->active_transaction || transaction->generation != authority->manifest.generation) {
        arena_unlock(authority);
        return (errno = EINVAL, -1);
    }
    for (uint32_t index = authority->materialization_count; index > authority->transaction_materialization_count;
         --index) {
        const uint64_t identity = authority->materialized_identities[index - 1];
        for (uint32_t reservation_index = 0; reservation_index < authority->reservation_count; ++reservation_index) {
            const hl_arena_reservation *reservation = &authority->reservations[reservation_index];
            if (reservation->identity == identity &&
                arena_placeholder_restore(reservation->address, reservation->length) != 0 && rollback_error == 0)
                rollback_error = errno != 0 ? errno : EIO;
        }
        authority->materialized_identities[index - 1] = 0;
    }
    authority->materialization_count = authority->transaction_materialization_count;
    authority->manifest.normal_cursor = authority->transaction_normal_cursor;
    authority->manifest.low32_cursor = authority->transaction_low32_cursor;
    memset(&authority->reservations[authority->transaction_reservation_count], 0,
           (authority->reservation_count - authority->transaction_reservation_count) * sizeof(hl_arena_reservation));
    authority->reservation_count = authority->transaction_reservation_count;
    authority->manifest.reservation_count = authority->reservation_count;
    authority->active_transaction = 0;
    transaction->active = 0;
    if (rollback_error != 0) atomic_store_explicit(&authority->lifecycle, HL_ARENA_RETIRED, memory_order_release);
    arena_unlock(authority);
    if (rollback_error != 0) return (errno = rollback_error, -1);
    return 0;
}

int hl_arena_reservation_owned(hl_arena_authority *authority, const hl_arena_reservation *reservation) {
    int owned = 0;
    if (authority == NULL || reservation == NULL || reservation->identity == 0) return 0;
    if (arena_ready_lock(authority) != 0) return 0;
    if (authority->active_transaction) {
        arena_unlock(authority);
        return 0;
    }
    for (uint32_t index = 0; index < authority->reservation_count; ++index) {
        const hl_arena_reservation *candidate = &authority->reservations[index];
        if (candidate->authority_nonce == reservation->authority_nonce &&
            candidate->authority_nonce == authority->manifest.authority_nonce &&
            candidate->authority_identity == reservation->authority_identity &&
            candidate->authority_identity == authority->manifest.authority_identity &&
            candidate->identity == reservation->identity && candidate->address == reservation->address &&
            candidate->length == reservation->length && candidate->zone == reservation->zone &&
            candidate->state == HL_ARENA_RESERVATION_OWNED) {
            owned = 1;
            break;
        }
    }
    arena_unlock(authority);
    return owned;
}
