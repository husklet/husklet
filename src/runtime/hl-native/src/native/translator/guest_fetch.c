#include "guest_fetch.h"

#include "guest_memory.h"

#include <errno.h>
#include <stdatomic.h>
#include <string.h>

static hl_guest_fetch_direct_validator g_direct_validator;
static const _Atomic uint64_t *g_direct_generation;
static hl_guest_fetch_authority g_decode_authority = {.started = 1, .completed = 1};

void hl_guest_fetch_set_direct_validator(hl_guest_fetch_direct_validator validator) {
    g_direct_validator = validator;
}

void hl_guest_fetch_set_direct_generation(const _Atomic uint64_t *generation) {
    g_direct_generation = generation;
}

const hl_guest_fetch_authority *hl_guest_fetch_authority_source(void) { return &g_decode_authority; }

int hl_guest_fetch_authority_begin(void) {
    uint64_t started = atomic_load_explicit(&g_decode_authority.started, memory_order_seq_cst);
    for (;;) {
        if (started >> 63) return 0;
        uint64_t next = started + 1;
        if ((next & (UINT64_MAX >> 1)) == 0) next = UINT64_C(1) << 63;
        if (atomic_compare_exchange_weak_explicit(&g_decode_authority.started, &started, next,
                                                  memory_order_seq_cst, memory_order_seq_cst))
            return (next >> 63) == 0;
    }
}

void hl_guest_fetch_authority_end(int begun) {
    if (begun) atomic_fetch_add_explicit(&g_decode_authority.completed, 1, memory_order_seq_cst);
}

void hl_guest_fetch_authority_disable(void) {
    (void)hl_guest_fetch_authority_begin();
}

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
