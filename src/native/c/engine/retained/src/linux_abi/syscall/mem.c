// Extracted from service(): Memory — mmap/brk/mprotect/madvise syscalls. Returns 1 if nr was handled, 0 otherwise.
// Included by service.c after service/helpers.c, before service() — same TU scope (globals + helpers).
#include "../page.h"
#include "../logical_vma.h"

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
    gbus_clear(low, high);
}

static void mremap_publish_unmapped(uint64_t first, uint64_t last) {
    uint64_t low = first & ~(uint64_t)0xfff;
    uint64_t high = (last + 0xfff) & ~(uint64_t)0xfff;
    gna_add(low, high);
    gro_clear(low, high);
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

static int svc_mem(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                   uint64_t a5) {
    switch (nr) {
    // ===================== Memory — mmap/brk/mprotect/madvise (anon charged against cgroup memory.max)
    // =====================
    // brk
    case 214: {
        if (!G_BRK_GROWABLE) { // fixed, non-growable break -> glibc/musl fall back to their mmap allocator
            G_RET(c) = brk_lo;
            break;
        }
        if (a0 == 0) {
            G_RET(c) = brk_cur;
            break;
        }
        if (a0 >= brk_lo && a0 <= brk_hi) {
            // heap growth -> charge cgroup memory.max
            if (g_mem_max && a0 > brk_cur) {
                uint64_t delta = a0 - brk_cur;
                if (atomic_fetch_add(&g_mem_charged, delta) + delta > g_mem_max) {
                    atomic_fetch_sub(&g_mem_charged, delta);
                    G_RET(c) = brk_cur;
                    // over limit -> break unchanged (ENOMEM)
                    break;
                }
                // shrink -> uncharge
            } else if (g_mem_max && a0 < brk_cur) {
                uint64_t delta = brk_cur - a0, cur = atomic_load(&g_mem_charged);
                atomic_fetch_sub(&g_mem_charged, delta > cur ? cur : delta);
            }
            brk_cur = a0;
            acct_publish_mem(); // publish the new charge into this process's cross-process memory slot
        }
        G_RET(c) = brk_cur;
        break;
    }
    case 215: {
        // munmap error checks (Linux returns before touching anything): a zero length, an addr that is
        // not a multiple of the (guest) page size, or a range that wraps / lies outside the address space
        // is EINVAL. Aligning against guest_pagesz() (the 4 KB AT_PAGESZ both guest ISAs publish) -- not the
        // host page, which may be coarser -- lets a legitimate 4 KB-granular unmap through while rejecting a truly
        // mis-aligned start (LTP munmap03: len 0, addr+1, and an out-of-range rlim_max address).
        {
            size_t gpg = guest_pagesz();
            if (a1 == 0 || (a0 & (gpg - 1)) || a0 + a1 < a0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        int logical_mapping_prepared = 0;
        int logical_transition_locked = 0;
        if (jit_guest_soft_active()) {
            gbus_mapping_transition_lock();
            logical_transition_locked = 1;
            if (hl_logical_vma_global_overlap(a0, a1)) {
                gbus_mapping_stw_begin();
                logical_mapping_prepared = 1;
            }
        }
        // Drop any guest PROT_NONE coverage for the unmapped range (the EFAULT registry, thread.c): the
        // addresses no longer name an inaccessible mapping. Uses the guest logical [a0,a1) even when the
        // physical release below is partial -- the guest's mapping is logically gone either way.
        gna_clear(a0 & ~(uint64_t)0xfff, (a0 + a1 + 0xfff) & ~(uint64_t)0xfff);
        gro_clear(a0 & ~(uint64_t)0xfff, (a0 + a1 + 0xfff) & ~(uint64_t)0xfff);
        gbus_clear(a0, a0 + a1);
        // A non-fixed anon mapping carries a 64 KB guard tail that mmap (case 222) reserved
        // past the guest's logical length (so glibc's vectorized over-reads land in mapped memory).
        // The guest only knows its logical length a1, so a plain munmap(a0, a1) leaves that tail mapped
        // -> ~64 KB of address space (plus its gmap/anon_track bookkeeping) leaks per map/unmap cycle.
        // When a0 starts a tracked mapping whose FULL extent is exactly a1 + the 64 KB guard, extend the
        // unmap to cover the tail too. The gmap registry stores the full extent (incl. guard); requiring
        // an exact `full == a1 + 0x10000` match means a0 is the mapping start AND a1 is its original
        // logical length -- i.e. a complete unmap -- so this can never reach past the mapping into a
        // neighbour (a partial unmap, full == a1, leaves the tail alone). Guard-less mappings (file/fixed,
        // full == a1) and untracked mappings (full == 0) keep the plain a1 unmap unchanged.
        size_t len = (size_t)a1;
        uint64_t full = hl_gmap_find_length(a0);
        uint64_t guest_length = hl_gmap_find_guest_length(a0);
        // Page-size mismatch: guest pages are 4 KB (AT_PAGESZ) but the host granule may be coarser, and
        // munmap rounds the LENGTH up to a whole host page. The host also gives every distinct mmap
        // its own host-page-aligned base + host-page-rounded extent, so two SEPARATE guest mappings never
        // share a host page -- a host page is only ever shared by 4 KB sub-regions of ONE mapping.
        //   * COMPLETE unmap of a whole tracked mapping (a0 is its base and a1 is its logical length, with
        //     or without the 64 KB guard tail): the whole host-page-rounded extent is ours, no neighbour
        //     sits in the edge pages, so releasing it -- rounding the length UP, which also frees the guard
        //     tail (else ~64 KB leaks per map/unmap cycle) -- is safe.
        //   * Any OTHER unmap may be a 4 KB-granular SUB-RANGE of a larger mapping (V8's page allocator
        //     freeing an interior chunk, ZendMM trimming an aligned over-allocation), whose partial edge
        //     host pages still back a LIVE 4 KB neighbour the guest keeps (coarser host page only): a plain
        //     munmap there rounds a partial edge page OUT to the full host page and
        //     unmaps the neighbour -- and an unaligned start is outright EINVAL'd by host munmap (V8 then
        //     aborts on CHECK(0 == munmap)). So release only the whole HOST pages lying ENTIRELY inside
        //     [a0, a0+len); the partial edge pages stay mapped. The guest's logical unmap still succeeds
        //     (return 0) -- matching Linux, which never faults an unmap of a partial/already-unmapped range.
        size_t hp = hl_linux_host_map_granularity();
        uint64_t physical_address = 0, physical_length = 0;
        int has_physical = hl_gmap_find_physical(a0, &physical_address, &physical_length);
        int complete = full != 0 && guest_length == (uint64_t)a1 &&
                       (((a0 & (hp - 1)) == 0) || (has_physical && physical_address != a0));
        int r;
        uint64_t u_lo, u_hi; // the range host munmap actually cleared (empty when u_lo==u_hi)
        if (complete) {
            len = (size_t)full; // include the guard tail; whole extent is ours -> round-up is safe
            r = munmap((void *)(uintptr_t)(has_physical ? physical_address : a0),
                       (size_t)(has_physical ? physical_length : len));
            u_lo = a0, u_hi = a0 + len;
        } else {
            uint64_t lo = (a0 + hp - 1) & ~(uint64_t)(hp - 1); // first host page fully in range
            uint64_t hi = (a0 + len) & ~(uint64_t)(hp - 1);    // end of last host page fully in range
            r = (lo < hi) ? munmap((void *)lo, (size_t)(hi - lo)) : 0;
            u_lo = lo, u_hi = (lo < hi) ? hi : lo;
        }
        if (r == 0 && u_hi > u_lo) {
            // Update the registries against the range actually unmapped. A full-cover unmap drops the
            // entry; a partial unmap (guest trimming the head/middle of a larger mapping, e.g. ZendMM
            // freeing an aligned over-allocation) SPLITS it so the surviving sub-region(s) stay tracked --
            // reclaimed at execve() teardown and still findable by hl_gmap_find_length (the mremap grow path).
            // (PROT_NONE coverage was already dropped above via gna_clear over the guest-logical range.)
            hl_gmap_unmap_range(u_lo, u_hi);
            anon_split_unmap(u_lo, u_hi);
            filemap_unmap(u_lo, u_hi);
            futex_shared_unmap(u_lo, u_hi);         // drop/trim shared-futex-key coverage for the released range
            wipefork_del(u_lo, u_hi - u_lo);        // a wipe-on-fork range that was unmapped no longer applies
            dontfork_del(u_lo, u_hi - u_lo);        // ...nor does a dont-fork marking on the released range
            hl_gmap_lock_remove(u_lo, u_hi - u_lo); // an unmapped range is implicitly unlocked (mlock -> VmLck)
            // The host pages [u_lo,u_hi) are now genuinely released, so a guest access there must fault
            // (SIGSEGV). Without this the JIT's lazy zero-page grower (jit86_lazyguard) would re-serve the
            // fault -- growth budget for an adjacent live mapping, small budget otherwise -- and the guest
            // would silently read fresh zero memory instead of faulting. Mark the released range inaccessible
            // so the fault handler delivers the guest SIGSEGV; a later mmap over it clears the coverage.
            // (Coarse-host-page residual: a 4 KB sub-page whose host page still backs a LIVE
            // neighbour is NOT released above -> not marked here -> stays readable. That mixed-page case needs
            // per-4 KB software fault checks the JIT deliberately avoids; the common aligned/whole-page case is
            // now correct.)
            gna_add(u_lo, u_hi);
        }
        if (r == 0 && g_mem_max) {
            // uncharge (clamp >=0)
            uint64_t cur = atomic_load(&g_mem_charged), d = (uint64_t)a1;
            atomic_fetch_sub(&g_mem_charged, d > cur ? cur : d);
            acct_publish_mem(); // publish the reduced charge into this process's cross-process memory slot
        }
        if (r == 0 && logical_mapping_prepared) {
            (void)hl_logical_vma_global_unmap(a0, a1);
            hl_logical_vma_global_reclaim_quiescent();
        }
        // stale-translation: the guest may re-map DIFFERENT code at this now-free VA -> drop any cached block
        // translations for the unmapped range so the dispatcher re-translates the new bytes (JITs/trampolines).
        if (r == 0) G_SMC_UNMAP(a0, a0 + (uint64_t)a1);
        if (logical_mapping_prepared) gbus_mapping_stw_end();
        /* Keep soft translation active when the last logical view disappears.
           File-backed 4K views commonly oscillate between zero and one entry
           (CoreCLR does this while replacing JIT/runtime mappings).  Empty
           snapshots resolve through the identity path, so guarded blocks
           remain correct; deactivating here would rotate the entire code cache
           only to rotate it again at the next mapping.  Whole-image reset owns
           the eventual guarded -> direct transition. */
        if (logical_transition_locked) gbus_mapping_transition_unlock();
        G_RET(c) = (uint64_t)r;
        break;
    }
    case 216: {
        // mremap (a0=old, a1=old_len, a2=new_len, a3=flags, a4=new_addr). macOS has no mremap, so
        // emulate it -- but honor the FLAGS contract, which the guest relies on:
        //   flags==0        : the mapping MUST NOT move. Grow only if the tail is free, else -ENOMEM.
        //   MREMAP_MAYMOVE  : may relocate (allocate a new region, copy, free the old).
        // Getting this wrong corrupts the guest: a flags==0 caller keeps using the OLD address (Linux
        // guarantees it is unchanged), so relocating -- and freeing the old region out from under those
        // still-live pointers -- is a use-after-free (glibc/ZendMM grows a ~2 MB json_encode
        // buffer by one page with a no-move mremap; the old code always moved it -> SIGSEGV).
        // The original anon mmap (case 222) reserved a 64 KB guard tail past the guest's logical length,
        // so the tracked extent is a1+guard; a grow whose new length still fits inside that already-mapped
        // extent needs neither new memory nor a move.
        // EFAULT when the OLD range [a0,a1) is not fully mapped, the way Linux mremap validates its source
        // (LTP mremap03 mremaps a tst_get_bad_addr guard: one PROT_NONE page then unmapped space). Gated on
        // the source NOT being one of the guest's OWN tracked mappings, so the hot glibc realloc path (a
        // gmap-tracked region) and any fully-tracked PROT_NONE reservation skip the page-walk probe -- zero
        // cost and no false EFAULT there; only an untracked / partially-covered source is validated against
        // the live address space (host_range_mapped rejects both an unmapped page and a PROT_NONE page).
        //
        // old_size == 0 is only legal for a MAP_SHARED source (Linux mm/mremap.c: it then means "make a
        // second mapping of the same shared object"); for a private mapping it is -EINVAL. The engine had
        // no check at all and handed back a brand-new anonymous mapping for mremap(p, 0, n, MAYMOVE).
        if ((uint64_t)a1 == 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // A zero new_len is never legal (Linux mm/mremap.c: `if (!new_len) return -EINVAL;`). The engine
        // had no check and fell into the shrink path, unmapping the whole source and reporting success.
        if ((uint64_t)a2 == 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (a1 && !hl_gmap_contains(a0, (uint64_t)a1) && !host_range_mapped((uintptr_t)a0, (size_t)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        const uint64_t guard = 0x10000;
        uint64_t tracked = hl_gmap_find_length(a0);       // full mapped extent at a0 (incl. guard), 0 if untracked
        uint64_t phys = tracked ? tracked : (uint64_t)a1; // bytes we can assume are mapped at a0
        // MREMAP_DONTUNMAP(4): duplicate the mapping to a new address while the OLD range STAYS mapped as
        // fresh zero-filled anonymous memory (Linux mm/mremap.c). Requires MREMAP_MAYMOVE, an unchanged
        // length, and a private-anon source (the only case Linux accepts); anything else is EINVAL. Place a
        // fresh private-anon copy (+guard) at a kernel-chosen address, copy the bytes, then re-establish the
        // source range as zero anon so it remains readable/writable. Handled before the FIXED/shrink/grow
        // paths, which all assume the source is released.
        if (a3 & 4) {
            if (!(a3 & 1) || (uint64_t)a2 != (uint64_t)a1 || anon_prot_if_contained(a0, (size_t)a1) < 0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            void *r = mmap(0, (size_t)a2 + guard, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
            if (r == MAP_FAILED) {
                G_RET(c) = (uint64_t)(-errno);
                break;
            }
            memcpy(r, (void *)a0, (size_t)a1);
            // Re-zero the source in place (DONTUNMAP leaves it mapped as fresh anon zero pages).
            mmap((void *)a0, (size_t)a1, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0);
            hl_gmap_add((uint64_t)r, (uint64_t)a2 + guard);
            hl_gmap_set_guest_length((uint64_t)r, (uint64_t)a2);
            anon_track((uint64_t)r, (uint64_t)a2 + guard, PROT_READ | PROT_WRITE);
            anon_update_prot(a0, (uint64_t)a1, PROT_READ | PROT_WRITE); // source is writable zero anon again
            mremap_publish_accessible(a0, a0 + a1);
            mremap_publish_accessible((uint64_t)r, (uint64_t)r + a2 + guard);
            G_SMC_UNMAP(a0, a0 + (uint64_t)a1);
            G_SMC_UNMAP((uint64_t)r, (uint64_t)r + (uint64_t)a2);
            G_RET(c) = (uint64_t)r;
            break;
        }
        // MREMAP_FIXED(2): relocate the mapping to EXACTLY new_addr (a4), the way mremap(MREMAP_FIXED) does.
        // Linux (mm/mremap.c) requires MREMAP_MAYMOVE to also be set, a page-aligned new_addr, and that the
        // new range not overlap the old -- otherwise -EINVAL. It then unmaps whatever sat at the destination
        // (MAP_FIXED semantics) and moves the mapping there. Must be handled BEFORE the in-place shrink/grow
        // paths below (a FIXED remap ALWAYS moves, even to a smaller length).
        if (a3 & 2) {
            size_t gpg = guest_pagesz();
            uint64_t nlo = a4, nhi = a4 + (uint64_t)a2, olo = a0, ohi = a0 + (uint64_t)a1;
            // Flag/arg validation runs for every source (Linux checks it before touching the mapping).
            if (!(a3 & 1) || a2 == 0 || (a4 & (gpg - 1)) || (nlo < ohi && olo < nhi)) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Relocate ONLY a PRIVATE-ANON source: emulate by placing a fresh private-anon region
            // (+guard tail for glibc over-reads) at a4, copying min(old,new) bytes, then freeing the old
            // extent. A host MAP_FIXED needs a host-page-aligned base; when a4 is only guest-page- (4 KB-)
            // aligned it may fall inside a tracked writable anon reservation, so use the edge-safe
            // fixed map there. A FILE-backed / MAP_SHARED source is intentionally NOT relocated here (we do
            // not track the fd/offset needed to re-map the file at a4); it falls through to the pre-existing
            // shrink/grow/relocate logic below, where a same-size/shrink FIXED stays coherent via the shared
            // file exactly as before (LTP mremap06 moves a MAP_SHARED sub-mapping this way).
            if (anon_prot_if_contained(a0, (size_t)a1) >= 0) {
                size_t hp = hl_linux_host_map_granularity();
                void *r;
                if ((a4 & (hp - 1)) == 0) {
                    r = mmap((void *)a4, (size_t)a2 + guard, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_ANON | MAP_PRIVATE,
                             -1, 0);
                } else {
                    int aprot = anon_prot_if_contained(a4, (size_t)a2);
                    r = (aprot >= 0 && (aprot & PROT_WRITE) &&
                         host_fixed_map286(a4, (uint64_t)a2, PROT_READ | PROT_WRITE, 1, -1, 0) == 0)
                            ? (void *)a4
                            : MAP_FAILED;
                }
                if (r != MAP_FAILED) {
                    size_t n = (size_t)a1 < (size_t)a2 ? (size_t)a1 : (size_t)a2;
                    if (n) memcpy((void *)a4, (void *)a0, n);
                    if (a0) {
                        munmap((void *)a0, (size_t)phys); // free the full old extent (incl. its guard tail)
                        hl_gmap_remove(a0);
                        anon_untrack(a0, (size_t)phys);
                        mremap_publish_unmapped(a0, a0 + phys);
                        hl_gmap_lock_remove(a0, (uint64_t)a1);
                        wipefork_del(a0, (uint64_t)a1);
                        dontfork_del(a0, (uint64_t)a1);
                    }
                    hl_gmap_add(a4, (uint64_t)a2 + guard);
                    hl_gmap_set_guest_length(a4, (uint64_t)a2);
                    anon_track(a4, (uint64_t)a2 + guard, PROT_READ | PROT_WRITE);
                    mremap_publish_accessible(a4, a4 + a2 + guard);
                    // stale-translation: the mapping (and any executable code in it) relocated. Drop cached
                    // translations for BOTH the freed source VA and the replaced destination VA.
                    G_SMC_UNMAP(a0, a0 + (uint64_t)a1);
                    G_SMC_UNMAP(a4, a4 + (uint64_t)a2);
                    G_RET(c) = a4;
                    break;
                }
                // anon placement failed -> fall through to the generic logic below (best effort)
            }
            // file-backed source (or anon placement failed): fall through -- do NOT break.
        }
        // Genuine shrink: Linux mremap(new_len < old_len) unmaps the released tail [a0+new_len, a0+old_len)
        // in place (the base and surviving prefix are unchanged). Merely returning a0 -- as an earlier
        // "grow that fits the extent" fast path did -- left those pages mapped, so a guest access to the
        // released tail wrongly succeeded instead of faulting. Round new_len up to a guest page (Linux
        // rounds the length) and release everything from there to the end of the tracked extent (which
        // also drops the internal guard tail), then retire every registry over the freed range exactly as
        // munmap does so /proc, the anon/file/futex/lock maps, and the PROT_NONE fault registry all agree.
        {
            size_t gpg = guest_pagesz();
            uint64_t nlen = ((uint64_t)a2 + gpg - 1) & ~((uint64_t)gpg - 1);
            if (nlen < (uint64_t)a1) {
                uint64_t nend = a0 + nlen, oend = a0 + phys;
                if (oend > nend) {
                    munmap((void *)nend, (size_t)(oend - nend));
                    hl_gmap_unmap_range(nend, oend); // trim the tracked mapping, keep [a0, nend)
                    anon_split_unmap(nend, oend);
                    filemap_unmap(nend, oend);
                    futex_shared_unmap(nend, oend);
                    wipefork_del(nend, oend - nend);
                    dontfork_del(nend, oend - nend);
                    hl_gmap_lock_remove(nend, oend - nend);
                    gbus_clear(nend, oend);
                    // The released tail is genuinely unmapped now -> a guest access must fault. Record it in
                    // the PROT_NONE fault registry so the lazy zero-page grower does not silently re-serve it.
                    gna_add(nend, (oend + 0xfff) & ~(uint64_t)0xfff);
                    G_SMC_UNMAP(nend, oend);
                }
                G_RET(c) = a0;
                break;
            }
        }
        /*
         * A grow that fits in the hidden compatibility tail stays in place,
         * but it is not a no-op: the guest-visible VMA length expands and the
         * newly exposed pages cease to be guard/tombstone state. Keeping the
         * old guest length made a later munmap look partial, leaking or
         * repeatedly reclassifying the same mapping.
         */
        if ((uint64_t)a2 <= phys) {
            hl_gmap_set_guest_length(a0, (uint64_t)a2);
            uint64_t exposed_first = (a0 + a1 + 0xfff) & ~(uint64_t)0xfff;
            uint64_t exposed_last = (a0 + a2 + 0xfff) & ~(uint64_t)0xfff;
            mremap_publish_accessible(exposed_first, exposed_last);
            G_RET(c) = a0;
            break;
        }
        // Grow beyond the current extent. Unless a fixed destination was requested, first try to extend in
        // place by mapping the fresh tail right after the current extent; macOS relocates a hinted (non-
        // FIXED) mmap when the target range isn't free, so an exact-address result means the tail was free.
        if (!(a3 & 2 /*MREMAP_FIXED*/)) {
            uint64_t end = a0 + phys, want = (uint64_t)a2 + guard;
            void *ext =
                mmap((void *)end, (size_t)(a0 + want - end), PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
            if (ext == (void *)end) {
                hl_gmap_remove(a0);
                hl_gmap_add(a0, want); // track the grown extent (incl. fresh guard) for execve() teardown
                hl_gmap_set_guest_length(a0, (uint64_t)a2); // /proc maps report the guest length (sans guard)
                anon_track(a0, want, PROT_READ | PROT_WRITE);
                mremap_publish_accessible(end, a0 + want);
                G_RET(c) = a0;
                break;
            }
            if (ext != MAP_FAILED) munmap(ext, (size_t)(a0 + want - end)); // landed elsewhere -> discard
        }
        // Cannot extend in place. Without MREMAP_MAYMOVE we may not relocate -> ENOMEM (the caller then
        // does its own alloc+copy+free, exactly as it would when Linux can't grow a no-move mapping).
        if (!(a3 & 1 /*MREMAP_MAYMOVE*/)) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        // Relocate: allocate the new region (+guard tail so glibc's vectorized over-reads stay mapped),
        // copy the old bytes, then free the old extent. Allocate-before-free so a failure leaves old intact.
        void *r = mmap(0, (size_t)a2 + guard, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
        if (r == MAP_FAILED) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        size_t n = (size_t)a1 < (size_t)a2 ? (size_t)a1 : (size_t)a2;
        memcpy(r, (void *)a0, n);
        if (a0) {
            munmap((void *)a0, (size_t)phys); // free the FULL tracked extent (incl. old guard tail)
            hl_gmap_remove(a0);
            anon_untrack(a0, (size_t)phys);
            mremap_publish_unmapped(a0, a0 + phys);
        }
        hl_gmap_add((uint64_t)r, (uint64_t)a2 + guard);                        // track for execve() teardown
        hl_gmap_set_guest_length((uint64_t)r, (uint64_t)a2);                   // /proc maps: guest length (sans guard)
        anon_track((uint64_t)r, (uint64_t)a2 + guard, PROT_READ | PROT_WRITE); // fresh private-anon copy
        mremap_publish_accessible((uint64_t)r, (uint64_t)r + a2 + guard);
        G_RET(c) = (uint64_t)r;
        break;
    }
    // mmap
    case 222: {
        // A file-backed mmap (not MAP_ANON) whose fd is not a valid open descriptor is -EBADF, and Linux's
        // fget() rejects it BEFORE the length check -- so this must precede the len==0 EINVAL below (LTP
        // mmap08 maps a CLOSED/-1 fd with len 0 and expects EBADF, not EINVAL). macOS mmap otherwise reports
        // EINVAL for a stale fd, so validate explicitly to return the kernel's errno.
        if (!(a3 & 0x20) && ((int)a4 < 0 || fcntl((int)a4, F_GETFD) < 0)) {
            G_RET(c) = (uint64_t)(int64_t)(-EBADF);
            break;
        }
        // Linux mmap with length 0 is EINVAL (must return before the anon guard tail would otherwise map
        // a nonzero region and wrongly succeed). LTP mmap08 companion / general POSIX contract.
        if (a1 == 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // MAP_FIXED requires a page-aligned address: Linux mmap validates this up front (mm/mmap.c
        // addr & ~PAGE_MASK -> EINVAL) BEFORE reserving anything. Reject a misaligned fixed address here so
        // it never reaches the MAP_FIXED reconciliation below, whose neighbour-preserving memcpy would
        // otherwise dereference the misaligned low page (a bogus (void*)(ps+1) target) and fault the engine.
        if ((a3 & 0x10) && (a0 & (uint64_t)(guest_pagesz() - 1))) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // memfd write seal: a MAP_SHARED mapping that carries write permission over a file sealed with
        // F_SEAL_WRITE (0x8) is refused with EPERM (Linux mm/shmem.c seal check). A PROT_READ shared map
        // (no write) and a MAP_PRIVATE copy-on-write map are unaffected, so gate strictly on shared+write.
        if (!(a3 & 0x20) && (a3 & 0x01) && ((int)a2 & PROT_WRITE) && (int)a4 >= 0 && (int)a4 < HL_NFD &&
            (memfd_seals_fd((int)a4) & 0x8)) {
            G_RET(c) = (uint64_t)(int64_t)(-EPERM);
            break;
        }
        // File-backed mmap of a RAM-backed scratch fd: flush the cache so the mapping sees the real bytes.
        if (!(a3 & 0x20)) memf_materialize((int)a4);
        // charge anon, but NOT MAP_NORESERVE
        int charge = g_mem_max && (a3 & 0x20) && !(a3 & 0x4000);
        //   (libc reserves huge virtual arenas it never commits;
        if (charge) {
            if (atomic_fetch_add(&g_mem_charged, (uint64_t)a1) + (uint64_t)a1 >
                // real memory.max counts RSS, not reservations)
                g_mem_max) {
                atomic_fetch_sub(&g_mem_charged, (uint64_t)a1);
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
            acct_publish_mem(); // publish the new charge into this process's cross-process memory slot
        }
        // glibc's vectorized string ops over-read up to 16 bytes past a buffer's logical end; on Darwin
        // that hits an unmapped page -> SIGBUS. Map a 64KB guard tail on non-fixed anon maps so the
        // over-read lands in mapped zero memory (x86 glibc relies on this; harmless for aarch64).
        // MAP_FIXED_NOREPLACE (0x100000) forwards to the host, whose EEXIST verdict must reflect ONLY the
        // guest's requested range -- a guard tail would let a collision in the extra pages spuriously fail
        // (or a free-space map succeed where the tail overlaps), so keep NOREPLACE maps exact-length.
        size_t guard = (!(a3 & 0x10) && (a3 & 0x20) && !(a3 & 0x100000)) ? 0x10000 : 0;
        uint64_t page_mask = (uint64_t)guest_pagesz() - 1;
        if (a1 > UINT64_MAX - page_mask - guard) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        uint64_t mapped_length = ((a1 + page_mask) & ~page_mask) + guard;
        // mprotect (case 226) is a no-op (the JIT never executes guest pages), so a later PROT_READ ->
        // PROT_READ|WRITE upgrade would be silently dropped. Map ANON memory writable up front so the
        // upgrade is already in effect (redis' checkLinuxMadvFreeForkBug mmaps R then mprotects RW then stores).
#if defined(__linux__)
        // Linux host pages have the same granularity as this Linux ABI, so preserve the requested mapping
        // protection.  In particular, a PROT_NONE reservation must stay inaccessible until mprotect commits
        // it; the mprotect path below performs the real host transition on this platform.
        int prot = (int)a2;
#else
        int prot = (a3 & 0x20) ? ((int)a2 | PROT_READ | PROT_WRITE) : (int)a2;
#endif
        // W6A item 3: guest RWX / PROT_EXEC mmaps (JVM/V8/LuaJIT/.NET/PyPy JIT arenas). On macOS a
        // non-MAP_JIT mmap that requests PROT_EXEC fails with EPERM under the hardened W^X policy, so
        // these guests can't allocate their code arena. But this is a DBT: the host NEVER executes guest
        // pages natively -- guest "execution" is translate_block() reading the page's bytes and emitting
        // host code into the (separately RX) code cache. So PROT_EXEC on a guest mapping is meaningless to
        // the host and only triggers the EPERM. Strip it: the page is mapped R+W, the guest writes its
        // generated code, "executes" it (guest PC enters the page -> map_host miss -> translate), and runs.
        // Setting g_rwx_guest also arms the (otherwise inert) SMC write-fault invalidation in frontend/x86_64
        // so a guest that OVERWRITES already-translated code re-translates. NORWXFIX=1 disables the strip.
        if (prot & PROT_EXEC) {
            /*
             * A read-only executable mapping can alias a distinct writable
             * MAP_SHARED view of the same object. The writable view must arm
             * store observation even though this RX view itself is not
             * writable (memfd JIT/code-cache layout).
             */
            g_rwx_guest = 1;
            if (a3 & 0x20) {
                // Anon JIT arena: strip EXEC and map R+W so the guest can write its generated code.
                prot = (prot & ~PROT_EXEC) | PROT_READ | PROT_WRITE;
            } else if (prot & PROT_WRITE) {
                // File-backed WRITE+EXEC map: macOS W^X rejects it (EACCES) without MAP_JIT, but the JIT
                // never executes guest pages, so EXEC is meaningless -- drop it, keeping the file map R+W.
                // A file-backed READ+EXEC map (no write) is permitted by macOS -- that is how ld.so loads a
                // .so's text -- so it is left untouched. (LTP mincore02 maps a file PROT_READ|WRITE|EXEC.)
                prot &= ~PROT_EXEC;
            }
        }
        size_t hp = hl_linux_host_map_granularity();
        void *r = MAP_FAILED;
        int off_emul = 0;
        void *physical_mapping = NULL;
        size_t physical_mapping_size = 0;
        uint64_t bus_accessible = a1;
        int bus_prepared = 0;
        int mapping_prepared = 0;
        hl_logical_vma_plan *logical_plan = NULL;
        int logical_committed = 0;
        int logical_prepare_failed = 0;
        int logical_prepare_errno = 0;
        int soft_activated_here = 0;
        int logical_transition_locked = 0;
        int logical_candidate =
            hp > (size_t)guest_pagesz() && (a3 & 0x10) && (a3 & 0x01) && !(a3 & 0x20) && (int)a4 >= 0 && a1;
        if (logical_candidate) {
            /* One lock order for every logical mutation:
               transition -> translation flush -> mapping STW -> ledger. */
            gbus_mapping_transition_lock();
            logical_transition_locked = 1;
            if (jit_guest_soft_activate())
                soft_activated_here = 1;
            else {
                logical_prepare_failed = 1;
                logical_prepare_errno = ENOMEM;
            }
            if (!logical_prepare_failed) {
                gbus_mapping_stw_begin();
                mapping_prepared = 1;
                if (hl_logical_vma_global_prepare_shared(a0, a1, (uint32_t)a2, (int)a4, a5, hp, &logical_plan) != 0) {
                    logical_prepare_failed = 1;
                    logical_prepare_errno = errno;
                }
            }
        }
        // The past-EOF SIGBUS ledger + tail zero-fill below are keyed off st_size, which only bounds the
        // readable data of a REGULAR file. A character device (/dev/zero, /dev/full backed by /dev/zero,
        // /dev/mem) reports st_size 0 yet mmaps to an unlimited zero page on Linux -- treating it as a
        // 0-length file armed the whole mapping for guest SIGBUS, so a plain read of an mmap'd /dev/zero
        // page terminated the guest (SIGBUS) where Linux returns zero. Restrict the emulation to S_ISREG.
        if (!(a3 & 0x20) && (a3 & 0x02) && (int)a4 >= 0) {
            struct stat metadata;
            if (fstat((int)a4, &metadata) == 0 && S_ISREG(metadata.st_mode)) {
                uint64_t available = (uint64_t)metadata.st_size > a5 ? (uint64_t)metadata.st_size - a5 : 0;
                bus_accessible = available > UINT64_MAX - UINT64_C(4095)
                                     ? UINT64_MAX
                                     : (available + UINT64_C(4095)) & ~UINT64_C(4095);
                if (bus_accessible < a1) {
                    gbus_prepare();
                    bus_prepared = 1;
                }
            }
        }
        if (!bus_prepared && !mapping_prepared && (a3 & 0x10)) {
            gbus_mapping_prepare();
            mapping_prepared = 1;
        }
        uint64_t pc_hint = 0;
        (void)pc_hint;
        // checkpoint/restore: hint a kernel-placed (a0==0), non-fixed guest map into the deterministic high
        // arena so a later restore's MAP_FIXED lands on a free VA. Inert unless armed (returns 0). A plain
        // hint: if the (reliably free) high slot were busy, the kernel just places it elsewhere.
        if (a0 == 0 && !(a3 & 0x10)) {
            uint64_t ch = hl_linux_snapshot_reserve(&g_ckpt_snapshot, (uint64_t)a1 + guard);
            if (ch) a0 = ch;
        }
        // A non-PIE ET_EXEC is logically mapped at its low Linux link addresses but physically stored in
        // the high host arena. The host kernel therefore sees a low mmap hint overlapping the executable
        // as free and may honor it, while Linux would relocate the mapping because that guest range is busy.
        // Node 18's V8 allocator hinted 0xf00000 inside its 0x400000.. image, received that colliding address,
        // then the non-PIE fold correctly rebased a heap-header store into RX text. Treat such a non-fixed
        // hint as unavailable and let the host choose a genuinely free address. MAP_FIXED retains replacement
        // semantics and is handled separately.
        if (a0 && !(a3 & 0x10) && g_nonpie_lo && a0 < g_nonpie_hi && a0 + a1 > g_nonpie_lo) a0 = 0;
        // The anonymous compatibility tail is host-visible but not part of the guest's requested range.
        // Darwin may honor an executable mapping hint placed in a hole between ELF segments even when
        // that hidden tail reaches the next segment; unmapping the generated-code allocation then removes
        // live image pages. Let Darwin choose a genuinely free address for non-fixed JIT mappings. Their
        // address is not contractual, and reservation/commit hints without PROT_EXEC keep their cage path.
        if (a0 && !(a3 & 0x10) && (a3 & 0x20) && (a2 & PROT_EXEC)) a0 = 0;
#ifdef PCACHE_MMAP_HINT
        // (pcache): give the dynamic linker's file-backed, non-fixed, kernel-placed maps (library
        // loads) a DETERMINISTIC base hint so their translated blocks are reusable across runs of the same
        // binary. A plain hint, never MAP_FIXED: if the range is busy the kernel places it elsewhere and
        // the map simply isn't cacheable this run (pcache_note_libmap below only records hint-honored
        // maps, and a warm run only ACTIVATES restored blocks when the same file identity lands on the
        // same base). No-op unless HL_PCACHE is on.
        if (a0 == 0 && !(a3 & 0x10) && !(a3 & 0x20) && (int)a4 >= 0) {
            pc_hint = pcache_mmap_hint((uint64_t)a1);
            a0 = pc_hint;
        }
#endif
        // a MAP_FIXED map that REPLACES a 4 KB-granular sub-range of one of the guest's own
        // reservations (V8/Go committing fresh pages, or ld.so laying a segment inside its reserved span)
        // has a partial host-page edge shared with a LIVE 4 KB neighbour (coarser host page only --
        // hence the hp > guest_pagesz() gate below). A direct host MAP_FIXED
        // there replaces WHOLE host pages -> the neighbour is zeroed/relaid (the heap-corruption
        // class; the likely victoria-metrics SIGBUS). When the range is fully contained in a tracked
        // WRITABLE private-anon region (so its edge host pages are mapped+writable), emulate the fixed map
        // edge-safely instead: remap only the interior host pages, fill the partial edges in place. Gated
        // on containment, so every fresh/whole-page/free-space fixed map keeps the direct path below and is
        // byte-identical (a non-contained map has no neighbour to protect).
        int fixed286 = 0;
        if (!logical_prepare_failed && hp > (size_t)guest_pagesz() && (a3 & 0x10) && a1 &&
            ((a0 & (hp - 1)) || ((a0 + a1) & (hp - 1)))) {
            int aprot = anon_prot_if_contained(a0, (size_t)a1);
            if (aprot >= 0 && (aprot & PROT_WRITE)) {
                r = host_fixed_map286(a0, a1, prot, (a3 & 0x20) ? 1 : 0, (a3 & 0x20) ? -1 : (int)a4, (off_t)a5) == 0
                        ? (void *)a0
                        : MAP_FAILED;
                fixed286 = 1;
            }
        }
        if (fixed286 && r != MAP_FAILED && !(a3 & 0x20) && (a3 & 0x01)) off_emul = 2;
        if (!fixed286 && !logical_prepare_failed)
            r = mmap((void *)a0, (size_t)a1 + guard, prot, mmap_flags((int)a3), (a3 & 0x20) ? -1 : (int)a4, (off_t)a5);
        // Host-page-unaligned file offset. mmap requires the FILE OFFSET to be a multiple of the host page
        // size, but a Linux guest (4 KB pages) may map a file at any 4 KB-granular offset; on a coarser-page
        // host a non-MAP_FIXED file map at a 4 KB- but not host-page-aligned offset is
        // therefore rejected with EINVAL. Map from the preceding host-page-aligned file offset and return
        // the Linux-page-aligned subrange. The physical head remains tracked for complete munmap/exec cleanup.
        // Keeping the real vnode mapping is essential for MAP_SHARED aliases: runtimes such as CoreCLR map
        // one memfd RW and RX, then initialize executable stubs through the writable alias.
        // Gated on host page > guest page: when equal, any offset failing the test is one Linux itself EINVALs.
        if (r == MAP_FAILED && !(a3 & 0x10) && !(a3 & 0x20) && (int)a4 >= 0 && hp > (size_t)guest_pagesz() &&
            ((off_t)a5 & (off_t)(hp - 1))) {
            size_t head = (size_t)((off_t)a5 & (off_t)(hp - 1));
            off_t aligned_offset = (off_t)a5 - (off_t)head;
            size_t mapped_size = (size_t)a1 + head;
            void *base = mmap(NULL, mapped_size, prot, mmap_flags((int)a3), (int)a4, aligned_offset);
            if (base != MAP_FAILED) {
                physical_mapping = base;
                physical_mapping_size = (mapped_size + hp - 1) & ~(hp - 1);
                r = (char *)base + head;
                off_emul = 0;
            }
        }
        // Past-EOF tail zero-fill. A file mmap whose length runs past the file's end leaves the trailing
        // WHOLE pages with no backing: the host SIGBUSes on any read of them. ld.so does exactly this -- it maps
        // a .so's WHOLE vaddr span from the FIRST segment, so the inter-segment bytes become such past-EOF
        // pages. On Linux they are equally unbacked, but ld.so PROT_NONEs / replaces that region and never
        // reads it; where the host page is coarser, though, a later 4 KB-granular segment map (x86_64 .so p_align
        // 0x1000) shares its low host page with one of those past-EOF pages, so a stray access SIGBUSes where
        // Linux stayed quiet (julia's libdl/libjulia abort here). Re-map the genuinely-past-EOF whole-page
        // tail as anonymous zero -- the inaccessible-but-quiet region Linux effectively presents -- so such a
        // shared host page reads back zero instead of faulting. The partial page straddling EOF keeps the host's
        // file bytes + zero-fill, a later MAP_FIXED segment map overwrites whatever it needs, and a fully
        // file-backed mapping (valid_end >= a1) is left byte-identical. RW only (an ANON PROT_EXEC map hits
        // macOS W^X EPERM; the JIT never executes guest pages anyway). MAP_PRIVATE only: a MAP_SHARED file
        // map past EOF can be made valid later by ftruncate-extending the file (sqlite/lmdb), so its tail
        // must stay the real shared mapping; ld.so's .so segments are all MAP_PRIVATE, so julia is covered.
        if (r != MAP_FAILED && !off_emul && !fixed286 && (a3 & 0x02) && !(a3 & 0x20) && (int)a4 >= 0 && a1) {
            struct stat st;
            if (fstat((int)a4, &st) == 0 && S_ISREG(st.st_mode)) {
                uint64_t avail = (uint64_t)st.st_size > a5 ? (uint64_t)st.st_size - (uint64_t)a5 : 0;
                uint64_t valid_end = (avail + hp - 1) & ~(uint64_t)(hp - 1); // first host page wholly past EOF
                if (valid_end < a1)
                    mmap((char *)r + valid_end, (size_t)(a1 - valid_end), PROT_READ | PROT_WRITE,
                         MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0);
            }
        }
        // Host-page-vs-guest-page MAP_FIXED reconciliation. macOS arm64 mmap REQUIRES a host-page-aligned
        // MAP_FIXED address, but x86_64 .so PT_LOADs are p_align=0x1000, so ld.so's fixed map of libc's text
        // EINVALs ("failed to map segment from shared object"). ld.so already reserved the range, so emulate
        // the map with a private ANON map at the host-page-rounded base + pread. Gated on host page > guest
        // page: on a 4 KB-page host a0 is always host-page aligned, so every MAP_FAILED here is a GENUINE
        // kernel verdict (ENOMEM, EACCES, MAP_FIXED_NOREPLACE EEXIST) this would turn into a bogus success.
        if (r == MAP_FAILED && (a3 & 0x10) && hp > (size_t)guest_pagesz()) {
            uint64_t lo = a0 & ~((uint64_t)hp - 1); // round the start DOWN to a host page
            size_t head = (size_t)(a0 - lo);        // bytes in the low page that belong to the PREVIOUS segment
            // The low page may also hold the tail of the previous PT_LOAD (a0 sits mid-host-page). The ANON
            // MAP_FIXED below zeros that whole page, so snapshot the neighbour's bytes FIRST and restore them
            // after -- they were already written (prev segment / ld.so's reservation) and must survive. (The
            // past-EOF tail fill above guarantees the head is now readable -- a real neighbour byte or quiet
            // zero -- never a SIGBUSing hole. The HIGH edge needs no save: bytes past a0+a1 belong to the
            // NEXT segment, which refills them via its own map, or are this segment's BSS -> read as zero.)
            void *hsave = head ? malloc(head) : NULL;
            if (hsave) memcpy(hsave, (void *)lo, head);
            // RW only: the JIT never executes guest pages, and an ANON PROT_EXEC map hits macOS W^X EPERM.
            void *ar =
                mmap((void *)lo, (size_t)a1 + head, PROT_READ | PROT_WRITE, MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0);
            if (ar != MAP_FAILED) {
                if (hsave) memcpy((void *)lo, hsave, head); // restore the previous seg's tail
                if (!(a3 & 0x20) && (int)a4 >= 0 && pread_retry((int)a4, (void *)a0, (size_t)a1, (off_t)a5) < 0) {
                    munmap(ar, (size_t)a1 + head);
                    ar = MAP_FAILED;
                }
                if (ar != MAP_FAILED) {
                    r = (void *)a0; // success: the mapping now lives at the requested fixed guest address
                    off_emul = 2;
                }
            }
            free(hsave);
        }
        if (bus_prepared) {
            if (r != MAP_FAILED) gbus_clear((uint64_t)(uintptr_t)r, (uint64_t)(uintptr_t)r + a1);
            if (r != MAP_FAILED &&
                gbus_add((uint64_t)(uintptr_t)r + bus_accessible, (uint64_t)(uintptr_t)r + a1) != 0) {
                munmap(r, (size_t)a1 + guard);
                r = MAP_FAILED;
                errno = ENOMEM;
            }
        } else if (mapping_prepared) {
            if (r != MAP_FAILED) gbus_clear((uint64_t)(uintptr_t)r, (uint64_t)(uintptr_t)r + a1 + guard);
        }
        if (logical_plan != NULL) {
            if (r != MAP_FAILED && off_emul == 2) {
                hl_logical_vma_commit_shared(logical_plan);
                logical_committed = 1;
            } else
                hl_logical_vma_abort_shared(logical_plan);
        }
        // refund
        if (r == MAP_FAILED && charge) {
            atomic_fetch_sub(&g_mem_charged, (uint64_t)a1);
            acct_publish_mem(); // publish the refunded charge into this process's cross-process slot
        }
        if (r != MAP_FAILED) {
            if (a3 & 0x10) {
                uint64_t replaced_first = (uint64_t)r;
                uint64_t replaced_last = replaced_first + mapped_length;
                hl_gmap_supersede_range(replaced_first, replaced_last);
                anon_split_unmap(replaced_first, replaced_last);
                filemap_unmap(replaced_first, replaced_last);
                futex_shared_unmap(replaced_first, replaced_last);
                wipefork_del(replaced_first, replaced_last - replaced_first);
                dontfork_del(replaced_first, replaced_last - replaced_first);
                hl_gmap_lock_remove(replaced_first, replaced_last - replaced_first);
            }
            if (!bus_prepared && !mapping_prepared) gbus_clear((uint64_t)r, (uint64_t)r + (uint64_t)a1 + guard);
            if (physical_mapping != NULL)
                hl_gmap_add_physical((uint64_t)r, mapped_length, (uint64_t)physical_mapping,
                                     (uint64_t)physical_mapping_size);
            else
                hl_gmap_add((uint64_t)r, mapped_length);         // track for execve() teardown
            hl_gmap_set_guest_length((uint64_t)r, (uint64_t)a1); // /proc maps report the guest length (sans guard)
            if (!(a3 & 0x20) && (int)a4 >= 0)
                filemap_register((uint64_t)r, (uint64_t)a1, (int)a4, (uint64_t)a5, (a3 & 0x01) != 0,
                                 off_emul == 2 && !logical_committed);
            // Shared-futex key (thread.c): a file-backed MAP_SHARED region (memfd/shm, mapped independently
            // by each peer at its own VA) must key its futex words by the shared object identity, not the VA,
            // so a cross-process/cross-mapping FUTEX_WAKE reaches a FUTEX_WAIT (Wall 7). Record its VA range
            // -> (dev,ino,offset). MAP_SHARED=0x01 (incl. MAP_SHARED_VALIDATE=0x03); anon (a3&0x20) has no
            // fd/inode and, when shared, is only ever fork-inherited at a COMMON VA (the VA key already works
            // there), so it is excluded. off_emul (a private-anon offset-fixup copy) is not the real shared
            // object -- skip it. Inert for every private mapping (the fast-path gate stays 0).
            if ((a3 & 0x01) && !(a3 & 0x20) && (!off_emul || logical_committed) && (int)a4 >= 0)
                futex_shared_register((uint64_t)r, (uint64_t)a1, (int)a4, (uint64_t)a5);
            // x86-TSO ordering must hold for ANY observer, and a MAP_SHARED region (file-backed OR anon --
            // shared anon is fork-inherited) can be read by a peer PROCESS. The translator elides guest
            // load/store barriers while the process looks single-threaded, but g_threaded is per-process and
            // therefore says nothing about a cross-process observer, so a shared mapping must force barriers
            // back on permanently (and flush the barrier-elided blocks). Covers both shared cases; inert for
            // every MAP_PRIVATE mapping and a no-op on the aarch64 frontend, which elides nothing.
            if (a3 & 0x01) (void)G_SHARED_MAP_BARRIERS();
            // mlockall(MCL_FUTURE): a mapping created while future-locking is armed must be wired resident on
            // creation (Linux mm/mlock.c). Best-effort (a RLIMIT_MEMLOCK refusal leaves it pageable); the
            // hl_gmap_lock_add records it so /proc Locked:/VmLck: reports the range under whole-map locking too.
            // MCL_FUTURE accounting: only wire+count the new mapping while it stays within RLIMIT_MEMLOCK.
            // A mapping that would push the locked total over the guest's limit is left pageable/uncounted
            // (the mmap still succeeds) so the tracked locked bytes never exceed the limit.
            if (hl_gmap_lock_future() && hl_gmap_lock_limit_range((uint64_t)r, (uint64_t)a1) == 0) {
                mlock(r, (size_t)a1 + guard);
                hl_gmap_lock_add((uint64_t)r, (uint64_t)a1);
            }
            // DONTNEED anon registry: record PRIVATE-ANON ranges (incl. the guard tail); for any other
            // (file-backed/shared) mapping, forget overlapping anon coverage -- a MAP_FIXED file map may
            // now sit where anon used to, and we must never anon-remap over it.
            if ((a3 & 0x20) && (a3 & 0x02)) {
                // Keep the private-anon registry's CURRENT protection in sync: anon_prot_if_contained
                // scans first-match, so a MAP_FIXED re-commit inside an EXISTING tracked reservation
                // (Go's sysReserve(PROT_NONE) -> sysMap(MAP_FIXED, RW) heap pattern) must rewrite the
                // overlapped subrange in place -- appending alone leaves the stale PROT_NONE record
                // shadowing it, and a later MADV_DONTNEED re-establishes the live heap as an
                // inaccessible reservation (the Go memclr SIGSEGV class). Mirrors the mprotect-commit
                // path above (mozjs/V8 GC chunks). Only a MAP_FIXED map can land inside an EXISTING
                // reservation and therefore need the in-place rewrite: a non-fixed map returns a
                // kernel-placed address that never overlaps a live (hence tracked) mapping, so its
                // update_prot is a pure no-op scan -- skip it. Gating on MAP_FIXED (0x10) keeps the
                // documented Go/V8 re-commit behavior byte-identical while removing the O(n) scan that
                // ran on every anon mmap and made an N-mapping guest O(n^2).
                if (a3 & 0x10) anon_update_prot((uint64_t)r, (uint64_t)a1 + guard, prot);
                anon_track((uint64_t)r, (uint64_t)a1 + guard, prot);
            } else
                anon_untrack((uint64_t)r, (uint64_t)a1 + guard);
            // A fresh mapping resets any prior MADV_WIPEONFORK marking on this address range (advice does
            // not survive the region being remapped) -- drop stale wipe coverage so a reused address is
            // never wrongly zeroed in a child.
            wipefork_del((uint64_t)r, (uint64_t)a1 + guard);
            dontfork_del((uint64_t)r, (uint64_t)a1 + guard); // reused address drops stale dont-fork marking
            // PROT_NONE registry (g_gna, thread.c; read INSIDE host_range_mapped). hl force-maps this region
            // host-RW, so a guest PROT_NONE mmap is really RW -- record the guest's REQUESTED prot so a
            // syscall buffer landing in it still EFAULTs (LTP read02); an accessible map clears stale coverage.
            {
                uint64_t glo = (uint64_t)r, ghi = ((uint64_t)r + (uint64_t)a1 + 0xfff) & ~(uint64_t)0xfff;
                if ((int)a2 == PROT_NONE)
                    gna_add(glo, ghi);
                else
                    gna_clear(glo, ghi);
                // Read-only registry (g_gro): a store into a read-only mapping must be delivered as a guest
                // SIGSEGV, but the x86 write-fault path treats an unrecorded faulting page as a lazy/SMC
                // page and silently re-opens it. mprotect records this; a MAP_FIXED PROT_READ overmap must
                // too, or the newly read-only page stays writable (map_over_existing ro_write_faults).
                if ((int)a2 != PROT_NONE && !((int)a2 & PROT_WRITE))
                    gro_add(glo, ghi);
                else
                    gro_clear(glo, ghi);
            }
        }
        /* Keep registry publication inside the same serialized mapping
           transaction as the host replacement.  gmap/anon/wipe/protection
           registries are process-global and are not independently locked. */
        if (bus_prepared) {
            hl_logical_vma_global_reclaim_quiescent();
            gbus_prepare_release();
        } else if (logical_transition_locked) {
            if (mapping_prepared) {
                hl_logical_vma_global_reclaim_quiescent();
                gbus_mapping_stw_end();
            }
            /* A failed/empty transaction leaves a valid empty soft snapshot.
               Retain guarded translation until whole-image reset rather than
               churning the global code cache across short-lived mappings. */
            gbus_mapping_transition_unlock();
        } else if (mapping_prepared) {
            hl_logical_vma_global_reclaim_quiescent();
            gbus_mapping_prepare_release();
        }
        // stale-translation: a MAP_FIXED mmap REPLACES whatever code lived at the destination VA, so drop any
        // translations cached for it (Linux MAP_FIXED implicitly unmaps the range first).
        if (r != MAP_FAILED && (a3 & 0x10 /*MAP_FIXED*/))
            G_SMC_UNMAP((uint64_t)(uintptr_t)r, (uint64_t)(uintptr_t)r + a1);
        if (logical_prepare_failed) errno = logical_prepare_errno;
        G_RET(c) = (r == MAP_FAILED) ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // mprotect
    case 226: // mprotect: NO-OP for physical page protection (JIT never executes guest pages; a real
        // mprotect is harmful on macOS -- would fault the guest's own RELRO writes). The g_gna PROT_NONE
        // registry tracks the guest's INTENT (reserve PROT_NONE -> commit RW) so buffer checks EFAULT.
        if (a1) {
            // ET_EXEC addresses remain LOW in the Linux ABI while their storage is mapped at the engine's
            // HIGH bias. Keep all logical permission registries in guest coordinates, but use this translated
            // address for mapping validation and any safe host-side protection change.
            uint64_t physical_a0 = nonpie_fold(a0);
            // Linux mm/mprotect.c rejects a start not aligned to the (guest) page size with EINVAL BEFORE
            // touching anything, so a bad-alignment probe must not read as success.
            if (a0 & (uint64_t)(guest_pagesz() - 1)) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (a2 & ~(uint64_t)(PROT_READ | PROT_WRITE | PROT_EXEC | 0x01000000 /* PROT_GROWSDOWN */ |
                                 0x02000000 /* PROT_GROWSUP */)) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Linux mm/mprotect.c then walks the VMAs and returns ENOMEM if the range has a hole (any page
            // not backed by a mapping) -- an mprotect of a page-aligned but unmapped range must NOT read as a
            // fake success. Reject a range that is neither a tracked guest mapping (gmap -- covers ELF image /
            // stack / brk / anon+file mmap, INCLUDING a guest PROT_NONE reservation, which the host_range_mapped
            // probe would call unmapped) nor physically mapped host-side. Same regression-free idiom the mremap
            // source validation (case 216) uses: a hot mprotect on the guest's own memory hits hl_gmap_contains
            // (no probe cost, no false ENOMEM); only a genuinely unmapped range is rejected.
            // NON-PIE: the ET_EXEC image is force-mapped HIGH (addr+g_nonpie_bias, __PAGEZERO forbids the low
            // 4 GB) but the guest still names its image by the LOW link vaddr -- static glibc's RELRO
            // mprotect(_dl_protect_relro) passes that low address, which is mapped only at the rebased VA. So
            // a low-range miss must re-check at nonpie_fold(a0) before ENOMEM (inert for PIE: fold == a0).
            if (!hl_gmap_contains(a0, (uint64_t)a1) && !host_range_mapped((uintptr_t)a0, (size_t)a1)) {
                if (physical_a0 == a0 || (!hl_gmap_contains(physical_a0, (uint64_t)a1) &&
                                          !host_range_mapped((uintptr_t)physical_a0, (size_t)a1))) {
                    G_RET(c) = (uint64_t)(-ENOMEM);
                    break;
                }
            }
            uint64_t glo = a0 & ~(uint64_t)0xfff, ghi = (a0 + a1 + 0xfff) & ~(uint64_t)0xfff;
            int logical_protect_prepared = 0;
            int logical_protect_locked = 0;
            int logical_protect_failed = 0;
            hl_logical_vma_plan *logical_protect_plan = NULL;
            uint64_t folded_alias_a0 = nonpie_unfold(physical_a0);
            if (jit_guest_soft_active() || physical_a0 != a0) {
                gbus_mapping_transition_lock();
                logical_protect_locked = 1;
                if (!jit_guest_soft_active() && !jit_guest_soft_activate()) {
                    gbus_mapping_transition_unlock();
                    G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
                    break;
                }
                uint64_t logical_ranges[3] = {a0, physical_a0, folded_alias_a0};
                for (size_t logical_index = 0; logical_index < 3; ++logical_index) {
                    uint64_t logical_a0 = logical_ranges[logical_index];
                    int duplicate = 0;
                    for (size_t earlier = 0; earlier < logical_index; ++earlier)
                        if (logical_ranges[earlier] == logical_a0) duplicate = 1;
                    if (duplicate || !hl_logical_vma_global_overlap(logical_a0, a1)) continue;
                    if (!logical_protect_prepared) {
                        gbus_mapping_stw_begin();
                        logical_protect_prepared = 1;
                    }
                    if (hl_logical_vma_global_prepare_protect(logical_a0, a1, (uint32_t)a2, &logical_protect_plan) !=
                        0) {
                        int saved = errno;
                        if (logical_protect_plan != NULL) hl_logical_vma_abort_shared(logical_protect_plan);
                        gbus_mapping_stw_end();
                        gbus_mapping_transition_unlock();
                        G_RET(c) = (uint64_t)(int64_t)-saved;
                        logical_protect_failed = 1;
                        break;
                    }
                }
                if (logical_protect_failed) break;
                if (physical_a0 != a0 && !logical_protect_prepared) {
                    gbus_mapping_stw_begin();
                    logical_protect_prepared = 1;
                }
                if (physical_a0 != a0 && hl_logical_vma_global_prepare_direct(
                                             physical_a0, a1, (uint32_t)a2, physical_a0, &logical_protect_plan) != 0) {
                    int saved = errno;
                    if (logical_protect_plan != NULL) hl_logical_vma_abort_shared(logical_protect_plan);
                    gbus_mapping_stw_end();
                    gbus_mapping_transition_unlock();
                    G_RET(c) = (uint64_t)(int64_t)-saved;
                    break;
                }
            }
            // Making translated code writable is itself the guest's declaration that the bytes may change.
            // Drop translations while the SMC page is still tracked, before the host protection below makes
            // the store silent. This also covers 4K guest subpages where a 16K host mprotect is unsafe: the
            // later lazy write fault may open the host page, but the stale translation is already gone.
            if ((int)a2 & PROT_WRITE) G_SMC_UNMAP(physical_a0, physical_a0 + a1);
#if defined(__linux__)
            // On a Linux host the guest and host VM page granularities match.  Apply the transition for
            // real: managed runtimes reserve file-backed PROT_NONE arenas (commonly /dev/zero) and commit
            // individual pages with mprotect before their first store.  Merely clearing gna returned success
            // while leaving the host page PROT_NONE, so CoreCLR faulted on that first committed write.
            // Guest EXEC is data to this DBT; keep it host-readable for translation and omit host execution.
            int host_protection = (int)a2;
            if (host_protection & PROT_EXEC) host_protection = (host_protection | PROT_READ) & ~PROT_EXEC;
            if (mprotect((void *)(uintptr_t)physical_a0, (size_t)a1, host_protection) != 0) {
                int saved = errno;
                if (logical_protect_plan != NULL) {
                    hl_logical_vma_abort_shared(logical_protect_plan);
                    logical_protect_plan = NULL;
                }
                if (logical_protect_prepared) {
                    hl_logical_vma_global_reclaim_quiescent();
                    gbus_mapping_stw_end();
                }
                if (logical_protect_locked) gbus_mapping_transition_unlock();
                G_RET(c) = (uint64_t)(int64_t)-saved;
                break;
            }
            // Keep the private-anon registry's CURRENT protection in sync: MADV_DONTNEED re-establishes
            // a tracked range with the recorded prot, so a committed (mprotect'd RW) subrange of a
            // PROT_NONE reservation must not be remapped back to PROT_NONE (mozjs/V8 GC chunks:
            // reserve NONE -> commit RW -> DONTNEED -> store faulted).
            anon_update_prot(physical_a0 & ~(uint64_t)0xfff,
                             ((physical_a0 + a1 + 0xfff) & ~(uint64_t)0xfff) - (physical_a0 & ~(uint64_t)0xfff),
                             host_protection);
#elif defined(__APPLE__)
            // The ELF loader can physically narrow an independently aligned PT_LOAD segment to read-only.
            // A later guest mprotect that adds WRITE must therefore reopen the backing host VM page; updating
            // only GRO/GNA leaves CoreCLR's relocation target physically read-only and the first store dies in
            // Darwin before the Linux permission model can see it. Guest pages are 4K while Apple-silicon host
            // pages are 16K, so widen the host operation outwards. The adjacent guest subpages remain protected
            // logically by GRO/GNA; translated guest bytes are data and never execute from this mapping.
            if ((int)a2 & PROT_WRITE) {
                size_t host_page = hl_host_page_size();
                uint64_t host_lo = physical_a0;
                uint64_t host_hi = physical_a0 + a1;
                if (host_page != 0 && (host_page & (host_page - 1)) == 0) {
                    host_lo &= ~((uint64_t)host_page - 1);
                    host_hi = (host_hi + host_page - 1) & ~((uint64_t)host_page - 1);
                }
                if (host_page == 0 || host_hi < host_lo ||
                    mprotect((void *)(uintptr_t)host_lo, (size_t)(host_hi - host_lo), PROT_READ | PROT_WRITE) != 0) {
                    int saved = host_page == 0 ? EINVAL : errno;
                    if (logical_protect_plan != NULL) {
                        hl_logical_vma_abort_shared(logical_protect_plan);
                        logical_protect_plan = NULL;
                    }
                    if (logical_protect_prepared) {
                        hl_logical_vma_global_reclaim_quiescent();
                        gbus_mapping_stw_end();
                    }
                    if (logical_protect_locked) gbus_mapping_transition_unlock();
                    G_RET(c) = (uint64_t)(int64_t)-saved;
                    break;
                }
            }
#endif
            /* Publish guest-visible permission registries only after every
               fallible host operation succeeded.  Otherwise a failed
               mprotect would return an error while leaving syscall uaccess
               and write-fault classification at the rejected protection. */
            if ((int)a2 == PROT_NONE)
                gna_add(glo, ghi);
            else
                gna_clear(glo, ghi);
            uint64_t alias_glo = folded_alias_a0 & ~(uint64_t)0xfff;
            uint64_t alias_ghi = (folded_alias_a0 + a1 + 0xfff) & ~(uint64_t)0xfff;
            if (alias_glo != glo) {
                if ((int)a2 == PROT_NONE)
                    gna_add(alias_glo, alias_ghi);
                else
                    gna_clear(alias_glo, alias_ghi);
            }
            if ((int)a2 != PROT_NONE && !((int)a2 & PROT_WRITE)) {
                gro_add(glo, ghi);
                if (alias_glo != glo) gro_add(alias_glo, alias_ghi);
                filemap_refresh_emulated(physical_a0, physical_a0 + a1);
            } else {
                gro_clear(glo, ghi);
                if (alias_glo != glo) gro_clear(alias_glo, alias_ghi);
            }
            if (logical_protect_prepared) {
                hl_logical_vma_commit_shared(logical_protect_plan);
                logical_protect_plan = NULL;
                hl_logical_vma_global_reclaim_quiescent();
                gbus_mapping_stw_end();
            }
            if (logical_protect_locked) gbus_mapping_transition_unlock();
            // Guest permissions remain logical. Translated guest bytes are host data, and a physical
            // mprotect would turn ordinary guest guard/safepoint accesses into Darwin faults before the
            // Linux permission model can classify them. The SMC machinery independently protects translated
            // source pages when it needs a write trap.
            // #423 / H9: a guest that mprotect()s a page to add PROT_EXEC is a JIT toggling an
            // already-written page executable -- the mmap(RW) -> write code -> mprotect(RX) pattern that
            // .NET/Wasm/managed runtimes use (as opposed to the RWX mmap case 222 already covers). It MUST
            // arm SMC the same way case 222 does: setting g_rwx_guest makes smc_protect() (G_AFTER_TRANSLATE,
            // dispatch.h) write-protect each translated source page, so a later overwrite -- the
            // mprotect(RW) + rewrite + mprotect(RX) re-toggle -- traps in jit86_lazyguard -> smc_on_write()
            // drops the stale translation and the new bytes re-translate. Without this the FIRST RX
            // translation is cached forever -> silent miscompile. This mprotect stays a physical no-op
            // (the SMC machinery does its own host mprotect on the code page); only the gate is set.
            // g_rwx_guest latches -- once a JIT guest is present it stays armed across every re-toggle, so
            // SMC coverage is kept, not lost, on a subsequent mprotect(RW)->mprotect(RX). NORWXFIX=1
            // disables, mirroring case 222.
            if ((int)a2 & PROT_EXEC) g_rwx_guest = 1;
        }
        G_RET(c) = 0;
        break;
    case 227: // msync: stores through a MAP_SHARED mapping are already in the unified page cache, so the
        // file is coherent without an explicit flush; treat as success (avoids a spurious -ENOSYS).
        // Default/fast/none keep the no-op (page-cache coherent). Only `strict` issues a real host
        // msync for on-platter writeback durability, translating Linux MS_* flags to macOS (macOS
        // MS_SYNC=16 != Linux 4; MS_ASYNC=1/MS_INVALIDATE=2 match), tolerating EINVAL.
        // Linux validates the flags BEFORE any writeback (mm/msync.c): an unknown bit, or MS_SYNC and
        // MS_ASYNC both set (they are mutually exclusive), is -EINVAL. Emulate that here so the no-op
        // fast path still rejects a malformed flag word exactly as the kernel does (LTP msync surface).
        // Linux values: MS_ASYNC=1, MS_INVALIDATE=2, MS_SYNC=4.
        if (((int)a2 & ~(0x1 | 0x2 | 0x4)) || (((int)a2 & 0x1) && ((int)a2 & 0x4))) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // Linux (mm/msync.c) walks the VMAs after the flag check and returns -ENOMEM if the range contains
        // an unmapped hole -- msync of a stale/never-mapped range must NOT read as a fake success. Reject a
        // range that is neither a tracked guest mapping nor physically mapped host-side, using the same
        // hole-detection idiom as mprotect (case 226). len 0 is a Linux no-op success.
        if (a1 && !hl_gmap_contains(a0, (uint64_t)a1) && !host_range_mapped((uintptr_t)a0, (size_t)a1)) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        filemap_refresh_emulated(a0, a0 + a1);
        G_RET(c) = 0;
        break;
    // mlock(addr,len): wire+fault via macOS mlock so the range is RESIDENT (LTP mincore03), AND track the
    // range so the guest observes the lock STATE back through /proc/self/{smaps Locked:, status VmLck:}
    // (LTP mlock05). A host mlock failure (RLIMIT_MEMLOCK exhausted / EPERM / ENOMEM) is REAL -- the pages
    // are NOT wired -- so return -errno instead of swallowing it: a crypto/RT guest that relies on mlock to
    // keep key material out of swap/core dumps must SEE the failure (a fake success left its "locked" pages
    // swappable, and we must never report the range locked when it isn't). len 0 is a Linux success no-op.
    case 228: {
        // Honor the guest's RLIMIT_MEMLOCK first (the container is unprivileged: no CAP_IPC_LOCK) -- soft
        // limit 0 -> EPERM, exceeding the limit -> ENOMEM, before touching the host wiring.
        int rl = hl_gmap_lock_limit_range(a0, (uint64_t)a1);
        if (rl < 0) {
            G_RET(c) = (uint64_t)(int64_t)rl;
            break;
        }
        if (a1 && mlock((void *)a0, (size_t)a1) != 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        hl_gmap_lock_add(a0, (uint64_t)a1);
        G_RET(c) = 0;
        break;
    }
    case 229: // munlock: unwire + drop the tracked range. A host munlock failure is returned as -errno
        // (rather than a false success) so the guest sees Linux's error; len 0 is a success no-op.
        if (a1 && munlock((void *)a0, (size_t)a1) != 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        hl_gmap_lock_remove(a0, (uint64_t)a1);
        G_RET(c) = 0;
        break;
    // Container-init compat: in the single-process model these are no-ops that return success so
    // entrypoints (mount /proc, unshare, drop caps, set hostname) proceed; the path-jail is the
    // real boundary, and a faked namespace grants no actual privilege (program still runs as our uid).
    // mincore: report page residency. The host mincore(2) fills one status byte per HOST page; Linux
    // wants one byte per page with bit0 = resident. macOS sets MINCORE_INCORE(0x1) in bit0 already, so
    // masking each byte to bit0 yields the Linux convention. (Host pages may be coarser than the guest's 4 KB,
    // so sub-host-page granularity is coarser than a real 4 KB kernel, but residency of the covering
    // page is faithful.) Untouched trailing bytes (the guest zero-filled its vector) stay 0 = absent.
    case 232: {
        size_t hps = hl_linux_host_map_granularity(); // 16 KB on Apple Silicon, 64 KB on Windows
        size_t gps = guest_pagesz();                  // page size the GUEST believes in (AT_PAGESZ: 4 KB on both ISAs)
        size_t len = (size_t)a1;
        // Linux mincore requires a page-aligned start address -> EINVAL otherwise (align to the GUEST page
        // so a valid 4 KB-granular start is not rejected on a coarser host page).
        if (a0 & (gps - 1)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // The engine can retain a host mapping after a guest-logical munmap (for example a subrange sharing
        // a larger host page).  Host mincore would report that retained backing as resident even though Linux
        // must reject a range containing any logically unmapped guest page with ENOMEM.  gmap is the complete
        // guest mapping ledger (image, stack, brk, anonymous and file mappings), so validate against it before
        // asking the host about residency.
        if (len && !hl_gmap_contains(a0, (uint64_t)len)) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOMEM);
            break;
        }
        // Linux: `vec` must be a writable buffer of ceil(len/pagesize) bytes; a NULL or inaccessible vec is
        // EFAULT. Validate against GUEST protections up front (both paths), because hl force-maps guest
        // PROT_NONE pages host-writable -- so a raw host mincore would happily scribble a guest guard page
        // (aarch64 fast path) and the slow path skipped the check entirely when a2==NULL (x86 null-vec).
        // len==0 is a success no-op regardless of vec, matching the kernel.
        if (len) {
            size_t ps = gps ? gps : hps;
            size_t needp = (len + ps - 1) / ps;
            if (!a2 || guest_bad_ptr((uintptr_t)a2, needp)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        // Fast path: guest page == host page -- the host vec is already one byte per guest page.
        if (gps == hps || gps == 0 || len == 0) {
            size_t npages = len ? (len + hps - 1) / hps : 0;
            unsigned char stackvec[1024], *vec = stackvec;
            if (npages > sizeof stackvec) {
                vec = malloc(npages);
                if (!vec) {
                    G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
                    break;
                }
            }
            int r = mincore((void *)a0, len, (char *)vec);
            if (r == 0)
                for (size_t i = 0; i < npages; i++)
                    vec[i] &= 1u;
            if (r == 0 && npages && guest_copy_to(a2, vec, npages) != (ssize_t)npages) {
                if (vec != stackvec) free(vec);
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
            }
            if (vec != stackvec) free(vec);
            G_RET(c) = (r < 0) ? (uint64_t)(-errno) : 0;
            break;
        }
        // Guest pages SMALLER than host pages (x86_64: 4 KB guest vs 16 KB host). The host mincore fills
        // one status byte per 16 KB page, but the guest allocated ceil(len/4KB) bytes and indexes them at
        // 4 KB granularity -- so writing the host-granular vector directly leaves 3 of every 4 guest-page
        // slots at 0 (the x86 under-report). Run mincore into a host-granular scratch buffer, then
        // project each guest page's residency from the host page that physically covers it.
        size_t hpages = (len + hps - 1) / hps;
        size_t gpages = (len + gps - 1) / gps;
        size_t per = hps / gps; // guest pages per host page (4 on a 16 KB host)
        unsigned char stackbuf[1024], *hv = stackbuf;
        if (hpages > sizeof stackbuf) {
            hv = (unsigned char *)malloc(hpages);
            if (!hv) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
        }
        int r = mincore((void *)a0, len, (char *)hv);
        if (r == 0 && a2) {
            // The host mincore filled our scratch `hv`; the guest vector at a2 is written DIRECTLY by the
            // engine (one byte per guest page). Validate it before the projection loop so a bad/unmapped
            // pointer returns -EFAULT instead of faulting the engine -- the fast path above lets the
            // host mincore fault a2 itself, but this slow path never hands a2 to a host syscall.
            unsigned char *vec = malloc(gpages ? gpages : 1);
            if (!vec) {
                if (hv != stackbuf) free(hv);
                G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
                break;
            }
            for (size_t i = 0; i < gpages; i++) {
                size_t h = per ? i / per : i;
                vec[i] = (h < hpages) ? (unsigned char)(hv[h] & 1u) : 0;
            }
            if (guest_copy_to(a2, vec, gpages) != (ssize_t)gpages) {
                free(vec);
                if (hv != stackbuf) free(hv);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            free(vec);
        }
        if (hv != stackbuf) free(hv);
        G_RET(c) = (r < 0) ? (uint64_t)(-errno) : 0;
        break;
    }
    case 233: {
        // madvise: best-effort, advisory (never fail the guest). Only forward advice values whose
        // meaning is identical on both kernels -- NORMAL/RANDOM/SEQUENTIAL/WILLNEED/DONTNEED(0..4)
        // match, and Linux MADV_FREE(8) -> macOS MADV_FREE. Every OTHER Linux advice number collides
        // with an unrelated macOS one (e.g. Linux DONTFORK=10 vs macOS PAGEOUT=10), so no-op those.
        // (Note: macOS MADV_DONTNEED does not zero anonymous pages the way Linux's does.)
        int adv = (int)a2, hadv = -1;
        // Linux validates the advice value and start alignment BEFORE any work (mm/madvise.c). An advice
        // number the kernel does not define, or a start not aligned to the guest page size, is EINVAL --
        // otherwise a bad feature probe reads hl's best-effort no-op as success. (Valid Linux advice:
        // 0..4, 8..23, 25, 100, 101.)
        {
            int ok = (adv >= 0 && adv <= 4) || (adv >= 8 && adv <= 23) || adv == 25 || adv == 100 || adv == 101;
            if (!ok) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (a0 & (uint64_t)(guest_pagesz() - 1)) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // MADV_WIPEONFORK(18) / MADV_KEEPONFORK(19): valid ONLY on private-anon ranges (Linux EINVALs
        // otherwise). WIPEONFORK records the range so the fork child sees it zero-filled
        // (fork_child_hooks -> wipefork_apply_child); KEEPONFORK undoes that by dropping the range.
        // A zero length is a no-op success (nothing to mark). Not forwarded to the host: macOS has no
        // such advice, and the effect is realized in our own fork path.
        if (adv == 18 || adv == 19) {
            if (a1 == 0) {
                G_RET(c) = 0;
                break;
            }
            if (anon_prot_if_contained(a0, (size_t)a1) < 0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (adv == 18)
                wipefork_add(a0, (size_t)a1);
            else
                wipefork_del(a0, (size_t)a1);
            G_RET(c) = 0;
            break;
        }
        // MADV_REMOVE(9): punch a hole (zero the backing store) in a SHARED file-backed mapping -- the
        // zeros show through the mapping and via pread -- and EINVAL on a mapping that is not shmem/shared
        // file backed (a private or anonymous range). hl force-maps the guest region with the SAME kind of
        // real host mapping (a genuine MAP_SHARED file map, or a MAP_PRIVATE|ANON reservation), and the host
        // and guest page granularities match here, so the host kernel's own MADV_REMOVE gives exactly the
        // Linux verdict: it punches the shared file map and returns EINVAL for private/anon. Forward it and
        // surface the real errno instead of the advisory no-op the generic tail would return.
        if (adv == 9) {
            if (a1 == 0) {
                G_RET(c) = 0;
                break;
            }
#if defined(MADV_REMOVE)
            G_RET(c) = madvise((void *)a0, (size_t)a1, MADV_REMOVE) == 0 ? 0 : (uint64_t)(int64_t)(-errno);
#else
            /* Darwin has no MADV_REMOVE equivalent. In particular, passing
             * Linux's numeric value through would select an unrelated host
             * advice. Fail closed until the mapping layer can punch the
             * backing file through its owned descriptor. */
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
#endif
            break;
        }
        // MADV_DONTNEED(4): Linux drops the pages so the NEXT access faults in fresh ZERO pages. macOS
        // MADV_DONTNEED does not zero anon pages, so a reread would return stale data (breaks
        // redis/jemalloc, which lean on the zeroing). For a range fully inside a tracked PRIVATE-ANON
        // region we re-establish it with a fresh MAP_FIXED|MAP_ANON|MAP_PRIVATE mapping -> next read
        // faults in zeros. File-backed/shared mappings are NEVER touched here (the containment check
        // fails for them); they keep the safe advisory passthrough below.
        if (adv == 4 && a1) {
            int aprot = anon_prot_if_contained(a0, (size_t)a1);
            if (aprot >= 0) {
                // emulate Linux MADV_DONTNEED (range reads back ZERO) WITHOUT corrupting a live
                // neighbour that shares a host page. The guest uses 4 KB pages; the host page may be coarser.
                // A plain `mmap(a0, a1, MAP_FIXED|ANON)` rounds a partial head/tail host page OUT to the full
                // host page, so a guest DONTNEED of a free 4/8 KB span silently unmaps+zeros a LIVE object in the
                // rest of that host page (Go's scavenger DONTNEEDs a free 8 KB span whose 16 KB host page also
                // holds a live tiny string span -> the "heap corruption"). Fix: MAP_FIXED-remap only the
                // host-page-aligned INTERIOR (safe physical release + zero); zero the partial edge host pages
                // with memset over EXACTLY the requested bytes, never remapping a page shared with a neighbour.
                size_t hp = hl_linux_host_map_granularity();
                uint64_t lo = a0, hi = a0 + a1;
                uint64_t ilo = (lo + hp - 1) & ~((uint64_t)hp - 1); // first fully-covered host page
                uint64_t ihi = hi & ~((uint64_t)hp - 1);            // end of last fully-covered host page
                int done = 1;
                if (ilo < ihi) { // release the fully-covered interior (drop physical + fault back zero)
                    if (mmap((void *)ilo, (size_t)(ihi - ilo), aprot, MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0) ==
                        MAP_FAILED)
                        done = 0;
                }
                if (done && (aprot & PROT_WRITE)) { // zero the partial edges in place -- neighbours untouched
                    uint64_t he = ilo < hi ? ilo : hi;
                    if (lo < he) memset((void *)lo, 0, (size_t)(he - lo));
                    uint64_t tl = ihi > lo ? ihi : lo;
                    if (tl < hi) memset((void *)tl, 0, (size_t)(hi - tl));
                    G_RET(c) = 0;
                    break;
                }
                if (done && ilo < ihi) { // interior released; edges not writable -> best effort, don't fail
                    G_RET(c) = 0;
                    break;
                }
                // could not satisfy exactly -> never fail the guest; fall through to advisory
            }
        }
        // MADV_DONTFORK(10) / MADV_DOFORK(11): the marked range must NOT be inherited by a fork child (a
        // child touch faults), DOFORK undoes it. A guest fork re-establishes the child's guest memory
        // itself rather than relying on host VMA inheritance, so forwarding the advice to the host kernel
        // is inert; instead track the range (like MADV_WIPEONFORK) and unmap it in the fork child, so the
        // child faults exactly as Linux's VM_DONTCOPY would make it. Valid on private-anon ranges here;
        // other mappings keep the advisory no-op. A zero length is a no-op success.
        if (adv == 10 || adv == 11) {
            if (a1 == 0) {
                G_RET(c) = 0;
                break;
            }
            if (anon_prot_if_contained(a0, (size_t)a1) >= 0) {
                if (adv == 10)
                    dontfork_add(a0, (size_t)a1);
                else
                    dontfork_del(a0, (size_t)a1);
            }
            G_RET(c) = 0;
            break;
        }
        if (adv >= 0 && adv <= 4)
            hadv = adv;
        else if (adv == 8)
            hadv = MADV_FREE;
        if (hadv >= 0 && madvise((void *)a0, (size_t)a1, hadv) < 0) { /* advisory: ignore */
        }
        G_RET(c) = 0;
        break;
    }
    // process_vm_readv: copy FROM the remote iovecs (a3/a4) INTO the local iovecs (a1/a2). Same address
    // space here, so it's a direct scatter/gather memcpy (the remote pid in a0 is the guest itself).
    // when the remote pid is a DIFFERENT (traced, stopped) guest process -- strace reads a tracee's
    // syscall-string args this way -- route to the ptrace cross-process path (the remote lives in another
    // host address space, so a direct memcpy would read OUR own COW copy). ptrace_pvm returns >=0 bytes /
    // -errno when it owns the call, or INT_MIN to say "not a traced remote -> use the same-space memcpy".
    case 270: {
        // flags (a5) must be 0 -- no process_vm_readv flags are defined, and the kernel rejects any non-zero
        // value with EINVAL BEFORE it touches the iovecs (mm/process_vm_access.c). A silent accept here let a
        // probe with a junk flag read as supported.
        if (a5) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        // The iovec arrays themselves are guest buffers the kernel reads via copy_from_user: cap the count at
        // UIO_MAXIOV (EINVAL) and reject an unmapped array (EFAULT) so a bad pointer never faults the engine.
        if ((unsigned long)a2 > 1024 || (unsigned long)a4 > 1024) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        struct iovec local_iov[1024], remote_iov[1024];
        if (guest_iov_import(a1, (size_t)a2, local_iov) < 0 || guest_iov_import(a3, (size_t)a4, remote_iov) < 0) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        long pr = ptrace_pvm(c, 0, (pid_t)(int)a0, local_iov, (unsigned long)a2, remote_iov, (unsigned long)a4);
        if (pr != PT_PVM_LOCAL) {
            G_RET(c) = (uint64_t)pr;
            break;
        }
        G_RET(c) = (uint64_t)svc_vm_iov_copy(local_iov, (unsigned long)a2, remote_iov, (unsigned long)a4);
        break;
    }
    // process_vm_writev: the mirror -- copy FROM the local iovecs (a1/a2) INTO the remote iovecs (a3/a4).
    case 271: {
        // flags (a5) must be 0, rejected with EINVAL before the iovecs are read (mirrors process_vm_readv).
        if (a5) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        if ((unsigned long)a2 > 1024 || (unsigned long)a4 > 1024) {
            G_RET(c) = (uint64_t)(int64_t)-EINVAL;
            break;
        }
        struct iovec local_iov[1024], remote_iov[1024];
        if (guest_iov_import(a1, (size_t)a2, local_iov) < 0 || guest_iov_import(a3, (size_t)a4, remote_iov) < 0) {
            G_RET(c) = (uint64_t)(int64_t)-EFAULT;
            break;
        }
        long pr = ptrace_pvm(c, 1, (pid_t)(int)a0, local_iov, (unsigned long)a2, remote_iov, (unsigned long)a4);
        if (pr != PT_PVM_LOCAL) {
            G_RET(c) = (uint64_t)pr;
            break;
        }
        G_RET(c) = (uint64_t)svc_vm_iov_copy(remote_iov, (unsigned long)a4, local_iov, (unsigned long)a2);
        break;
    }
    // membarrier: CMD_QUERY(0) returns the bitmask of supported commands; the barrier commands issue a
    // process-wide full memory barrier. The host is cache-coherent and a seq-cst fence orders all threads,
    // so every (expedited or not, global or private) barrier is satisfied by a single host fence. The
    // REGISTER_* commands only arm the kernel's per-mm expedited fast path -- there is nothing to register
    // here, so they succeed as a no-op. SYNC_CORE variants additionally guarantee instruction-cache
    // coherence for self-modifying code; the guest's own JIT already flushes via its code-patch path, so a
    // fence suffices. glibc/Go/HAProxy probe QUERY, then REGISTER_PRIVATE_EXPEDITED(16) + PRIVATE_EXPEDITED(8).
    case 283:
        switch ((int)a0) {
        case 0: // CMD_QUERY -> bitmask of supported commands (every command we accept below)
            G_RET(c) = (1u << 0) | (1u << 1) | (1u << 2) | (1u << 3) | (1u << 4) | (1u << 5) | (1u << 6);
            break;
        case 1:  // CMD_GLOBAL
        case 2:  // CMD_GLOBAL_EXPEDITED
        case 8:  // CMD_PRIVATE_EXPEDITED
        case 32: // CMD_PRIVATE_EXPEDITED_SYNC_CORE
            atomic_thread_fence(memory_order_seq_cst);
            G_RET(c) = 0;
            break;
        case 4:           // CMD_REGISTER_GLOBAL_EXPEDITED
        case 16:          // CMD_REGISTER_PRIVATE_EXPEDITED
        case 64:          // CMD_REGISTER_PRIVATE_EXPEDITED_SYNC_CORE
            G_RET(c) = 0; // arm the expedited fast path -> nothing to do in this coherent DBT
            break;
        default: G_RET(c) = (uint64_t)(-EINVAL); break;
        }
        break;
    default: return 0;
    }
    return 1;
}
