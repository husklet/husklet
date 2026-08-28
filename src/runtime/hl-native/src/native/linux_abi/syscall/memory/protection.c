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
        if (physical_a0 == a0 ||
            (!hl_gmap_contains(physical_a0, (uint64_t)a1) && !host_range_mapped((uintptr_t)physical_a0, (size_t)a1))) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
    }
    /* Publish before any execute-permission registry or logical mapping can become visible to a peer.
       This is monotonic and may conservatively latch if a later host operation fails. */
    if (((int)a2 & (PROT_WRITE | PROT_EXEC)) == (PROT_WRITE | PROT_EXEC))
        atomic_store_explicit(&g_exec_bytes_unstable, 1, memory_order_release);
    uint64_t glo = a0 & ~(uint64_t)0xfff, ghi = (a0 + a1 + 0xfff) & ~(uint64_t)0xfff;
    int logical_protect_prepared = 0;
    int logical_protect_locked = 0;
    int logical_protect_failed = 0;
    hl_logical_vma_plan *logical_protect_plan = NULL;
    uint64_t folded_alias_a0 = nonpie_unfold(physical_a0);
    uint64_t alias_glo = folded_alias_a0 & ~(uint64_t)0xfff;
    uint64_t alias_ghi = (folded_alias_a0 + a1 + 0xfff) & ~(uint64_t)0xfff;
    /* Restoring an accessible protection re-arms any past-EOF coverage parked by a PROT_NONE below,
       before anything can reach the range again -- and before the logical-VMA transition lock is
       taken, because gbus_prepare() takes that same non-recursive lock. Arming needs the same
       prepare/STW transaction a mapping that arms the ledger uses. Inert (one filtered scan under
       the ledger lock) unless this range was actually parked. */
    if ((int)a2 != PROT_NONE &&
        (gbus_parked_overlap(glo, ghi) || (alias_glo != glo && gbus_parked_overlap(alias_glo, alias_ghi)))) {
        gbus_prepare();
        gbus_unpark(glo, ghi);
        if (alias_glo != glo) gbus_unpark(alias_glo, alias_ghi);
        gbus_prepare_release();
    }
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
            if (hl_logical_vma_global_prepare_protect(logical_a0, a1, (uint32_t)a2, &logical_protect_plan) != 0) {
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
        if (physical_a0 != a0 && hl_logical_vma_global_prepare_direct(physical_a0, a1, (uint32_t)a2, physical_a0,
                                                                      &logical_protect_plan) != 0) {
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
    if (((int)a2 & PROT_WRITE) || !((int)a2 & PROT_EXEC)) G_SMC_UNMAP(physical_a0, physical_a0 + a1);
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
    if ((int)a2 == PROT_NONE) {
        gna_add(glo, ghi);
        if (alias_glo != glo) gna_add(alias_glo, alias_ghi);
        /* The guest can no longer reach these bytes, and Linux answers a touch on a PROT_NONE page
           with a permission fault, never with SIGBUS. Park the past-EOF ledger's coverage so the
           translated BUS guard is not armed for the life of the process: ld.so PROT_NONEs the
           inter-segment hole of every shared library it maps, and that hole is the past-EOF tail of
           the whole-span reservation ld.so mapped first, so without this EVERY dynamically linked
           guest armed the ledger during startup and never disarmed it. The park is reversible --
           restoring an accessible protection (above) restores the SIGBUS contract. */
        gbus_park(glo, ghi);
        if (alias_glo != glo) gbus_park(alias_glo, alias_ghi);
    } else {
        gna_clear(glo, ghi);
        if (alias_glo != glo) gna_clear(alias_glo, alias_ghi);
    }
    if ((int)a2 != PROT_NONE && !((int)a2 & PROT_WRITE)) {
        gro_add(glo, ghi);
        if (alias_glo != glo) gro_add(alias_glo, alias_ghi);
        filemap_refresh_emulated(physical_a0, physical_a0 + a1);
    } else {
        gro_clear(glo, ghi);
        if (alias_glo != glo) gro_clear(alias_glo, alias_ghi);
    }
    if ((int)a2 & PROT_EXEC) {
        gnx_clear(glo, ghi);
        if (alias_glo != glo) gnx_clear(alias_glo, alias_ghi);
    } else {
        gnx_add(glo, ghi);
        if (alias_glo != glo) gnx_add(alias_glo, alias_ghi);
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
        int r = mincore((void *)a0, len, (unsigned char *)vec);
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
    int r = mincore((void *)a0, len, (unsigned char *)hv);
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
