// Extracted from service(): Memory — mmap/brk/mprotect/madvise syscalls. Returns 1 if nr was handled, 0 otherwise.
// Included by service.c after service/helpers.c, before service() — same TU scope (globals + helpers).
// process_vm_readv/writev between two iovec arrays. In this single-address-space DBT the "remote"
// process is always the guest itself, so both vectors point into directly-dereferenceable guest memory
// and the transfer is a scatter/gather memcpy -- exactly the kernel's stream semantics: bytes flow from
// the src vectors into the dst vectors in order, stopping when either side is exhausted. Returns the
// number of bytes copied.
// Is guest range [a,a+len) inaccessible to a userspace read/write, the way the Linux kernel would EFAULT?
// TWO cases dd must catch: (1) not mapped at all, and (2) mapped but PROT_NONE — dd force-maps guest anon
// memory host-RW so mprotect can stay a near-noop, which discards the guest's PROT_NONE intent, so a guest
// guard page (LTP tst_get_bad_addr = mmap(PROT_NONE)) stays host-readable. Both are handled by
// host_range_mapped: it rejects the wrap/unmapped case AND queries the single PROT_NONE registry g_gna
// (gna_hit, thread.c; fed by mmap/mprotect/munmap) up front. ONE helper so connect/bind/pselect/ppoll/...
// all agree (consolidates three agents' duplicates).
static int guest_bad_ptr(uintptr_t a, size_t len) {
    return !host_range_mapped(a, len);
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
            memcpy((char *)dst[di].iov_base + doff, (char *)src[si].iov_base + soff, n);
            total += (ssize_t)n;
            doff += n;
            soff += n;
        }
        if (doff == dst[di].iov_len) di++, doff = 0;
        if (soff == src[si].iov_len) si++, soff = 0;
    }
    return total;
}

// True if [addr,addr+len) lies entirely within a single tracked GUEST mapping (the gmap registry that
// records every guest mmap). Used to confine the cage-hint honoring in case 222 to memory the guest
// itself reserved -- never the engine's own internal allocations.
static int gmap_contains(uint64_t addr, uint64_t len) {
    uint64_t end = addr + len;
    for (int i = 0; i < g_ngmap; i++)
        if (g_gmap[i].addr <= addr && end <= g_gmap[i].addr + g_gmap[i].len) return 1;
    return 0;
}

// A munmap of [ustart,uend) removed those bytes from the address space; keep the gmap registry
// consistent with what is STILL mapped. A full-cover unmap drops the whole entry (execve teardown no
// longer needs it); a PARTIAL unmap must NOT drop the survivors -- otherwise the still-mapped head/tail
// leaks past execve() teardown and gmap_find_len (the mremap in-place-grow path, case 216) can no longer
// size it. So for each overlapping tracked mapping keep the surviving sub-region(s): resize the entry to
// its head (tail unmap) or its tail (head unmap), and for a middle unmap add a second entry for the tail.
// The 64 KB guard tail the anon mmap reserved is just the region's last bytes, so it rides along with
// whichever survivor it belongs to. Multiple entries may overlap (defensive); handle them all.
static void gmap_split_unmap(uint64_t ustart, uint64_t uend) {
    for (int i = 0; i < g_ngmap;) {
        uint64_t base = g_gmap[i].addr, end = base + g_gmap[i].len;
        if (ustart >= end || uend <= base) { // no overlap
            i++;
            continue;
        }
        int keep_head = base < ustart; // [base, ustart) survives below the unmapped range
        int keep_tail = uend < end;    // [uend, end)   survives above it
        if (!keep_head && !keep_tail) {
            g_gmap[i] = g_gmap[--g_ngmap]; // fully covered -> drop the whole entry (order-independent)
            continue;
        }
        if (keep_head)
            g_gmap[i].len = g_gmap[i].glen = ustart - base; // addr unchanged; trim to the surviving head
        else                                                // keep_tail only
            g_gmap[i].addr = uend, g_gmap[i].len = g_gmap[i].glen = end - uend;
        if (keep_head && keep_tail) gmap_add(uend, end - uend); // middle unmap -> tail becomes a 2nd entry
        i++;
    }
}

// Mirror of gmap_split_unmap for the DONTNEED private-anon registry: keep the surviving sub-region(s)
// (with their prot) tracked so madvise(MADV_DONTNEED) still gives Linux semantics on what remains,
// instead of forgetting the whole entry on a partial unmap. A non-anon range has no entry here and is
// left untouched. gmap_add/anon_track append past g_ngmap/g_nanonmap, and the appended tail starts at
// uend so it never re-overlaps [ustart,uend) -- the loop terminates.
static void anon_split_unmap(uint64_t ustart, uint64_t uend) {
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
        if (keep_head && keep_tail) anon_track(uend, end - uend, prot);
        i++;
    }
}

