// Extracted from service(): Memory — mmap/brk/mprotect/madvise syscalls. Returns 1 if nr was handled, 0 otherwise.
// Included by service.c after service/helpers.c, before service() — same TU scope (globals + helpers).
#include "../../page.h"
#include "../../logical_vma.h"

// process_vm_readv/writev between two iovec arrays. In this single-address-space DBT the "remote"
// process is always the guest itself, so both vectors point into directly-dereferenceable guest memory
// and the transfer is a scatter/gather memcpy -- exactly the kernel's stream semantics: bytes flow from
// the src vectors into the dst vectors in order, stopping when either side is exhausted. Returns the
// number of bytes copied.
// Is guest range [a,a+len) inaccessible to a userspace read/write, the way the Linux kernel would EFAULT?
// TWO cases hl must catch: (1) not mapped at all, and (2) mapped but PROT_NONE — hl force-maps guest anon
// memory host-RW so mprotect can stay a near-noop, which discards the guest's PROT_NONE intent, so a guest
// guard page (LTP tst_get_bad_addr = mmap(PROT_NONE)) stays host-readable. Both are handled by
// host_range_mapped: it rejects the wrap/unmapped case AND queries the single PROT_NONE registry g_gna
// (gna_hit, thread.c; fed by mmap/mprotect/munmap) up front. ONE helper so connect/bind/pselect/ppoll/...
// all agree (consolidates three agents' duplicates).
static int guest_bad_ptr(uintptr_t a, size_t len) {
    // The guest-facing predicate, so it answers about a GUEST address: fold to the storage the probe has to
    // touch (thread.c's rule), exactly as guest_span does. host_range_mapped keeps its host-address contract.
    return !host_range_mapped((uintptr_t)nonpie_fold((uint64_t)a), len);
}

static ssize_t svc_vm_iov_copy(const struct iovec *dst, unsigned long dcnt, const struct iovec *src,
                               unsigned long scnt) {
    ssize_t total = 0;
    unsigned long di = 0, si = 0;
    size_t doff = 0, soff = 0;
    while (di < dcnt && si < scnt) {
        size_t drem = dst[di].iov_len - doff, srem = src[si].iov_len - soff;
        size_t n = drem < srem ? drem : srem;
        if (n) {
            // Either endpoint may be an unmapped/straddling guest buffer. A raw memcpy would fault the ENGINE
            // (a guest-crashes-engine isolation break); mirror the kernel's copy_{to,from}_user instead --
            // stop at the first inaccessible byte, returning the bytes already transferred, or -EFAULT if none.
            uint64_t dg = (uint64_t)(uintptr_t)dst[di].iov_base + doff;
            uint64_t sg = (uint64_t)(uintptr_t)src[si].iov_base + soff;
            size_t moved = 0;
            while (moved < n) {
                void *dp, *sp;
                size_t da, sa;
                hl_logical_vma_pin dpin = {0}, spin = {0};
                if (guest_span(dg + moved, n - moved, HL_LOGICAL_VMA_WRITE, &dp, &da, &dpin) < 0 ||
                    guest_span(sg + moved, n - moved, HL_LOGICAL_VMA_READ, &sp, &sa, &spin) < 0) {
                    hl_logical_vma_unpin(&dpin);
                    hl_logical_vma_unpin(&spin);
                    return total > 0 ? total : -EFAULT;
                }
                size_t chunk = da < sa ? da : sa;
                memcpy(dp, sp, chunk);
                guest_smc_copyout(&dpin, dg + moved, chunk);
                hl_logical_vma_unpin(&dpin);
                hl_logical_vma_unpin(&spin);
                moved += chunk;
                total += (ssize_t)chunk;
                doff += chunk;
                soff += chunk;
            }
        }
        if (doff == dst[di].iov_len) di++, doff = 0;
        if (soff == src[si].iov_len) si++, soff = 0;
    }
    return total;
}

