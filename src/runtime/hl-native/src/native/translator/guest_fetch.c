#include "guest_fetch.h"

#include "guest_memory.h"

#include <errno.h>
#include <stdatomic.h>
#include <string.h>

static hl_guest_fetch_direct_validator g_direct_validator;
static const _Atomic uint64_t *g_direct_generation;
static _Atomic uint64_t g_decode_authority = HL_GUEST_FETCH_AUTHORITY_VERSION_ONE;

void hl_guest_fetch_set_direct_validator(hl_guest_fetch_direct_validator validator) {
    g_direct_validator = validator;
}

void hl_guest_fetch_set_direct_generation(const _Atomic uint64_t *generation) {
    g_direct_generation = generation;
}

const _Atomic uint64_t *hl_guest_fetch_authority_source(void) { return &g_decode_authority; }

static int authority_begin_observed(_Atomic uint64_t *authority, _Atomic int *registered) {
    uint64_t state = atomic_load_explicit(authority, memory_order_relaxed);
    for (;;) {
        if (state & HL_GUEST_FETCH_AUTHORITY_DISABLED) return 0;
        uint64_t version = state & ~(HL_GUEST_FETCH_AUTHORITY_DISABLED | HL_GUEST_FETCH_AUTHORITY_READER_MASK |
                                     HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK);
        uint64_t active = state & HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK;
        uint64_t readers = state & HL_GUEST_FETCH_AUTHORITY_READER_MASK;
        uint64_t next;
        int begun;
        if (active == HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK ||
            version == (HL_GUEST_FETCH_AUTHORITY_DISABLED - HL_GUEST_FETCH_AUTHORITY_VERSION_ONE)) {
            next = state | HL_GUEST_FETCH_AUTHORITY_DISABLED;
            begun = 0;
        } else {
            next = readers | (version + HL_GUEST_FETCH_AUTHORITY_VERSION_ONE) | (active + 1);
            begun = 1;
        }
        if (atomic_compare_exchange_weak_explicit(authority, &state, next,
                                                  memory_order_acq_rel, memory_order_relaxed)) {
            if (begun) {
                if (registered != NULL) atomic_store_explicit(registered, 1, memory_order_release);
                while (atomic_load_explicit(authority, memory_order_acquire) &
                       HL_GUEST_FETCH_AUTHORITY_READER_MASK) {}
            }
            return begun;
        }
    }
}

static int authority_begin(_Atomic uint64_t *authority) {
    return authority_begin_observed(authority, NULL);
}

static void authority_end(_Atomic uint64_t *authority, int begun) {
    if (!begun) return;
    uint64_t state = atomic_load_explicit(authority, memory_order_relaxed);
    for (;;) {
        if (state & HL_GUEST_FETCH_AUTHORITY_DISABLED) return;
        uint64_t version = state & ~(HL_GUEST_FETCH_AUTHORITY_DISABLED | HL_GUEST_FETCH_AUTHORITY_READER_MASK |
                                     HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK);
        uint64_t active = state & HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK;
        uint64_t readers = state & HL_GUEST_FETCH_AUTHORITY_READER_MASK;
        uint64_t next = active == 0 ||
                                version == (HL_GUEST_FETCH_AUTHORITY_DISABLED - HL_GUEST_FETCH_AUTHORITY_VERSION_ONE)
                            ? state | HL_GUEST_FETCH_AUTHORITY_DISABLED
                            : readers | (version + HL_GUEST_FETCH_AUTHORITY_VERSION_ONE) | (active - 1);
        /* The final overlapping writer must acquire the prior writer's release
           before it publishes active=0, forming one release sequence for every
           mapping payload covered by this authority state. */
        if (atomic_compare_exchange_weak_explicit(authority, &state, next,
                                                  memory_order_acq_rel, memory_order_relaxed))
            return;
    }
}

int hl_guest_fetch_authority_begin(void) { return authority_begin(&g_decode_authority); }

void hl_guest_fetch_authority_end(int begun) { authority_end(&g_decode_authority, begun); }

void hl_guest_fetch_authority_disable(void) {
    int begun = authority_begin(&g_decode_authority);
    if (begun)
        atomic_fetch_or_explicit(&g_decode_authority, HL_GUEST_FETCH_AUTHORITY_DISABLED, memory_order_release);
}