// emulate a MAP_FIXED mapping of [a0, a0+a1) -- anon-zero when `anon`, else the file bytes at
// fd@off -- that lands inside one of the guest's OWN existing (writable private-anon) reservations,
// WITHOUT clobbering a live 4 KB neighbour that shares a partial 16 KB host page. The guest uses 4 KB
// pages; Apple Silicon uses 16 KB, and macOS MAP_FIXED replaces WHOLE host pages -- so a fixed map of a
// sub-host-page range zeros/relays the neighbour occupying the rest of the edge host page (same class as
// MADV_DONTNEED). Fix (mirrors that split): MAP_FIXED-remap only the fully-covered INTERIOR host
// pages (fresh pages; load the file bytes there for a file map); write the partial head/tail edges IN
// PLACE over EXACTLY the requested bytes (memset 0 for anon, pread for file) so the neighbour survives.
// The caller gates this on the range being contained in a writable private-anon region, so the edge host
// pages are guaranteed mapped+writable. Returns 0 on success, -1 if the interior remap failed.
static int host_fixed_map286(uint64_t a0, uint64_t a1, int prot, int anon, int fd, off_t off) {
    size_t hp = (size_t)getpagesize();
    uint64_t lo = a0, hi = a0 + a1;
    uint64_t ilo = (lo + hp - 1) & ~((uint64_t)hp - 1); // first fully-covered host page
    uint64_t ihi = hi & ~((uint64_t)hp - 1);            // end of last fully-covered host page
    if (ilo < ihi) {
        if (mmap((void *)ilo, (size_t)(ihi - ilo), prot | PROT_READ | PROT_WRITE, MAP_FIXED | MAP_ANON | MAP_PRIVATE,
                 -1, 0) == MAP_FAILED)
            return -1;
        if (!anon && fd >= 0) pread(fd, (void *)ilo, (size_t)(ihi - ilo), off + (off_t)(ilo - lo));
    }
    uint64_t he = ilo < hi ? ilo : hi; // partial head edge [lo, he)
    if (lo < he) {
        if (anon || fd < 0)
            memset((void *)lo, 0, (size_t)(he - lo));
        else
            pread(fd, (void *)lo, (size_t)(he - lo), off);
    }
    uint64_t tl = he > ihi ? he : ihi; // partial tail edge [tl, hi) (never re-covers the head)
    if (tl < hi) {
        if (anon || fd < 0)
            memset((void *)tl, 0, (size_t)(hi - tl));
        else
            pread(fd, (void *)tl, (size_t)(hi - tl), off + (off_t)(tl - lo));
    }
    return 0;
}