// Mirror of hl_gmap_unmap_range for the DONTNEED private-anon registry: keep the surviving sub-region(s)
// (with their prot) tracked so madvise(MADV_DONTNEED) still gives Linux semantics on what remains,
// instead of forgetting the whole entry on a partial unmap. A non-anon range has no entry here and is
// left untouched. hl_gmap_add/anon_track append to their registries, and the appended tail starts at
// uend so it never re-overlaps [ustart,uend) -- the loop terminates.
static void anon_split_unmap(uint64_t ustart, uint64_t uend) {
    anon_lock();
    for (int i = 0; i < g_nanonmap;) {
        uint64_t base = g_anonmap[i].addr, end = base + g_anonmap[i].len;
        if (ustart >= end || uend <= base) {
            i++;
            continue;
        }
        int keep_head = base < ustart, keep_tail = uend < end, prot = g_anonmap[i].prot;
        if (!keep_head && !keep_tail) {
            g_anonmap[i] = g_anonmap[--g_nanonmap];
            continue;
        }
        if (keep_head)
            g_anonmap[i].len = ustart - base;
        else
            g_anonmap[i].addr = uend, g_anonmap[i].len = end - uend;
        // Already holding g_anonmap_lock -> append the surviving tail via the _locked core, not the
        // self-locking wrapper (the mutex is non-recursive).
        if (keep_head && keep_tail) anon_track_locked(uend, end - uend, prot);
        i++;
    }
    anon_unlock();
}

// emulate a MAP_FIXED mapping of [a0, a0+a1) -- anon-zero when `anon`, else the file bytes at
// fd@off -- that lands inside one of the guest's OWN existing (writable private-anon) reservations,
// WITHOUT clobbering a live 4 KB neighbour that shares a partial host page. The guest uses 4 KB
// pages; the host granule can be coarser (16 KB), and MAP_FIXED replaces WHOLE host pages -- so a fixed map of a
// sub-host-page range zeros/relays the neighbour occupying the rest of the edge host page (same class as
// MADV_DONTNEED). Fix (mirrors that split): MAP_FIXED-remap only the fully-covered INTERIOR host
// pages (fresh pages; load the file bytes there for a file map); write the partial head/tail edges IN
// PLACE over EXACTLY the requested bytes (memset 0 for anon, pread for file) so the neighbour survives.
// The caller gates this on the range being contained in a writable private-anon region, so the edge host
// pages are guaranteed mapped+writable. Returns 0 on success, -1 if the interior remap failed.
static ssize_t pread_retry(int fd, void *buffer, size_t length, off_t offset) {
    ssize_t result;
    do {
        result = pread(fd, buffer, length, offset);
    } while (result < 0 && errno == EINTR);
    return result;
}

