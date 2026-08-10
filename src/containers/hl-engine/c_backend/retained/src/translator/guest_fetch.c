#include "guest_fetch.h"

#include "guest_memory.h"

#include <errno.h>
#include <stdatomic.h>
#include <string.h>

static hl_guest_fetch_direct_validator g_direct_validator;

void hl_guest_fetch_set_direct_validator(hl_guest_fetch_direct_validator validator) {
    g_direct_validator = validator;
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
 * The ordinary/direct verdict is deliberately NOT cached.  Guest munmap,
 * MAP_FIXED, mremap and mprotect(PROT_NONE) over ordinary memory change host
 * mappings without touching the ledger, so the direct validator still runs on
 * every fetch and those paths need no hook (G_SMC_UNMAP included).
 */
typedef struct {
    uint64_t generation;
    uint64_t first; /* half-open guest interval over which the resolution holds */
    uint64_t last;
    uint64_t delta; /* host = guest + delta; zero for the ordinary case */
    int indirect;
} fetch_span;

static _Thread_local fetch_span g_span;

/* The whole point: a hit is two compares and one load, with no call. */
static const fetch_span *span_hit(uint64_t guest) {
    const _Atomic uint64_t *generation = hl_guest_memory_generation;
    if (generation == NULL || guest < g_span.first || guest >= g_span.last) return NULL;
    return g_span.generation == atomic_load_explicit(generation, memory_order_acquire) ? &g_span : NULL;
}

static const fetch_span *span_for(uint64_t guest, size_t length, fetch_span *scratch) {
    const fetch_span *hit = span_hit(guest);
    if (hit != NULL) return hit;
    uint64_t resolved = 0, first = 0, last = 0, delta = 0;
    int resolution = hl_guest_memory_resolve_exec_span(guest, length, &resolved, &first, &last, &delta);
    if (resolution < 0) return NULL;
    *scratch = (fetch_span){resolved, first, last, delta, resolution > 0};
    if (hl_guest_memory_generation != NULL) g_span = *scratch;
    return scratch;
}

/*
 * Nothing is read before it is proven.  A logical VMA followed by an unmapped
 * logical hole resolves as "ordinary" from inside the hole, and treating that
 * as a direct host VA would crash the translator.
 */
static int chunk_valid(uint64_t guest, const fetch_span *span, size_t chunk) {
    if (span->indirect) {
        if (span->last - guest >= (uint64_t)chunk) return 1;
        errno = EFAULT; /* the VMA ends inside this chunk: not one executable mapping */
        return 0;
    }
    if (g_direct_validator == NULL || g_direct_validator(guest, chunk)) return 1;
    errno = EFAULT;
    return 0;
}

/* Validate and copy in one pass, page-chunked because the direct validator's
   verdict is only meaningful per page.  output == NULL validates only. */
static int fetch_walk(uint64_t guest, unsigned char *output, size_t length) {
    while (length != 0) {
        size_t page_left = (size_t)(4096u - (guest & UINT64_C(4095)));
        size_t chunk = length < page_left ? length : page_left;
        fetch_span scratch;
        const fetch_span *span = span_for(guest, chunk, &scratch);
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

int hl_guest_fetch_exec(uint64_t guest, void *destination, size_t length) {
    if (length != 0 && guest > UINT64_MAX - length) {
        errno = EFAULT;
        return -1;
    }
    /* One page, one cached mapping: the shape of every guest instruction fetch
       that is not straddling something.  Nothing to stage, so copy in place. */
    if (length <= 4096u - (size_t)(guest & UINT64_C(4095))) {
        const fetch_span *span = span_hit(guest);
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
        if (fetch_walk(guest, staging, length) != 0) return -1;
        memcpy(destination, staging, length);
        return 0;
    }
    if (fetch_walk(guest, NULL, length) != 0) return -1;
    return fetch_walk(guest, destination, length);
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