// The guest's page size (as it sees via AT_PAGESZ / sysconf(_SC_PAGESIZE)): 4 KB for x86_64 guests,
// 16 KB for aarch64. Read straight from the auxv the loader built (type 6 = AT_PAGESZ), so this shared
// TU needs no per-arch macro. Falls back to the host granularity if the auxv isn't populated yet.
static size_t guest_pagesz(void) {
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == 6 && v) return (size_t)v;
    }
    return (size_t)getpagesize();
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
        // is EINVAL. Aligning against guest_pagesz() (4 KB x86 / 16 KB arm) -- not the 16 KB host page --
        // is what lets a legitimate 4 KB-granular x86 unmap through while still rejecting a truly
        // mis-aligned start (LTP munmap03: len 0, addr+1, and an out-of-range rlim_max address).
        {
            size_t gpg = guest_pagesz();
            if (a1 == 0 || (a0 & (gpg - 1)) || a0 + a1 < a0) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // Drop any guest PROT_NONE coverage for the unmapped range (the EFAULT registry, thread.c): the
        // addresses no longer name an inaccessible mapping. Uses the guest logical [a0,a1) even when the
        // physical release below is partial -- the guest's mapping is logically gone either way.
        gna_clear(a0 & ~(uint64_t)0xfff, (a0 + a1 + 0xfff) & ~(uint64_t)0xfff);
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
        uint64_t full = gmap_find_len(a0);
        // Page-size mismatch: guest pages are 4 KB (AT_PAGESZ) but the macOS host uses 16 KB pages, and
        // macOS munmap rounds the LENGTH up to a whole host page. macOS also gives every distinct mmap
        // its own host-page-aligned base + host-page-rounded extent, so two SEPARATE guest mappings never
        // share a 16 KB host page -- a host page is only ever shared by 4 KB sub-regions of ONE mapping.
        //   * COMPLETE unmap of a whole tracked mapping (a0 is its base and a1 is its logical length, with
        //     or without the 64 KB guard tail): the whole host-page-rounded extent is ours, no neighbour
        //     sits in the edge pages, so releasing it -- rounding the length UP, which also frees the guard
        //     tail (else ~64 KB leaks per map/unmap cycle) -- is safe.
        //   * Any OTHER unmap may be a 4 KB-granular SUB-RANGE of a larger mapping (V8's page allocator
        //     freeing an interior chunk, ZendMM trimming an aligned over-allocation), whose partial edge
        //     host pages still back a LIVE 4 KB neighbour the guest keeps. (same 16 KB-vs-4 KB
        //     class as): a plain munmap there rounds a partial edge page OUT to the full 16 KB and
        //     unmaps the neighbour -- and an unaligned start is outright EINVAL'd by host munmap (V8 then
        //     aborts on CHECK(0 == munmap)). So release only the whole HOST pages lying ENTIRELY inside
        //     [a0, a0+len); the partial edge pages stay mapped. The guest's logical unmap still succeeds
        //     (return 0) -- matching Linux, which never faults an unmap of a partial/already-unmapped range.
        size_t hp = (size_t)getpagesize();
        int complete = (a0 & (hp - 1)) == 0 && (full == (uint64_t)a1 || full == (uint64_t)a1 + 0x10000);
        int r;
        uint64_t u_lo, u_hi; // the range host munmap actually cleared (empty when u_lo==u_hi)
        if (complete) {
            len = (size_t)full; // include the guard tail; whole extent is ours -> round-up is safe
            r = munmap((void *)a0, len);
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
            // reclaimed at execve() teardown and still findable by gmap_find_len (the mremap grow path).
            // (PROT_NONE coverage was already dropped above via gna_clear over the guest-logical range.)
            gmap_split_unmap(u_lo, u_hi);
            anon_split_unmap(u_lo, u_hi);
            futex_shared_unmap(u_lo, u_hi); // drop/trim shared-futex-key coverage for the released range
            wipefork_del(u_lo, u_hi - u_lo); // a wipe-on-fork range that was unmapped no longer applies
            mlk_del(u_lo, u_hi - u_lo);      // an unmapped range is implicitly unlocked (mlock -> VmLck)
            // The host pages [u_lo,u_hi) are now genuinely released, so a guest access there must fault
            // (SIGSEGV). Without this the JIT's lazy zero-page grower (jit86_lazyguard) would re-serve the
            // fault -- growth budget for an adjacent live mapping, small budget otherwise -- and the guest
            // would silently read fresh zero memory instead of faulting. Mark the released range inaccessible
            // so the fault handler delivers the guest SIGSEGV; a later mmap over it clears the coverage.
            // (16 KB host-page granularity residual: a 4 KB sub-page whose 16 KB host page still backs a LIVE
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
        // stale-translation: the guest may re-map DIFFERENT code at this now-free VA -> drop any cached block
        // translations for the unmapped range so the dispatcher re-translates the new bytes (JITs/trampolines).
        if (r == 0) G_SMC_UNMAP(a0, a0 + (uint64_t)a1);
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
        if (a1 && !gmap_contains(a0, (uint64_t)a1) && !host_range_mapped((uintptr_t)a0, (size_t)a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        const uint64_t guard = 0x10000;
        uint64_t tracked = gmap_find_len(a0);             // full mapped extent at a0 (incl. guard), 0 if untracked
        uint64_t phys = tracked ? tracked : (uint64_t)a1; // bytes we can assume are mapped at a0
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
                size_t hp = (size_t)getpagesize();
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
                        gmap_del(a0);
                        anon_untrack(a0, (size_t)phys);
                        gna_clear(a0 & ~(uint64_t)0xfff, (ohi + 0xfff) & ~(uint64_t)0xfff);
                        mlk_del(a0, (uint64_t)a1);
                        wipefork_del(a0, (uint64_t)a1);
                    }
                    gmap_add(a4, (uint64_t)a2 + guard);
                    gmap_set_glen(a4, (uint64_t)a2);
                    anon_track(a4, (uint64_t)a2 + guard, PROT_READ | PROT_WRITE);
                    gna_clear(a4 & ~(uint64_t)0xfff, (nhi + 0xfff) & ~(uint64_t)0xfff);
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
        // Shrink, or a grow that still fits within the already-mapped extent: stay in place, touch nothing.
        if ((uint64_t)a2 <= phys) {
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
                gmap_del(a0);
                gmap_add(a0, want);              // track the grown extent (incl. fresh guard) for execve() teardown
                gmap_set_glen(a0, (uint64_t)a2); // /proc maps report the guest length (sans guard)
                anon_track(a0, want, PROT_READ | PROT_WRITE);
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
            gmap_del(a0);
            anon_untrack(a0, (size_t)phys);
        }
        gmap_add((uint64_t)r, (uint64_t)a2 + guard);                           // track for execve() teardown
        gmap_set_glen((uint64_t)r, (uint64_t)a2);                              // /proc maps: guest length (sans guard)
        anon_track((uint64_t)r, (uint64_t)a2 + guard, PROT_READ | PROT_WRITE); // fresh private-anon copy
        G_RET(c) = (uint64_t)r;
        break;
    }
    // mmap
    case 222: {
#ifdef __APPLE__
        // GPU rung 2 (opt-in): mmap of a synth DRM render-node fd at a MAP_DUMB offset. Mesa's kms_swrast
        // maps the dumb buffer for CPU software rendering; the buffer is an IOSurface that already lives at
        // a host VA (== guest VA in this in-process JIT), so decode the handle from the fake offset MAP_DUMB
        // handed back, look up the surface, and return its base VA directly — no real mmap. Gated on the
        // render-node tag, so no other mmap is affected (inert unless DD_GPU_IOSURFACE).
        if (gpu_iosurface_on() && !(a3 & 0x20) && (int)a4 >= 0 && (int)a4 < 1024 && g_devdri[(int)a4]) {
            uint32_t handle = (uint32_t)((uint64_t)a5 >> 12);
            int gi = dd_gpu_reg_by_handle(handle);
            if (gi >= 0) {
                G_RET(c) = (uint64_t)(uintptr_t)g_gpu_reg[gi].base;
                break;
            }
        }
#endif
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
        size_t guard = (!(a3 & 0x10) && (a3 & 0x20)) ? 0x10000 : 0;
        // mprotect (case 226) is a no-op (the JIT never executes guest pages), so a later PROT_READ ->
        // PROT_READ|WRITE upgrade would be silently dropped. Map ANON memory writable up front so the
        // upgrade is already in effect (redis' checkLinuxMadvFreeForkBug mmaps R then mprotects RW then stores).
        int prot = (a3 & 0x20) ? ((int)a2 | PROT_READ | PROT_WRITE) : (int)a2;
        // W6A item 3: guest RWX / PROT_EXEC mmaps (JVM/V8/LuaJIT/.NET/PyPy JIT arenas). On macOS a
        // non-MAP_JIT mmap that requests PROT_EXEC fails with EPERM under the hardened W^X policy, so
        // these guests can't allocate their code arena. But this is a DBT: the host NEVER executes guest
        // pages natively -- guest "execution" is translate_block() reading the page's bytes and emitting
        // host code into the (separately RX) code cache. So PROT_EXEC on a guest mapping is meaningless to
        // the host and only triggers the EPERM. Strip it: the page is mapped R+W, the guest writes its
        // generated code, "executes" it (guest PC enters the page -> map_host miss -> translate), and runs.
        // Setting g_rwx_guest also arms the (otherwise inert) SMC write-fault invalidation in frontend/x86_64
        // so a guest that OVERWRITES already-translated code re-translates. NORWXFIX=1 disables the strip.
        if (!getenv("NORWXFIX") && (prot & PROT_EXEC)) {
            if (a3 & 0x20) {
                // Anon JIT arena: strip EXEC and map R+W so the guest can write its generated code.
                prot = (prot & ~PROT_EXEC) | PROT_READ | PROT_WRITE;
                g_rwx_guest = 1; // a JIT guest is present (informational + SMC gate)
            } else if (prot & PROT_WRITE) {
                // File-backed WRITE+EXEC map: macOS W^X rejects it (EACCES) without MAP_JIT, but the JIT
                // never executes guest pages, so EXEC is meaningless -- drop it, keeping the file map R+W.
                // A file-backed READ+EXEC map (no write) is permitted by macOS -- that is how ld.so loads a
                // .so's text -- so it is left untouched. (LTP mincore02 maps a file PROT_READ|WRITE|EXEC.)
                prot &= ~PROT_EXEC;
                g_rwx_guest = 1;
            }
        }
        size_t hp = (size_t)getpagesize();
        void *r;
        int off_emul = 0;
        uint64_t pc_hint = 0;
        (void)pc_hint;
        // checkpoint/restore: hint a kernel-placed (a0==0), non-fixed guest map into the deterministic high
        // arena so a later restore's MAP_FIXED lands on a free VA. Inert unless armed (returns 0). A plain
        // hint: if the (reliably free) high slot were busy, the kernel just places it elsewhere.
        if (a0 == 0 && !(a3 & 0x10)) {
            uint64_t ch = ckpt_place_hint((uint64_t)a1 + guard);
            if (ch) a0 = ch;
        }
#ifdef PCACHE_MMAP_HINT
        // (pcache): give the dynamic linker's file-backed, non-fixed, kernel-placed maps (library
        // loads) a DETERMINISTIC base hint so their translated blocks are reusable across runs of the same
        // binary. A plain hint, never MAP_FIXED: if the range is busy the kernel places it elsewhere and
        // the map simply isn't cacheable this run (pcache_note_libmap below only records hint-honored
        // maps, and a warm run only ACTIVATES restored blocks when the same file identity lands on the
        // same base). No-op unless DDJIT_PCACHE is on.
        if (a0 == 0 && !(a3 & 0x10) && !(a3 & 0x20) && (int)a4 >= 0) {
            pc_hint = pcache_mmap_hint((uint64_t)a1);
            a0 = pc_hint;
        }
#endif
        // a MAP_FIXED map that REPLACES a 4 KB-granular sub-range of one of the guest's own
        // reservations (V8/Go committing fresh pages, or ld.so laying a segment inside its reserved span)
        // has a partial 16 KB host-page edge shared with a LIVE 4 KB neighbour. A direct host MAP_FIXED
        // there replaces WHOLE host pages -> the neighbour is zeroed/relaid (the heap-corruption
        // class; the likely victoria-metrics SIGBUS). When the range is fully contained in a tracked
        // WRITABLE private-anon region (so its edge host pages are mapped+writable), emulate the fixed map
        // edge-safely instead: remap only the interior host pages, fill the partial edges in place. Gated
        // on containment, so every fresh/whole-page/free-space fixed map keeps the direct path below and is
        // byte-identical (a non-contained map has no neighbour to protect).
        int fixed286 = 0;
        if ((a3 & 0x10) && a1 && ((a0 & (hp - 1)) || ((a0 + a1) & (hp - 1)))) {
            int aprot = anon_prot_if_contained(a0, (size_t)a1);
            if (aprot >= 0 && (aprot & PROT_WRITE)) {
                r = host_fixed_map286(a0, a1, prot, (a3 & 0x20) ? 1 : 0, (a3 & 0x20) ? -1 : (int)a4, (off_t)a5) == 0
                        ? (void *)a0
                        : MAP_FAILED;
                fixed286 = 1;
            }
        }
        if (!fixed286)
            r = mmap((void *)a0, (size_t)a1 + guard, prot, mmap_flags((int)a3), (a3 & 0x20) ? -1 : (int)a4, (off_t)a5);
        // Host-page-unaligned file offset. macOS uses 16 KB pages and its mmap requires the FILE OFFSET to
        // be a multiple of the host page size, but a Linux guest (4 KB pages) may legitimately map a file at
        // any 4 KB-granular offset. A non-MAP_FIXED file map whose offset is 4 KB- but not 16 KB-aligned is
        // therefore rejected with EINVAL (gcc maps its spec files at off=0x1000 and dies with an "internal
        // compiler error"). ld.so never trips this -- it maps its extra segments MAP_FIXED, which the
        // reconciliation block below already emulates -- which is why it stayed hidden. Emulate it the same
        // way: a kernel-chosen (a0 honored as a placement hint) private-anon region preloaded from the file
        // at the requested offset. RW so a later PROT upgrade is already in effect (mprotect is a no-op); a
        // short pread past EOF leaves the tail as anon zero, exactly as Linux presents it. Gated on the
        // direct mmap having FAILED for such an offset, so every aligned/fixed map keeps the native path and
        // is byte-identical. (A writable MAP_SHARED map at such an offset loses write-back -- the same
        // limitation the MAP_FIXED emulation has -- but read-only/private maps, the common case, are exact.)
        if (r == MAP_FAILED && !(a3 & 0x10) && !(a3 & 0x20) && (int)a4 >= 0 && ((off_t)a5 & (off_t)(hp - 1))) {
            void *ar = mmap((void *)a0, (size_t)a1, PROT_READ | PROT_WRITE, MAP_ANON | MAP_PRIVATE, -1, 0);
            if (ar != MAP_FAILED) {
                pread((int)a4, ar, (size_t)a1, (off_t)a5); // short read => trailing bytes stay anon-zero
                r = ar;
                off_emul = 1;
            }
        }
        // Past-EOF tail zero-fill. A file mmap whose length runs past the file's end leaves the trailing
        // WHOLE pages with no backing: macOS SIGBUSes on any read of them. ld.so does exactly this -- it maps
        // a .so's WHOLE vaddr span from the FIRST segment, so the inter-segment bytes become such past-EOF
        // pages. On Linux they are equally unbacked, but ld.so PROT_NONEs / replaces that region and never
        // reads it; with macOS's 16 KB pages, though, a later 4 KB-granular segment map (x86_64 .so p_align
        // 0x1000) shares its low host page with one of those past-EOF pages, so a stray access SIGBUSes where
        // Linux stayed quiet (julia's libdl/libjulia abort here). Re-map the genuinely-past-EOF whole-page
        // tail as anonymous zero -- the inaccessible-but-quiet region Linux effectively presents -- so such a
        // shared host page reads back zero instead of faulting. The partial page straddling EOF keeps macOS's
        // file bytes + zero-fill, a later MAP_FIXED segment map overwrites whatever it needs, and a fully
        // file-backed mapping (valid_end >= a1) is left byte-identical. RW only (an ANON PROT_EXEC map hits
        // macOS W^X EPERM; the JIT never executes guest pages anyway). MAP_PRIVATE only: a MAP_SHARED file
        // map past EOF can be made valid later by ftruncate-extending the file (sqlite/lmdb), so its tail
        // must stay the real shared mapping; ld.so's .so segments are all MAP_PRIVATE, so julia is covered.
        if (r != MAP_FAILED && !off_emul && !fixed286 && (a3 & 0x02) && !(a3 & 0x20) && (int)a4 >= 0 && a1) {
            struct stat st;
            if (fstat((int)a4, &st) == 0) {
                uint64_t avail = (uint64_t)st.st_size > a5 ? (uint64_t)st.st_size - (uint64_t)a5 : 0;
                uint64_t valid_end = (avail + hp - 1) & ~(uint64_t)(hp - 1); // first host page wholly past EOF
                if (valid_end < a1)
                    mmap((char *)r + valid_end, (size_t)(a1 - valid_end), PROT_READ | PROT_WRITE,
                         MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0);
            }
        }
        // 16 KB-vs-4 KB MAP_FIXED reconciliation. macOS arm64 mmap REQUIRES a 16 KB-aligned address for
        // MAP_FIXED, but x86_64 .so PT_LOAD segments are only p_align=0x1000 (4 KB), so ld.so's MAP_FIXED
        // mapping of e.g. libc's text segment at a 4 KB- (not 16 KB-) aligned guest address returns EINVAL
        // -> "failed to map segment from shared object" (file-backed) / "cannot map zero-fill pages" (the
        // anon BSS tail). (aarch64 .so segments use p_align 0x10000, a multiple of 16 KB, so they never hit
        // this.) ld.so has ALREADY reserved this whole .so address range with an earlier (kernel-placed,
        // 16 KB-aligned) mmap, so the range is ours -- emulate the failing fixed map with a private ANON
        // map at the 16 KB-rounded base, then pread the file bytes (file-backed) or leave it zero (anon
        // BSS): a private, writable copy, exactly what MAP_PRIVATE promises. Gated on the DIRECT mmap having
        // FAILED for a MAP_FIXED request, so every working case (non-fixed maps, and 16 KB-aligned aarch64
        // file/anon maps) takes the unchanged direct path above and is byte-identical.
        if (r == MAP_FAILED && (a3 & 0x10)) {
            uint64_t lo = a0 & ~(uint64_t)0x3fff; // round the start DOWN to a 16 KB host page
            size_t head = (size_t)(a0 - lo);      // bytes in the low page that belong to the PREVIOUS segment
            // The low page may also hold the tail of the previous PT_LOAD (a0 sits mid-16 KB-page). The ANON
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
                if (hsave) memcpy((void *)lo, hsave, head);            // restore the previous seg's tail
                if (!(a3 & 0x20) && (int)a4 >= 0)                      // file-backed: load the file bytes;
                    pread((int)a4, (void *)a0, (size_t)a1, (off_t)a5); //   short read => trailing BSS zeros
                r = (void *)a0; // success: the mapping now lives at the requested fixed guest address
            }
            free(hsave);
        }
        // V8 pointer-compression cage placement. macOS treats a non-MAP_FIXED address as a weak hint: it
        // lands AT the hint when that range is free (so node's randomly-based cage reservations work), but
        // when the hint overlaps an existing mapping macOS RELOCATES the new map far away (e.g. to
        // 0x70xx_xxxx). Linux instead honors the hint when a guest COMMITS fresh pages over its own
        // reservation (V8's BoundedPageAllocator carving heap pages out of the pointer-compression cage);
        // a guest that derives cage-relative (compressed) pointers from the hint faults when the page lands
        // outside the cage. So when macOS diverged from a high hint whose whole requested range is still
        // inside one of the guest's OWN tracked reservations, re-place the mapping AT the hint with
        // MAP_FIXED -- committing the fresh anon pages exactly where the guest expects. Gated on a DIVERGENT
        // result and on guest-owned coverage, so every already-correct placement (incl. all of node's, which
        // macOS honors) and anything touching engine-internal memory is left byte-identical and untouched.
        if (r != MAP_FAILED && a0 && (uint64_t)(uintptr_t)r != a0 && !(a3 & 0x10) && (a3 & 0x20) &&
            a0 >= 0x100000000ull && gmap_contains(a0, (uint64_t)a1 + guard)) {
            void *fr = mmap((void *)a0, (size_t)a1 + guard, prot, mmap_flags((int)a3) | MAP_FIXED, -1, 0);
            if (fr != MAP_FAILED) {
                munmap(r, (size_t)a1 + guard); // drop the relocated placement macOS chose
                r = fr;                        // mapping now lives at the requested cage-relative hint
            }
        }
        // refund
        if (r == MAP_FAILED && charge) {
            atomic_fetch_sub(&g_mem_charged, (uint64_t)a1);
            acct_publish_mem(); // publish the refunded charge into this process's cross-process slot
        }
        if (r != MAP_FAILED) {
            gmap_add((uint64_t)r, (uint64_t)a1 + guard); // track for execve() teardown
            gmap_set_glen((uint64_t)r, (uint64_t)a1);    // /proc maps report the guest length (sans guard)
            // Shared-futex key (thread.c): a file-backed MAP_SHARED region (memfd/shm, mapped independently
            // by each peer at its own VA) must key its futex words by the shared object identity, not the VA,
            // so a cross-process/cross-mapping FUTEX_WAKE reaches a FUTEX_WAIT (Wall 7). Record its VA range
            // -> (dev,ino,offset). MAP_SHARED=0x01 (incl. MAP_SHARED_VALIDATE=0x03); anon (a3&0x20) has no
            // fd/inode and, when shared, is only ever fork-inherited at a COMMON VA (the VA key already works
            // there), so it is excluded. off_emul (a private-anon offset-fixup copy) is not the real shared
            // object -- skip it. Inert for every private mapping (the fast-path gate stays 0).
            if ((a3 & 0x01) && !(a3 & 0x20) && !off_emul && (int)a4 >= 0)
                futex_shared_register((uint64_t)r, (uint64_t)a1, (int)a4, (uint64_t)a5);
            // mlockall(MCL_FUTURE): a mapping created while future-locking is armed must be wired resident on
            // creation (Linux mm/mlock.c). Best-effort (a RLIMIT_MEMLOCK refusal leaves it pageable); the
            // mlk_add records it so /proc Locked:/VmLck: reports the range even under g_mlock_all reporting.
            // MCL_FUTURE accounting: only wire+count the new mapping while it stays within RLIMIT_MEMLOCK.
            // A mapping that would push the locked total over the guest's limit is left pageable/uncounted
            // (the mmap still succeeds) so the tracked locked bytes never exceed the limit.
            if (g_mlock_future && mlk_rlimit_gate((uint64_t)r, (uint64_t)a1) == 0) {
                mlock(r, (size_t)a1 + guard);
                mlk_add((uint64_t)r, (uint64_t)a1);
            }
            // DONTNEED anon registry: record PRIVATE-ANON ranges (incl. the guard tail); for any other
            // (file-backed/shared) mapping, forget overlapping anon coverage -- a MAP_FIXED file map may
            // now sit where anon used to, and we must never anon-remap over it.
            if ((a3 & 0x20) && (a3 & 0x02))
                anon_track((uint64_t)r, (uint64_t)a1 + guard, prot);
            else
                anon_untrack((uint64_t)r, (uint64_t)a1 + guard);
            // A fresh mapping resets any prior MADV_WIPEONFORK marking on this address range (advice does
            // not survive the region being remapped) -- drop stale wipe coverage so a reused address is
            // never wrongly zeroed in a child.
            wipefork_del((uint64_t)r, (uint64_t)a1 + guard);
            // PROT_NONE registry (g_gna, thread.c; read INSIDE host_range_mapped). dd force-maps this region
            // host-RW, so a guest PROT_NONE mmap is really RW -- record the guest's REQUESTED prot so a
            // syscall buffer landing in it still EFAULTs (LTP read02); an accessible map clears stale coverage.
            {
                uint64_t glo = (uint64_t)r, ghi = ((uint64_t)r + (uint64_t)a1 + 0xfff) & ~(uint64_t)0xfff;
                if ((int)a2 == PROT_NONE)
                    gna_add(glo, ghi);
                else
                    gna_clear(glo, ghi);
            }
#ifdef PCACHE_MMAP_HINT
            // (pcache): a hinted library map that landed ON its deterministic hint. Cold epoch:
            // record {base, len, file identity} in the save manifest. Warm epoch: the activation gate for
            // the restored blocks deferred over this range (identity must match, or they stay dead).
            if (pc_hint && (uint64_t)(uintptr_t)r == pc_hint)
                pcache_note_libmap((uint64_t)(uintptr_t)r, (uint64_t)a1, (int)a4);
#endif
        }
        // stale-translation: a MAP_FIXED mmap REPLACES whatever code lived at the destination VA, so drop any
        // translations cached for it (Linux MAP_FIXED implicitly unmaps the range first).
        if (r != MAP_FAILED && (a3 & 0x10 /*MAP_FIXED*/)) G_SMC_UNMAP((uint64_t)(uintptr_t)r, (uint64_t)(uintptr_t)r + a1);
        G_RET(c) = (r == MAP_FAILED) ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // mprotect
    case 226: // mprotect: NO-OP for physical page protection (JIT never executes guest pages; a real
        // mprotect is harmful on macOS -- would fault the guest's own RELRO writes). The g_gna PROT_NONE
        // registry tracks the guest's INTENT (reserve PROT_NONE -> commit RW) so buffer checks EFAULT.
        if (a1) {
            // Linux mm/mprotect.c rejects a start not aligned to the (guest) page size with EINVAL BEFORE
            // touching anything, so a bad-alignment probe must not read as success.
            if (a0 & (uint64_t)(guest_pagesz() - 1)) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            // Linux mm/mprotect.c then walks the VMAs and returns ENOMEM if the range has a hole (any page
            // not backed by a mapping) -- an mprotect of a page-aligned but unmapped range must NOT read as a
            // fake success. Reject a range that is neither a tracked guest mapping (gmap -- covers ELF image /
            // stack / brk / anon+file mmap, INCLUDING a guest PROT_NONE reservation, which the host_range_mapped
            // probe would call unmapped) nor physically mapped host-side. Same regression-free idiom the mremap
            // source validation (case 216) uses: a hot mprotect on the guest's own memory hits gmap_contains
            // (no probe cost, no false ENOMEM); only a genuinely unmapped range is rejected.
            // NON-PIE: the ET_EXEC image is force-mapped HIGH (addr+g_nonpie_bias, __PAGEZERO forbids the low
            // 4 GB) but the guest still names its image by the LOW link vaddr -- static glibc's RELRO
            // mprotect(_dl_protect_relro) passes that low address, which is mapped only at the rebased VA. So
            // a low-range miss must re-check at nonpie_p(a0) before ENOMEM (inert for PIE: nonpie_p == a0).
            if (!gmap_contains(a0, (uint64_t)a1) && !host_range_mapped((uintptr_t)a0, (size_t)a1)) {
                // (open-coded nonpie_p: dispatch.c defines it AFTER this module in the TU)
                uint64_t reb = (g_nonpie_lo && a0 >= g_nonpie_lo && a0 < g_nonpie_hi) ? a0 + g_nonpie_bias : a0;
                if (reb == a0 ||
                    (!gmap_contains(reb, (uint64_t)a1) && !host_range_mapped((uintptr_t)reb, (size_t)a1))) {
                    G_RET(c) = (uint64_t)(-ENOMEM);
                    break;
                }
            }
            uint64_t glo = a0 & ~(uint64_t)0xfff, ghi = (a0 + a1 + 0xfff) & ~(uint64_t)0xfff;
            if ((int)a2 == PROT_NONE)
                gna_add(glo, ghi);
            else
                gna_clear(glo, ghi);
            // #423 / H9: a guest that mprotect()s a page to add PROT_EXEC is a JIT toggling an
            // already-written page executable -- the mmap(RW) -> write code -> mprotect(RX) pattern that
            // .NET/Wasm/managed runtimes use (as opposed to the RWX mmap case 222 already covers). It MUST
            // arm SMC the same way case 222 does: setting g_rwx_guest makes smc_protect() (G_AFTER_TRANSLATE,
            // dispatch_hooks.h) write-protect each translated source page, so a later overwrite -- the
            // mprotect(RW) + rewrite + mprotect(RX) re-toggle -- traps in jit86_lazyguard -> smc_on_write()
            // drops the stale translation and the new bytes re-translate. Without this the FIRST RX
            // translation is cached forever -> silent miscompile. This mprotect stays a physical no-op
            // (the SMC machinery does its own host mprotect on the code page); only the gate is set.
            // g_rwx_guest latches -- once a JIT guest is present it stays armed across every re-toggle, so
            // SMC coverage is kept, not lost, on a subsequent mprotect(RW)->mprotect(RX). NORWXFIX=1
            // disables, mirroring case 222.
            if (((int)a2 & PROT_EXEC) && !getenv("NORWXFIX"))
                g_rwx_guest = 1;
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
        if (s3db_durability() == 2) {
            int lf = (int)a2, mf = 0;
            if (lf & 0x1) mf |= MS_ASYNC;                    // Linux MS_ASYNC=1
            if (lf & 0x2) mf |= MS_INVALIDATE;               // Linux MS_INVALIDATE=2
            if (lf & 0x4) mf |= MS_SYNC;                     // Linux MS_SYNC=4 -> macOS MS_SYNC(16)
            if (!(mf & (MS_ASYNC | MS_SYNC))) mf |= MS_SYNC; // default to a sync flush
            int r = msync((void *)a0, (size_t)a1, mf);
            G_RET(c) = (r < 0 && errno != EINVAL) ? (uint64_t)(-errno) : 0;
        } else {
            G_RET(c) = 0;
        }
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
        int rl = mlk_rlimit_gate(a0, (uint64_t)a1);
        if (rl < 0) {
            G_RET(c) = (uint64_t)(int64_t)rl;
            break;
        }
        if (a1 && mlock((void *)a0, (size_t)a1) != 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        mlk_add(a0, (uint64_t)a1);
        G_RET(c) = 0;
        break;
    }
    case 229: // munlock: unwire + drop the tracked range. A host munlock failure is returned as -errno
        // (rather than a false success) so the guest sees Linux's error; len 0 is a success no-op.
        if (a1 && munlock((void *)a0, (size_t)a1) != 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        mlk_del(a0, (uint64_t)a1);
        G_RET(c) = 0;
        break;
    // Container-init compat: in the single-process model these are no-ops that return success so
    // entrypoints (mount /proc, unshare, drop caps, set hostname) proceed; the path-jail is the
    // real boundary, and a faked namespace grants no actual privilege (program still runs as our uid).
    // mincore: report page residency. The host mincore(2) fills one status byte per HOST page; Linux
    // wants one byte per page with bit0 = resident. macOS sets MINCORE_INCORE(0x1) in bit0 already, so
    // masking each byte to bit0 yields the Linux convention. (Host pages are 16 KB vs the guest's 4 KB,
    // so sub-host-page granularity is coarser than a real 4 KB kernel, but residency of the covering
    // page is faithful.) Untouched trailing bytes (the guest zero-filled its vector) stay 0 = absent.
    case 232: {
        size_t hps = (size_t)getpagesize(); // host mmap granularity (16 KB on Apple Silicon)
        size_t gps = guest_pagesz();        // page size the GUEST believes in (4 KB x86 / 16 KB arm)
        size_t len = (size_t)a1;
        // Linux mincore requires a page-aligned start address -> EINVAL otherwise (align to the GUEST page
        // so a valid 4 KB-granular x86 start is not rejected on the 16 KB host).
        if (a0 & (gps - 1)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // Linux: `vec` must be a writable buffer of ceil(len/pagesize) bytes; a NULL or inaccessible vec is
        // EFAULT. Validate against GUEST protections up front (both paths), because dd force-maps guest
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
        // Fast path: guest page == host page (aarch64) -- the host vec is already one byte per guest page.
        if (gps == hps || gps == 0 || len == 0) {
            int r = mincore((void *)a0, len, (char *)a2);
            if (r == 0 && a2) {
                size_t npages = (len + hps - 1) / hps;
                unsigned char *vec = (unsigned char *)a2;
                for (size_t i = 0; i < npages; i++)
                    vec[i] &= 1u; // Linux: bit0 = resident
            }
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
        size_t per = hps / gps; // guest pages per host page (== 4)
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
            if (!host_range_mapped((uintptr_t)a2, gpages)) {
                if (hv != stackbuf) free(hv);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            unsigned char *vec = (unsigned char *)a2;
            for (size_t i = 0; i < gpages; i++) {
                size_t h = per ? i / per : i;
                vec[i] = (h < hpages) ? (unsigned char)(hv[h] & 1u) : 0;
            }
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
        // otherwise a bad feature probe reads dd's best-effort no-op as success. (Valid Linux advice:
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
                // neighbour that shares a host page. The guest uses 4 KB pages; Apple Silicon uses 16 KB.
                // A plain `mmap(a0, a1, MAP_FIXED|ANON)` rounds a partial head/tail host page OUT to the full
                // 16 KB, so a guest DONTNEED of a free 4/8 KB span silently unmaps+zeros a LIVE object in the
                // rest of that host page (Go's scavenger DONTNEEDs a free 8 KB span whose 16 KB host page also
                // holds a live tiny string span -> the "heap corruption"). Fix: MAP_FIXED-remap only the
                // host-page-aligned INTERIOR (safe physical release + zero); zero the partial edge host pages
                // with memset over EXACTLY the requested bytes, never remapping a page shared with a neighbour.
                size_t hp = (size_t)getpagesize();
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
        long pr = ptrace_pvm(c, 0, (pid_t)(int)a0, (const struct iovec *)a1, (unsigned long)a2,
                             (const struct iovec *)a3, (unsigned long)a4);
        if (pr != PT_PVM_LOCAL) {
            G_RET(c) = (uint64_t)pr;
            break;
        }
        G_RET(c) = (uint64_t)svc_vm_iov_copy((const struct iovec *)a1, (unsigned long)a2, (const struct iovec *)a3,
                                             (unsigned long)a4);
        break;
    }
    // process_vm_writev: the mirror -- copy FROM the local iovecs (a1/a2) INTO the remote iovecs (a3/a4).
    case 271: {
        long pr = ptrace_pvm(c, 1, (pid_t)(int)a0, (const struct iovec *)a1, (unsigned long)a2,
                             (const struct iovec *)a3, (unsigned long)a4);
        if (pr != PT_PVM_LOCAL) {
            G_RET(c) = (uint64_t)pr;
            break;
        }
        G_RET(c) = (uint64_t)svc_vm_iov_copy((const struct iovec *)a3, (unsigned long)a4, (const struct iovec *)a1,
                                             (unsigned long)a2);
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
