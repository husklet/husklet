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
        int logical_transition_locked = 0;
        int logical_candidate =
            hp > (size_t)guest_pagesz() && (a3 & 0x10) && (a3 & 0x01) && !(a3 & 0x20) && (int)a4 >= 0 && a1;
        if (logical_candidate) {
            /* One lock order for every logical mutation:
               transition -> translation flush -> mapping STW -> ledger. */
            gbus_mapping_transition_lock();
            logical_transition_locked = 1;
            if (!jit_guest_soft_activate()) {
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
                if ((int)a2 & PROT_EXEC)
                    gnx_clear(glo, ghi);
                else
                    gnx_add(glo, ghi);
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