static int authority_lease(_Atomic uint64_t *source, uint64_t authority) {
    uint64_t state = atomic_load_explicit(source, memory_order_acquire);
    for (;;) {
        uint64_t token = state & ~HL_GUEST_FETCH_AUTHORITY_READER_MASK;
        uint64_t readers = state & HL_GUEST_FETCH_AUTHORITY_READER_MASK;
        if (token != authority || (token & (HL_GUEST_FETCH_AUTHORITY_DISABLED |
                                            HL_GUEST_FETCH_AUTHORITY_ACTIVE_MASK)) ||
            readers == HL_GUEST_FETCH_AUTHORITY_READER_MASK)
            return 0;
        uint64_t next = state + HL_GUEST_FETCH_AUTHORITY_READER_ONE;
        if (atomic_compare_exchange_weak_explicit(source, &state, next,
                                                  memory_order_acquire, memory_order_relaxed))
            return 1;
    }
}

static void authority_unlease(_Atomic uint64_t *source) {
    uint64_t state = atomic_load_explicit(source, memory_order_relaxed);
    for (;;) {
        uint64_t readers = state & HL_GUEST_FETCH_AUTHORITY_READER_MASK;
        if (readers == 0 || (state & HL_GUEST_FETCH_AUTHORITY_DISABLED)) return;
        uint64_t next = state - HL_GUEST_FETCH_AUTHORITY_READER_ONE;
        if (atomic_compare_exchange_weak_explicit(source, &state, next,
                                                  memory_order_acq_rel, memory_order_relaxed))
            return;
    }
}

int hl_guest_fetch_authority_lease(uint64_t authority) {
    return authority_lease(&g_decode_authority, authority);
}

void hl_guest_fetch_authority_unlease(void) { authority_unlease(&g_decode_authority); }

void hl_guest_fetch_authority_after_fork_child(void) {
    atomic_store_explicit(&g_decode_authority, HL_GUEST_FETCH_AUTHORITY_DISABLED, memory_order_release);
}

#if defined(HL_NATIVE_TEST_HOOKS)
int hl_guest_fetch_authority_test_begin(_Atomic uint64_t *authority) { return authority_begin(authority); }
int hl_guest_fetch_authority_test_begin_observed(_Atomic uint64_t *authority, _Atomic int *registered) {
    return authority_begin_observed(authority, registered);
}
int hl_guest_fetch_authority_test_global_begin_observed(_Atomic int *registered) {
    return authority_begin_observed(&g_decode_authority, registered);
}
void hl_guest_fetch_authority_test_end(_Atomic uint64_t *authority, int begun) { authority_end(authority, begun); }
int hl_guest_fetch_authority_test_lease(_Atomic uint64_t *authority, uint64_t token) {
    return authority_lease(authority, token);
}
void hl_guest_fetch_authority_test_unlease(_Atomic uint64_t *authority) { authority_unlease(authority); }
void hl_guest_fetch_authority_test_after_fork_child(_Atomic uint64_t *authority) {
    atomic_store_explicit(authority, HL_GUEST_FETCH_AUTHORITY_DISABLED, memory_order_release);
}
#endif

/*
 * One resolved executable mapping, remembered per thread.
 *
 * It caches a MAPPING, never guest bytes: every execution still copies the
 * instruction out of guest memory below, so self-modifying code stays coherent
 * by construction.  That is the property the whole design rests on.
 *
 * Invalidation is by revalidation rather than notification, and the set is
 * complete for that reason.  `generation` is re-read from the ledger on every
 * hit, and every publication of a logical-VMA snapshot bumps it -- map, unmap,
 * protect, plan commit, reset, destroy -- so no mapping transition has to know
 * this memo exists.  A snapshot POINTER would not do: a retired snapshot is
 * freed at the next quiescent reclaim and malloc can hand the same address back
 * to the next publication (ABA), which would make a stale entry look fresh.
 *
 * The ordinary/direct verdict carries its own independent generation below.
 * Guest munmap, MAP_FIXED, mremap and mprotect(PROT_NONE) over ordinary memory
 * do not necessarily touch the logical ledger, so a mapping-span hit may reuse
 * execute validity only while that second authority is unchanged.
 */
typedef hl_guest_fetch_context fetch_span;

static _Thread_local fetch_span g_span;
hl_guest_fetch_context *hl_guest_fetch_context_current(void) { return &g_span; }

/* The whole point: a hit is two compares and one load, with no call. */
static fetch_span *span_hit(fetch_span *context, uint64_t guest) {
    const _Atomic uint64_t *generation = hl_guest_memory_generation;
    if (generation == NULL || guest < context->first || guest >= context->last) return NULL;
    return context->generation == atomic_load_explicit(generation, memory_order_acquire) ? context : NULL;
}

