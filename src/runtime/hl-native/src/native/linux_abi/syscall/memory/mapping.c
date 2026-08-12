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
        gnx_clear(a0 & ~(uint64_t)0xfff, (a0 + a1 + 0xfff) & ~(uint64_t)0xfff);
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
            gnx_add(u_lo, u_hi);
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
