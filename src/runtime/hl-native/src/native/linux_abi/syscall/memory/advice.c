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