static int host_fixed_map286(uint64_t a0, uint64_t a1, int prot, int anon, int fd, off_t off) {
    size_t hp = hl_linux_host_map_granularity();
    uint64_t lo = a0, hi = a0 + a1;
    uint64_t ilo = (lo + hp - 1) & ~((uint64_t)hp - 1); // first fully-covered host page
    uint64_t ihi = hi & ~((uint64_t)hp - 1);            // end of last fully-covered host page
    if (ilo < ihi) {
        if (mmap((void *)ilo, (size_t)(ihi - ilo), prot | PROT_READ | PROT_WRITE, MAP_FIXED | MAP_ANON | MAP_PRIVATE,
                 -1, 0) == MAP_FAILED)
            return -1;
        if (!anon && fd >= 0 && pread_retry(fd, (void *)ilo, (size_t)(ihi - ilo), off + (off_t)(ilo - lo)) < 0)
            return -1;
    }
    uint64_t he = ilo < hi ? ilo : hi; // partial head edge [lo, he)
    if (lo < he) {
        uint64_t page = lo & ~((uint64_t)hp - 1);
        size_t prefix = (size_t)(lo - page);
        size_t suffix = he == hi ? (size_t)(page + hp - hi) : 0;
        void *saved = NULL, *saved_suffix = NULL;
        if (anon || hl_linux_bus_hit(lo, he - lo)) {
            if (prefix != 0) {
                saved = malloc(prefix);
                if (saved == NULL || hl_linux_bus_hit(page, prefix)) {
                    free(saved);
                    return -1;
                }
                memcpy(saved, (void *)page, prefix);
            }
            if (suffix != 0) {
                saved_suffix = malloc(suffix);
                if (saved_suffix == NULL || hl_linux_bus_hit(hi, suffix)) {
                    free(saved);
                    free(saved_suffix);
                    return -1;
                }
                memcpy(saved_suffix, (void *)hi, suffix);
            }
            if (mmap((void *)page, hp, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0) ==
                MAP_FAILED) {
                free(saved);
                free(saved_suffix);
                return -1;
            }
            if (saved != NULL) memcpy((void *)page, saved, prefix);
            if (saved_suffix != NULL) memcpy((void *)hi, saved_suffix, suffix);
            free(saved);
            free(saved_suffix);
        }
        if (anon || fd < 0)
            memset((void *)lo, 0, (size_t)(he - lo));
        else if (pread_retry(fd, (void *)lo, (size_t)(he - lo), off) < 0)
            return -1;
    }
    uint64_t tl = he > ihi ? he : ihi; // partial tail edge [tl, hi) (never re-covers the head)
    if (tl < hi) {
        if (anon || fd < 0) {
            /* A fixed anonymous BSS map can replace the BUS tail of the file
               reservation immediately below it.  The tail begins on a host
               page boundary; replace that poisoned page before zeroing it.
               Linux rounds the mapping to guest pages, and bytes beyond hi in
               this host page were inaccessible past-EOF reservation bytes. */
            if (anon) {
                size_t suffix = (size_t)((tl + hp) - hi);
                void *saved_suffix = suffix != 0 ? malloc(suffix) : NULL;
                if ((suffix != 0 && saved_suffix == NULL) || hl_linux_bus_hit(hi, suffix)) {
                    free(saved_suffix);
                    return -1;
                }
                if (saved_suffix != NULL) memcpy(saved_suffix, (void *)hi, suffix);
                if (mmap((void *)tl, hp, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0) ==
                    MAP_FAILED) {
                    free(saved_suffix);
                    return -1;
                }
                if (saved_suffix != NULL) memcpy((void *)hi, saved_suffix, suffix);
                free(saved_suffix);
            } else {
                memset((void *)tl, 0, (size_t)(hi - tl));
            }
        } else if (pread_retry(fd, (void *)tl, (size_t)(hi - tl), off + (off_t)(tl - lo)) < 0)
            return -1;
    }
    return 0;
}

static void mremap_publish_accessible(uint64_t first, uint64_t last) {
    uint64_t low = first & ~(uint64_t)0xfff;
    uint64_t high = (last + 0xfff) & ~(uint64_t)0xfff;
    gna_clear(low, high);
    gro_clear(low, high);
    gnx_clear(low, high);
    gbus_clear(low, high);
}

static void mremap_publish_unmapped(uint64_t first, uint64_t last) {
    uint64_t low = first & ~(uint64_t)0xfff;
    uint64_t high = (last + 0xfff) & ~(uint64_t)0xfff;
    gna_add(low, high);
    gro_clear(low, high);
    gnx_add(low, high);
    gbus_clear(low, high);
}

// The guest's page size (as it sees via AT_PAGESZ / sysconf(_SC_PAGESIZE)).  Read it from auxv after
// stack construction; before that exists, fall back to the Linux ABI constant, never the host mapping
// granularity.  Host mmap alignment paths use hl_linux_host_map_granularity() explicitly.
static size_t guest_pagesz(void) {
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == 6 && v) return (size_t)v;
    }
    return HL_LINUX_GUEST_PAGE_SIZE;
}