static fetch_span *span_for(fetch_span *context, uint64_t guest, size_t length, fetch_span *scratch) {
    fetch_span *hit = span_hit(context, guest);
    if (hit != NULL) return hit;
    uint64_t resolved = 0, first = 0, last = 0, delta = 0;
    int resolution = hl_guest_memory_resolve_exec_span(guest, length, &resolved, &first, &last, &delta);
    if (resolution < 0) return NULL;
    *scratch = (fetch_span){.generation = resolved, .first = first, .last = last, .delta = delta,
                            .indirect = resolution > 0};
    if (hl_guest_memory_generation != NULL) {
        *context = *scratch;
        return context;
    }
    return scratch;
}

/*
 * Nothing is read before it is proven.  A logical VMA followed by an unmapped
 * logical hole resolves as "ordinary" from inside the hole, and treating that
 * as a direct host VA would crash the translator.
 */
static int chunk_valid(uint64_t guest, fetch_span *span, size_t chunk) {
    if (span->indirect) {
        if (span->last - guest >= (uint64_t)chunk) return 1;
        errno = EFAULT; /* the VMA ends inside this chunk: not one executable mapping */
        return 0;
    }
    if (g_direct_validator == NULL) return 1;
    uint64_t end = guest + chunk;
    const _Atomic uint64_t *authority = g_direct_generation;
    if (authority != NULL) {
        uint64_t generation = atomic_load_explicit(authority, memory_order_acquire);
        if (!(generation & 1) && span->direct_generation == generation && guest >= span->direct_first &&
            end <= span->direct_last)
            return 1;
        uint64_t first = guest & ~UINT64_C(4095);
        if (first <= UINT64_MAX - UINT64_C(4096) && g_direct_validator(first, 4096)) {
            uint64_t confirmed = atomic_load_explicit(authority, memory_order_acquire);
            if (confirmed == generation && !(confirmed & 1)) {
                span->direct_generation = confirmed;
                span->direct_first = first;
                span->direct_last = first + UINT64_C(4096);
                return 1;
            }
        }
    }
    if (g_direct_validator(guest, chunk)) return 1;
    errno = EFAULT;
    return 0;
}

/* Validate and copy in one pass, page-chunked because the direct validator's
   verdict is only meaningful per page.  output == NULL validates only. */
static int fetch_walk(fetch_span *context, uint64_t guest, unsigned char *output, size_t length) {
    while (length != 0) {
        size_t page_left = (size_t)(4096u - (guest & UINT64_C(4095)));
        size_t chunk = length < page_left ? length : page_left;
        fetch_span scratch;
        fetch_span *span = span_for(context, guest, chunk, &scratch);
        if (span == NULL || !chunk_valid(guest, span, chunk)) return -1;
        if (output != NULL) {
            memcpy(output, (const void *)(uintptr_t)(guest + span->delta), chunk);
            output += chunk;
        }
        guest += chunk;
        length -= chunk;
    }
    return 0;
}

int hl_guest_fetch_exec_context(fetch_span *context, uint64_t guest, void *destination, size_t length) {
    if (length != 0 && guest > UINT64_MAX - length) {
        errno = EFAULT;
        return -1;
    }
    /* One page, one cached mapping: the shape of every guest instruction fetch
       that is not straddling something.  Nothing to stage, so copy in place. */
    if (length <= 4096u - (size_t)(guest & UINT64_C(4095))) {
        fetch_span *span = span_hit(context, guest);
        if (span != NULL) {
            if (!chunk_valid(guest, span, length)) return -1;
            memcpy(destination, (const void *)(uintptr_t)(guest + span->delta), length);
            return 0;
        }
    }
    /* Otherwise stage, so a fault in a later chunk cannot leave the caller's
       buffer half written; every in-tree fetch is an instruction or a 64-byte
       code line and fits. */
    unsigned char staging[64];
    if (length <= sizeof staging) {
        if (fetch_walk(context, guest, staging, length) != 0) return -1;
        memcpy(destination, staging, length);
        return 0;
    }
    if (fetch_walk(context, guest, NULL, length) != 0) return -1;
    return fetch_walk(context, guest, destination, length);
}

int hl_guest_fetch_exec(uint64_t guest, void *destination, size_t length) {
    return hl_guest_fetch_exec_context(&g_span, guest, destination, length);
}

int hl_guest_fetch_u32(uint64_t guest, uint32_t *instruction) {
    return hl_guest_fetch_exec(guest, instruction, sizeof(*instruction));
}

/*
 * decode.c supplies a weak direct-address default so its standalone unit
 * tests remain independent of the Linux ABI.  Engine links include this
 * strong definition and therefore route every decoder (translator, trace
 * lookahead, and C AVX fallback) through the same executable-byte reader.
 */
