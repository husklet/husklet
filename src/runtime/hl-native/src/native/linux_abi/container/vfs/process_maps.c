static void container_path_env(char *out, size_t n) {
    out[0] = 0;
    const char *ge = hl_process_guest_environment_get();
    if (!ge) return;
    for (const char *s = ge; *s;) {
        const char *e = s;
        while (*e && *e != '\n')
            e++;
        if (!strncmp(s, "PATH=", 5)) {
            size_t L = (size_t)(e - s) - 5;
            if (L >= n) L = n - 1;
            memcpy(out, s + 5, L);
            out[L] = 0;
            return;
        }
        s = *e ? e + 1 : e;
    }
}

// Resolve a bare program name (no '/') against the container PATH, like execvp -- docker passes `sh`,
// not `/bin/sh`. Returns a guest path ("/bin/sh") that exists in the rootfs, or `prog` unchanged.
// Searches the guest's ACTUAL PATH (image-config ENV + `-e PATH=`), split on ':' in order, so programs
// outside the FHS bin dirs (golang's /usr/local/go/bin, rust's /usr/local/cargo/bin) are found; falls
// back to the historical FHS defaults only when PATH is unset/empty (manual/direct mode, no daemon env).
static const char *find_in_path(const char *prog, char *gbuf, size_t n) {
    if (!prog || strchr(prog, '/')) return prog; // absolute/relative name: execvp bypasses PATH search
    char hb[4200];
    char pathenv[4200];
    container_path_env(pathenv, sizeof pathenv);
    if (pathenv[0]) {
        for (const char *s = pathenv;;) {
            const char *e = s;
            while (*e && *e != ':')
                e++;
            size_t dl = (size_t)(e - s);
            // An empty entry ("::", or a leading/trailing ':') means the cwd per POSIX; a relative dir is
            // likewise cwd-relative. Anchor both at the guest cwd so the result is a rootfs-absolute guest
            // path -- secure_resolve/xresolve_overlay then confine it inside the jail (an escaping dir lands
            // on .jail-escape-denied and simply fails to match), so this is safe.
            if (dl == 0) {
                if (path_join(gbuf, n, g_cwd, prog) != 0) continue;
            } else {
                char dir[4200];
                if (dl >= sizeof dir) dl = sizeof dir - 1;
                memcpy(dir, s, dl);
                dir[dl] = 0;
                if (dir[0] == '/') {
                    if (path_join(gbuf, n, dir, prog) != 0) continue;
                } else {
                    char rooted[8400];
                    if (path_join(rooted, sizeof rooted, g_cwd, dir) != 0 || path_join(gbuf, n, rooted, prog) != 0)
                        continue;
                }
            }
            // Search the FULL overlay (upper THEN lowers): a fresh container's upper is empty and the program
            // lives only in a read-only image lower, so a bare xresolve_exec would ENOENT every PATH dir.
            if (access(xresolve_overlay(gbuf, hb, sizeof hb), X_OK) == 0) return gbuf;
            if (!*e) break;
            s = e + 1;
        }
        return gbuf; // not found on PATH: let the loader report ENOENT against the last attempted path
    }
    // No container PATH forwarded: historical FHS defaults.
    static const char *const dirs[] = {"/usr/local/sbin", "/usr/local/bin", "/usr/sbin", "/usr/bin",
                                       "/sbin",           "/bin",           NULL};
    for (int i = 0; dirs[i]; i++) {
        snprintf(gbuf, n, "%s/%s", dirs[i], prog);
        if (access(xresolve_overlay(gbuf, hb, sizeof hb), X_OK) == 0) return gbuf;
    }
    snprintf(gbuf, n, "/bin/%s", prog); // not found anywhere: let the loader report the error against /bin
    return gbuf;
}

#include "resolve.c"

// ===================== /proc/[self|pid] process introspection =====================
// macOS has no /proc, so the per-process files Linux servers read are synthesized here. All of these
// answer for the GUEST's own process only -- "self", the host pid, the container pid, or init's "1".

// Back a synthesized text file with an anonymous temp fd (mkstemp + immediate unlink): the fd holds the
// content, has no name, and behaves like an ordinary read-only file. Returns the fd, or -1 on error.
static int proc_text_fd(const char *buf, int n) {
    char tn[] = "/tmp/.hl-procXXXXXX";
    int fd = mkstemp(tn);
    if (fd >= 0) {
        unlink(tn);
        if (write(fd, buf, (size_t)n) < 0) {}
        lseek(fd, 0, SEEK_SET);
        if (fd < HL_NFD) g_proc_text_ro[fd] = 1;
    }
    return fd;
}

static char g_proc_text_desc[HL_NFD][64];

static int proc_text_fd_tagged(const char *buf, int n, const char *desc) {
    int fd = proc_text_fd(buf, n);
    if (fd >= 0 && fd < HL_NFD && desc) { snprintf(g_proc_text_desc[fd], sizeof g_proc_text_desc[fd], "%s", desc); }
    return fd;
}

static int proc_text_host_path(const char *path) {
    if (!path || !path[0]) return 0;
    const char *base = strrchr(path, '/');
    base = base ? base + 1 : path;
    return !strncmp(base, ".hl-proc", 8);
}

// ---- guest comm + canonical-exe tracking (the /proc/self/exe surface) ----
// Linux sets a task's comm from the LAST component of the path PASSED to execve, BEFORE binfmt_script
// rewrites it -- so "./run.sh" keeps comm "run.sh" (not "sh"), and execve("/proc/self/exe") gets comm
// "exe" -- while /proc/<pid>/exe names the canonical FILE that was actually loaded. Track the two
// separately: set_guest_comm() records the exec-name at boot and on every execve; g_exe_path holds the
// canonical exe path (see exe_canon below).
static char g_comm_store[16];
static int g_comm_store_set;

static void set_guest_comm(const char *execpath) {
    const char *b = (execpath && execpath[0]) ? execpath : "init";
    const char *s = strrchr(b, '/');
    if (s) b = s + 1;
    snprintf(g_comm_store, sizeof g_comm_store, "%.15s", b[0] ? b : "init");
    g_comm_store_set = 1;
#if defined(__linux__)
    // Mirror onto the host task name so a peer reading /proc/<pid>/{stat,status,comm} sees this comm
    // (each guest process is its own host process; without this a peer read reports the engine binary).
    (void)prctl(PR_SET_NAME, (unsigned long)g_comm_store, 0, 0, 0);
#endif
}

// Set the task comm verbatim (not a basename): prctl(PR_SET_NAME) renames the running task, and Linux
// exposes that exact name through /proc/self/{comm,status:Name,stat:field2}. Keeps the procfs comm surface
// in sync with the prctl name so a rename after boot/exec is reflected everywhere.
// `leader` says whether the renamed task is the thread-group leader. Only the leader owns the PROCESS comm
// surface (/proc/<pid>/{comm,status,stat}); a worker renaming itself must not clobber it, or concurrent
// pthread_setname_np callers overwrite each other. Every task still renames its own HOST thread, which is
// what a peer's /proc/<pid>/task/<tid>/comm reads.
static void set_guest_comm_name(const char *name, int leader) {
    char resolved[16];
    snprintf(resolved, sizeof resolved, "%.15s", name ? name : "");
    if (leader) {
        memcpy(g_comm_store, resolved, sizeof resolved);
        g_comm_store_set = 1;
    }
#if defined(__linux__)
    (void)prctl(PR_SET_NAME, (unsigned long)resolved, 0, 0, 0); // keep the host task name in sync (see set_guest_comm)
#endif
}

// Normalize a guest path LEXICALLY: collapse "//" and "." components and fold ".." (clamped at "/").
// No fs access and no symlink resolution (exe_canon below adds that); always emits an absolute path.
static void path_norm_lex(const char *in, char *out, size_t n) {
    if (!n) return;
    size_t o = 0;
    const char *p = in;
    while (*p) {
        while (*p == '/')
            p++;
        if (!*p) break;
        const char *e = p;
        while (*e && *e != '/')
            e++;
        size_t cl = (size_t)(e - p);
        if (cl == 1 && p[0] == '.') {
            p = e;
            continue;
        }
        if (cl == 2 && p[0] == '.' && p[1] == '.') { // pop the previous component (stays at root)
            while (o > 0 && out[o - 1] != '/')
                o--;
            if (o > 0) o--;
            p = e;
            continue;
        }
        if (o + 1 + cl < n) {
            out[o++] = '/';
            memcpy(out + o, p, cl);
            o += cl;
        }
        p = e;
    }
    if (o == 0) out[o++] = '/';
    out[o < n ? o : n - 1] = 0;
}

// Canonical ABSOLUTE guest path of an executable -- what readlink("/proc/self/exe") must return. Joins
// a relative exec path to the guest cwd, folds "."/".."/"//", then resolves symlinks the way the
// kernel's d_path would: through the overlay to the backing host file, mapped back into the guest view
// (an exec of the /bin/sh -> busybox symlink reports /bin/busybox, exactly like Linux). glibc's
// static-pie startup ASSERTS on a non-absolute link value ("dl-origin.c: linkval[0]=='/'") and ld.so
// resolves $ORIGIN RUNPATHs through this path, so it must be absolute and canonical.
static void exe_canon(const char *guest, char *out, size_t n) {
    if (!guest || !guest[0]) {
        snprintf(out, n, "/");
        return;
    }
    char joined[8600];
    if (guest[0] != '/') {
        char cwd[4200];
        if (g_rootfs)
            snprintf(cwd, sizeof cwd, "%s", g_cwd[0] ? g_cwd : "/");
        else if (!getcwd(cwd, sizeof cwd))
            snprintf(cwd, sizeof cwd, "/");
        snprintf(joined, sizeof joined, "%s/%s", cwd, guest);
    } else
        snprintf(joined, sizeof joined, "%s", guest);
    char lex[4200];
    path_norm_lex(joined, lex, sizeof lex);
    // resolve symlinks to the backing file, then map back into the guest namespace
    char hb[4200];
    const char *hp = xresolve_overlay(lex, hb, sizeof hb); // confined resolution (upper, then lowers)
    if (!g_rootfs) {
        // bare mode: guest view == host view; host realpath IS the canonical answer
        char rp[4200];
        snprintf(out, n, "%s", realpath(hp, rp) ? rp : lex);
        return;
    }
    struct stat st;
    if (stat(hp, &st) != 0) { // unresolvable/dangling: keep the (absolute) lexical form
        snprintf(out, n, "%s", lex);
        return;
    }
    char gb[4200];
    int mapped = guest_from_host_raw(hp, gb, sizeof gb);
    // guest_from_host_raw answers "/" for a host path outside every layer (fail-safe); keep the lexical
    // guest path then rather than claiming the exe is "/".
    snprintf(out, n, "%s", (mapped <= 0 || (gb[0] == '/' && gb[1] == 0 && !(lex[0] == '/' && lex[1] == 0))) ? lex : gb);
}

// The guest task name (Linux comm, max 15 chars): the recorded exec-name (set_guest_comm), falling back
// to the basename of the running image (g_exe_path) for paths that never went through an exec hook.
static void proc_comm(char *out, size_t n) {
    if (g_comm_store_set) {
        snprintf(out, n, "%s", g_comm_store);
        return;
    }
    const char *p = (g_exe_path && g_exe_path[0]) ? g_exe_path : "init";
    const char *base = strrchr(p, '/');
    base = base ? base + 1 : p;
    if (!base[0]) base = "init";
    snprintf(out, n, "%.15s", base);
}

// If `rp` addresses THIS process -- "/proc/self/<leaf>" or "/proc/<our-pid>/<leaf>" (host pid, container
// pid, or init's "1") -- return the <leaf> tail; else NULL. Foreign pids are not introspectable.
static const char *proc_self_leaf(const char *rp) {
    if (!rp) return NULL; // a NULL (bad) guest path resolves to NULL here; let the caller's host syscall EFAULT
    if (!strncmp(rp, "/proc/self/", 11)) return rp + 11;
    if (strncmp(rp, "/proc/", 6)) return NULL;
    const char *q = rp + 6;
    int i = 0;
    while (q[i] >= '0' && q[i] <= '9' && i < 15)
        i++;
    if (i == 0 || q[i] != '/') return NULL;
    char num[16];
    memcpy(num, q, (size_t)i);
    num[i] = 0;
    int pid = atoi(num);
    if (pid != (int)getpid() && pid != container_pid()) return NULL;
    return q + i + 1;
}

// One /proc/.../maps line for [lo,hi), plus the per-region smaps fields when `smaps` is set. The smaps
// fields are what redis's COW self-test parses; rss/dirty are reported equal to the region size (a
// resident mapping) so any field a parser looks up is present and consistent. Returns the length.
//
// The resident dirty bytes are reported under Shared_Dirty (not Private_Dirty): redis'
// checkLinuxMadvFreeForkBug forks and, in the CHILD, reads /proc/self/smaps Shared_Dirty for its
// MADV_FREE'd + rewritten private-anon page -- a value of 0 there is exactly its "buggy arm64 kernel"
// signature ("data corruption during background save", then it exits). A just-forked dirty COW page IS
// Shared_Dirty on real Linux (parent+child map it until COW breaks), so reporting the dirty bytes there
// both matches Linux for that query and clears the false positive. Rss stays == Shared_Clean +
// Shared_Dirty + Private_Clean + Private_Dirty (the kernel's invariant), so a summing parser is consistent.
static int proc_map_region_p(char *b, size_t n, unsigned long lo, unsigned long hi, const char *perms,
                             unsigned long long pgoff, unsigned dev_major, unsigned dev_minor, unsigned long long ino,
                             const char *name, int smaps) {
    unsigned long kb = (hi - lo) / 1024;
    // "Locked:" reports the mlock/mlockall'd bytes of THIS region (LTP mlock05 mlock()s a whole mapping
    // and reads its Locked back == the mapping size).
    unsigned long lockkb = (unsigned long)(hl_gmap_lock_region_bytes(lo, hi) / 1024);
    // A PROT_NONE region (perms "---p", e.g. the stack guard gap) is NOT resident: its resident/dirty
    // smaps fields must read 0 like the kernel, even though its virtual Size is the full span.
    int resident = (perms[0] != '-' || perms[1] != '-' || perms[2] != '-');
    unsigned long rkb = resident ? kb : 0;
    // Addresses use the kernel's own %08lx field width (min 8, NOT zero-padded to 12) so pmap/gdb and a
    // strict structural diff see the exact byte layout real Linux emits for the same address. A named row
    // reproduces seq_pad(): the name starts at offset 73 whatever the field widths, with at least one
    // separating space (measured against this host's kernel, every row type).
    int m = snprintf(b, n, "%08lx-%08lx %s %08llx %02x:%02x %llu ", lo, hi, perms, pgoff, dev_major, dev_minor, ino);
    if (name[0]) {
        if (m < 72) m += snprintf(b + m, (size_t)n - (size_t)m, "%*s", 72 - m, "");
        m += snprintf(b + m, (size_t)n - (size_t)m, " %s", name);
    }
    m += snprintf(b + m, (size_t)n - (size_t)m, "\n");
    if (smaps) {
        // The kernel's full per-region field set, in its order and its layout (name padded to 16, value
        // right-aligned at column 24). The set was short of Pss_Dirty/KSM/LazyFree/{Shmem,File}PmdMapped/
        // {Shared,Private}_Hugetlb/SwapPss/THPeligible/ProtectionKey, and a profiler that requires a field
        // it cannot find treats the region as unparsable rather than as a zero.
        // A FILE-backed region's resident pages are clean page-cache and carry no anonymous bytes -- report
        // them under Private_Clean with Anonymous 0, as the kernel does. The Shared_Dirty attribution above
        // is specific to private-anon COW and must not be extended to the image, or a parser summing
        // Anonymous over the regions counts the executable as anonymous memory.
        int fileback = ino != 0;
        unsigned long pclean = fileback ? rkb : 0, sdirty = fileback ? 0 : rkb, anon = fileback ? 0 : rkb;
        m += snprintf(b + m, (size_t)n - (size_t)m,
                      "Size:%19lu kB\nKernelPageSize:%9d kB\nMMUPageSize:%12d kB\n"
                      "Rss:%20lu kB\nPss:%20lu kB\nPss_Dirty:%14lu kB\n"
                      "Shared_Clean:%11d kB\nShared_Dirty:%11lu kB\n"
                      "Private_Clean:%10lu kB\nPrivate_Dirty:%10lu kB\nReferenced:%13lu kB\n"
                      "Anonymous:%14lu kB\nKSM:%20d kB\nLazyFree:%15d kB\nAnonHugePages:%10d kB\n"
                      "ShmemPmdMapped:%9d kB\nFilePmdMapped:%10d kB\n"
                      "Shared_Hugetlb:%9d kB\nPrivate_Hugetlb:%8d kB\n"
                      "Swap:%19d kB\nSwapPss:%16d kB\nLocked:%17lu kB\nTHPeligible:%12d\nProtectionKey:%10d\n",
                      kb, 4, 4, rkb, rkb, sdirty, 0, sdirty, pclean, 0UL, rkb, anon, 0, 0, 0, 0, 0, 0, 0, 0, 0, lockkb,
                      0, 0);
        // VmFlags follows the region's real protection (rd/wr/ex), not a fixed string: a PROT_NONE guard
        // claiming "rd wr" contradicts its own perms column. mr/mw/me are the may- bits, ac accountable.
        m += snprintf(b + m, (size_t)n - (size_t)m, "VmFlags:%s%s%s mr mw me ac \n", perms[0] == 'r' ? " rd" : "",
                      perms[1] == 'w' ? " wr" : "", perms[2] == 'x' ? " ex" : "");
    }
    return m;
}

// PT_LOAD segments of the main executable, read from the auxv the loader planted (AT_PHDR/AT_PHENT/
// AT_PHNUM) so /proc/self/maps shows the text as r-xp, rodata r--p, data rw-p -- the real per-segment
// protection, not a single flat rw-p span. Cross-arch (the Elf64_Phdr layout is arch-independent).
//
// Row geometry follows the kernel's ELF loader exactly, because that is what the file's readers model:
// a PT_LOAD is FILE-backed over [pgdown(vaddr), pgup(vaddr+filesz)) at file offset pgdown(p_offset), and
// the .bss remainder up to pgup(vaddr+memsz) is a separate ANONYMOUS row (offset 0, dev 00:00, no path).
struct mseg {
    uint64_t lo, hi, off;
    int prot;
    int file; // 1 -> carries the exe path + its dev:inode; 0 -> the anonymous .bss tail
};

// Guest -> host for a main-image address. A non-PIE ET_EXEC is linked low but mapped high (see
// g_nonpie_bias): every guest-visible image address, AT_PHDR included, is the LOW link value, and the bytes
// live at +bias. Dereferencing the guest value raw is what made this synthesis bail out entirely.
static uint64_t maps_image_host(uint64_t guest) {
    return (g_nonpie_lo && guest >= g_nonpie_lo && guest < g_nonpie_hi) ? guest + g_nonpie_bias : guest;
}

// The main image's program headers at their HOST location, with `phnum`/`phent` and the load bias that maps
// a link-time vaddr to the guest-visible one (0 for a non-PIE, whose guest addresses stay at the link
// values). NULL when the auxv is absent or the headers are no longer mapped -- callers then degrade rather
// than fault the engine.
static const uint8_t *maps_phdr_table(uint64_t *phnum_out, uint64_t *phent_out, uint64_t *bias_out) {
    uint64_t phdr = 0, phent = 0, phnum = 0;
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == 3)
            phdr = v;
        else if (t == 4)
            phent = v;
        else if (t == 5)
            phnum = v;
    }
    if (!phdr || phent < 56 || phnum == 0 || phnum > 256) return NULL;
    /* Probe the HOST location of the headers: unprobed, a guest unmap would let any guest reading
     * /proc/self/maps SIGSEGV the engine. Bailing out only drops rows. */
    uint64_t hostphdr = maps_image_host(phdr);
    if (!hl_host_range_mapped((uintptr_t)hostphdr, (size_t)(phnum * phent))) return NULL;
    const uint8_t *ph = (const uint8_t *)(uintptr_t)hostphdr;
    // load bias: PT_PHDR's runtime address (AT_PHDR) minus its link vaddr; 0 for a non-PIE.
    uint64_t bias = 0;
    for (uint64_t i = 0; i < phnum; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type;
        memcpy(&type, e, 4);
        if (type == 6) {
            uint64_t pv;
            memcpy(&pv, e + 16, 8);
            bias = phdr - pv;
            break;
        } // PT_PHDR
    }
    *phnum_out = phnum;
    *phent_out = phent;
    *bias_out = bias;
    return ph;
}

static int maps_phdr_segs(struct mseg *seg, int maxn) {
    uint64_t phent = 0, phnum = 0, bias = 0;
    const uint8_t *ph = maps_phdr_table(&phnum, &phent, &bias);
    if (!ph) return 0;
    // PT_GNU_RELRO (0x6474e552): the prefix of the data segment the loader RE-PROTECTS read-only after
    // relocation. The kernel splits the writable load VMA there, so /proc/self/maps shows that prefix as
    // r--p then the rest rw-p. Toolchains that fold rodata into the r-xp text segment (aarch64 gcc default,
    // unlike x86 -z separate-code) otherwise expose NO r--p image row at all -- so replay the relro split.
    uint64_t relro_lo = 0, relro_hi = 0;
    for (uint64_t i = 0; i < phnum; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type;
        memcpy(&type, e, 4);
        if (type == 0x6474e552u) {
            uint64_t vaddr, memsz;
            memcpy(&vaddr, e + 16, 8);
            memcpy(&memsz, e + 40, 8);
            relro_lo = (bias + vaddr) & ~0xfffULL;
            relro_hi = (bias + vaddr + memsz + 0xfffULL) & ~0xfffULL;
            break;
        }
    }
    int nseg = 0;
#define MSEG_PUSH(LO, HI, PROT, OFF, FILE)                                                                             \
    do {                                                                                                               \
        if (nseg < maxn && (HI) > (LO)) {                                                                              \
            seg[nseg].lo = (LO);                                                                                       \
            seg[nseg].hi = (HI);                                                                                       \
            seg[nseg].prot = (PROT);                                                                                   \
            seg[nseg].off = (OFF);                                                                                     \
            seg[nseg].file = (FILE);                                                                                   \
            nseg++;                                                                                                    \
        }                                                                                                              \
    } while (0)
    for (uint64_t i = 0; i < phnum && nseg < maxn; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type, flags;
        uint64_t poff, vaddr, filesz, memsz;
        memcpy(&type, e, 4);
        memcpy(&flags, e + 4, 4);
        memcpy(&poff, e + 8, 8);
        memcpy(&vaddr, e + 16, 8);
        memcpy(&filesz, e + 32, 8);
        memcpy(&memsz, e + 40, 8);
        if (type != 1 || memsz == 0) continue; // PT_LOAD only
        uint64_t start = bias + vaddr;
        uint64_t lo = start & ~0xfffULL;
        uint64_t fhi = filesz ? ((start + filesz + 0xfffULL) & ~0xfffULL) : lo; // end of the file-backed part
        uint64_t hi = (start + memsz + 0xfffULL) & ~0xfffULL;
        uint64_t foff = poff - (start - lo); // the file offset the row's first page maps
        int prot = ((flags & 4) ? 4 : 0) | ((flags & 2) ? 2 : 0) | ((flags & 1) ? 1 : 0); // R|W|X
        // A writable segment whose start is covered by relro: emit the relro prefix as r--p, the rest rw-p.
        uint64_t rhi = relro_hi < fhi ? relro_hi : fhi;
        if ((prot & 2) && rhi > relro_lo && relro_lo >= lo && rhi > lo) {
            uint64_t rlo = relro_lo > lo ? relro_lo : lo;
            MSEG_PUSH(lo, rlo, prot, foff, 1);
            MSEG_PUSH(rlo, rhi, 4, foff + (rlo - lo), 1); // r--p (read-only after relocation)
            MSEG_PUSH(rhi, fhi, prot, foff + (rhi - lo), 1);
        } else {
            MSEG_PUSH(lo, fhi, prot, foff, 1);
        }
        MSEG_PUSH(fhi, hi, prot, 0, 0); // the .bss remainder: anonymous, like the kernel's set_brk()
    }
#undef MSEG_PUSH
    return nseg;
}

// mm->{start_code,end_code,start_data,end_data} as /proc/[pid]/stat fields 26/27/45/46, derived the way
// load_elf_binary derives them: the text bounds are the executable PT_LOAD's [vaddr, vaddr+filesz) and the
// data bounds the HIGHEST PT_LOAD's -- both un-rounded, unlike the maps rows. A backtrace/dladdr-alike asks
// "is this pc in the text?" here, so leaving them zero says the program has no code.
static void maps_code_data_bounds(uint64_t *sc, uint64_t *ec, uint64_t *sd, uint64_t *ed) {
    *sc = *ec = *sd = *ed = 0;
    uint64_t phent = 0, phnum = 0, bias = 0;
    const uint8_t *ph = maps_phdr_table(&phnum, &phent, &bias);
    if (!ph) return;
    for (uint64_t i = 0; i < phnum; i++) {
        const uint8_t *e = ph + i * phent;
        uint32_t type, flags;
        uint64_t vaddr, filesz;
        memcpy(&type, e, 4);
        memcpy(&flags, e + 4, 4);
        memcpy(&vaddr, e + 16, 8);
        memcpy(&filesz, e + 32, 8);
        if (type != 1) continue; // PT_LOAD only
        uint64_t lo = bias + vaddr, hi = lo + filesz;
        if ((flags & 1) && (!*sc || lo < *sc)) *sc = lo;
        if ((flags & 1) && hi > *ec) *ec = hi;
        if (lo > *sd) *sd = lo;
        if (hi > *ed) *ed = hi;
    }
}

static void maps_perms_str(int prot, char *out) { // prot bits: 4=R 2=W 1=X
    out[0] = (prot & 4) ? 'r' : '-';
    out[1] = (prot & 2) ? 'w' : '-';
    out[2] = (prot & 1) ? 'x' : '-';
    out[3] = 'p';
    out[4] = 0;
}

// The guest brk arena bounds, defined (as file-scope statics) in syscall/dispatch.c which is #included
// AFTER this TU; a matching tentative declaration here lets the maps synth name the [heap] region. Both
// are static definitions of the same object in one translation unit, so this reads the live break.
static uint64_t brk_lo, brk_cur, brk_hi;

// One /proc/maps row, collected before emit so the whole file can be address-sorted (the kernel ALWAYS
// emits VMAs in ascending start order; pmap/gdb and jemalloc/glibc's sequential parse rely on it).
struct maprow {
    uint64_t lo, hi, off, ino;
    unsigned dev_major, dev_minor;
    char perms[5];
    const char *name;
};

static int maprow_cmp(const void *a, const void *b) {
    const struct maprow *p = (const struct maprow *)a, *q = (const struct maprow *)b;
    if (p->lo != q->lo) return p->lo < q->lo ? -1 : 1;
    // Equal starts: the NARROWER row first. A MAP_FIXED sub-mapping shares its start with the reservation
    // it replaced, and the overlap trim below keeps whichever row comes first -- which must be the
    // sub-mapping, exactly as the kernel's VMA split leaves it.
    return p->hi < q->hi ? -1 : p->hi > q->hi ? 1 : 0;
}

static int proc_fd_rebase(char *tgt, size_t capacity); // defined below; maps naming reuses /proc/self/fd's
static int synth_names_dir_open(const char *guestpath, const char *const *names, int kind);

// The maps rows for one read, plus the arena the file-backed rows' pathnames live in (`name` points into
// it). One table serves maps, smaps, numa_maps, smaps_rollup and map_files, so the five files cannot
// disagree about the guest's address space.
struct maptable {
    struct maprow *row;
    char *names;
    int n;
};

#define MAPTABLE_NAME_MAX 512 // per file-backed mapping; longer guest paths are dropped, never truncated

// The guest-visible path a file-backed mapping was created from. thread.c's g_filemap keeps a retained
// dup of the backing descriptor alive for the mapping's lifetime, so this resolves even after the guest
// closed its own fd. Rebased out of the rootfs/volume table exactly as /proc/self/fd is: an unrebasable
// host path is REFUSED (0), because an unnamed anon row is a loss of detail while a host path is a
// containment failure. Returns 1 on success.
static int filemap_guest_path(int fd, char *out, size_t n) {
    char hp[4200];
    if (fd < 0 || hl_native_fd_path(fd, hp, sizeof hp) != 0 || hp[0] != '/') return 0;
    int mapped = proc_fd_rebase(hp, sizeof hp);
    // Jailed and unrebased means the path lies outside every layer: refuse it. In bare mode the guest
    // namespace IS the host's, so the path is already the guest's own (same rule /proc/self/exe follows).
    if (mapped < 0 || (g_rootfs && mapped == 0)) return 0;
    if (strlen(hp) >= n) return 0;
    snprintf(out, n, "%s", hp);
    return 1;
}

// The g_filemap entry whose span contains [lo,hi), or -1. mmap registers one entry per file-backed
// mapping and filemap_unmap splits them on munmap/MAP_FIXED, so a containing entry names exactly one file.
static int filemap_row_index(uint64_t lo, uint64_t hi) {
    for (int i = 0; i < g_nfilemap; i++)
        if (lo >= g_filemap[i].lo && hi <= g_filemap[i].hi) return i;
    return -1;
}

// The guest protection registries thread.c keeps and mem.c maintains from mmap/mprotect: g_gna is the
// PROT_NONE intervals, g_gro the read-only (no PROT_WRITE) ones. They are the only live record of a guest's
// CURRENT protection -- the image rows are derived from the program headers, so without consulting these a
// guest that mprotects its own text keeps seeing the link-time permissions, and a mapping is rarely
// uniformly protected anyway (a glibc pthread stack is one mmap whose first page is the guard).
//
// Returns the intervals of `reg` overlapping [lo,hi), clipped and sorted ascending (insertion sort: the
// registries hold at most GNA_MAX entries and are not kept in order).
static int maps_prot_spans(const void *reg, int count, uint64_t lo, uint64_t hi, uint64_t *out, int maxn) {
    const struct {
        uint64_t lo, hi;
    } *iv = reg;

    int n = 0;
    for (int i = 0; i < count && n < maxn; i++) {
        uint64_t a = iv[i].lo > lo ? iv[i].lo : lo, b = iv[i].hi < hi ? iv[i].hi : hi;
        if (b <= a) continue;
        int at = n;
        while (at > 0 && out[2 * at - 2] > a) {
            out[2 * at] = out[2 * at - 2];
            out[2 * at + 1] = out[2 * at - 1];
            at--;
        }
        out[2 * at] = a;
        out[2 * at + 1] = b;
        n++;
    }
    return n;
}

// Whether `lo` sits inside one of `reg`'s intervals within [lo,hi), and how far the answer holds.
static int maps_prot_at(const void *reg, int count, uint64_t lo, uint64_t hi, uint64_t *edge) {
    uint64_t iv[64];
    int n = maps_prot_spans(reg, count, lo, hi, iv, 32), in = 0;
    for (int i = 0; i < n; i++) {
        if (iv[2 * i] <= lo && lo < iv[2 * i + 1]) {
            in = 1;
            if (iv[2 * i + 1] < *edge) *edge = iv[2 * i + 1];
        } else if (iv[2 * i] > lo && iv[2 * i] < *edge)
            *edge = iv[2 * i];
    }
    return in;
}

// The perms a row's [lo,hi) currently carries: `natural` (phdr-derived for the image, the mapping's own for
// a registry row) with the live protection registries applied. *until reports how far the answer holds, so
// the caller can split the row where the protection changes inside it.
static void maps_live_perms(uint64_t lo, uint64_t hi, const char *natural, char *out, uint64_t *until) {
    // The registries are written in TWO coordinate systems: the ELF loader (x86.c, elf.c) registers a
    // non-PIE image's segments at the HOST addresses its bytes occupy (+g_nonpie_bias), while mprotect
    // registers the GUEST address the guest passed. So query both and take the union -- reading only the
    // host fold missed the guest's own RELRO mprotect, reading only the guest address missed the loader's
    // whole image. (The mixed keying is itself a defect; see the non-PIE bias family.)
    uint64_t bias = maps_image_host(lo) - lo;
    uint64_t edge = hi, hedge = hi + bias;
    int in_none = maps_prot_at(g_gna, g_ngna, lo, hi, &edge);
    int in_ro = maps_prot_at(g_gro, g_ngro, lo, hi, &edge);
    if (bias) {
        in_none |= maps_prot_at(g_gna, g_ngna, lo + bias, hi + bias, &hedge);
        in_ro |= maps_prot_at(g_gro, g_ngro, lo + bias, hi + bias, &hedge);
        if (hedge - bias < edge) edge = hedge - bias;
    }
    snprintf(out, 5, "%s", natural);
    if (in_none) {
        out[0] = out[1] = out[2] = '-';
    } else if (in_ro) {
        out[1] = '-'; // read-only: keep whatever R/X the row already claims, drop W
        if (out[0] == '-') out[0] = 'r';
    } else if (out[0] == 'r') {
        // Readable and NOT in the read-only registry. The ELF loader registers every non-writable PT_LOAD
        // there at load time, so a phdr-derived row that has left it can only have been mprotect'd writable
        // by the guest -- which is the case a W^X audit or a JIT's own RW/RX toggle asks about, and which a
        // purely phdr-derived row answers with the stale link-time permission forever.
        out[1] = 'w';
    }
    *until = edge;
}

static void maptable_free(struct maptable *t) {
    free(t->row);
    free(t->names);
    t->row = NULL;
    t->names = NULL;
    t->n = 0;
}

// Collect the guest's address space as maps rows: the main image's PT_LOAD segments, the stack + its
// guard, the brk arena as [heap], and one row per remaining guest-map registry entry -- file-backed ones
// named from g_filemap. Sorted ascending and trimmed to be non-overlapping, the two invariants every
// consumer of this file (pmap, gdb, libunwind, jemalloc, glibc) assumes. Returns 0 on allocation failure.
static int maptable_build(struct maptable *t) {
    memset(t, 0, sizeof *t);
    // Capacity: main-exe PT_LOAD segs + stack + guard + heap split + one row per gmap entry, plus two per
    // protection-registry interval (a row splits at each). Dropping a row would truncate the file.
    size_t mapping_count = hl_gmap_count();
    int cap = (int)mapping_count + 4 * GNA_MAX + 32;
    struct maprow *rows = (struct maprow *)calloc((size_t)cap, sizeof *rows);
    char *names = (char *)calloc((size_t)(g_nfilemap > 0 ? g_nfilemap : 1), MAPTABLE_NAME_MAX);
    if (!rows || !names) {
        free(rows);
        free(names);
        return 0;
    }
    int nrow = 0;
    // An anonymous row: file offset 0, dev 00:00, inode 0 -- the tuple every maps parser uses to tell an
    // anonymous VMA from a file-backed one.
#define MAPROW_ADD(LO, HI, PERMS, NAME) MAPROW_ADD_F(LO, HI, PERMS, 0, 0, 0, 0, NAME)
#define MAPROW_ADD_F(LO, HI, PERMS, OFF, DMAJ, DMIN, INO, NAME)                                                        \
    do {                                                                                                               \
        if (nrow < cap && (HI) > (LO)) {                                                                               \
            rows[nrow].lo = (LO);                                                                                      \
            rows[nrow].hi = (HI);                                                                                      \
            rows[nrow].off = (OFF);                                                                                    \
            rows[nrow].dev_major = (DMAJ);                                                                             \
            rows[nrow].dev_minor = (DMIN);                                                                             \
            rows[nrow].ino = (INO);                                                                                    \
            snprintf(rows[nrow].perms, sizeof rows[nrow].perms, "%s", (PERMS));                                        \
            rows[nrow].name = (NAME);                                                                                  \
            nrow++;                                                                                                    \
        }                                                                                                              \
    } while (0)
    // The main executable's PT_LOAD segments, with their real per-segment protection (text r-xp, rodata
    // r--p, data rw-p) and the exe path as the mapping name -- read from the auxv program headers.
    struct mseg seg[32];
    int nseg = maps_phdr_segs(seg, 32);
    const char *hostexe = (g_exe_path && g_exe_path[0]) ? g_exe_path : "";
    // The pathname column is the path the GUEST knows: strip the rootfs prefix exactly as /proc/self/exe
    // does, else the two files disagree and the host's rootfs location leaks into the container.
    const char *exe = hostexe;
    if (g_rootfs && !strncmp(exe, g_rootfs_canon, g_rootfs_canon_len)) exe += g_rootfs_canon_len;
    if (!exe[0]) exe = hostexe;
    // dev:inode of the image, stat'd through the HOST path. A file-backed row must carry a non-zero pair:
    // that -- not the pathname, which the kernel also prints for [heap]/[stack] -- is how libunwind/ASan/
    // dladdr-alikes decide a row names an object on disk. Unstattable -> the anonymous tuple, not a lie.
    unsigned exe_dmaj = 0, exe_dmin = 0;
    unsigned long long exe_ino = 0;
    {
        struct stat es;
        if (hostexe[0] && stat(hostexe, &es) == 0) {
            exe_dmaj = hl_linux_device_major((uint64_t)es.st_dev);
            exe_dmin = hl_linux_device_minor((uint64_t)es.st_dev);
            exe_ino = (unsigned long long)es.st_ino;
        }
    }
    for (int i = 0; i < nseg; i++) {
        char perms[5];
        maps_perms_str(seg[i].prot, perms);
        if (seg[i].file)
            MAPROW_ADD_F(seg[i].lo, seg[i].hi, perms, seg[i].off, exe_dmaj, exe_dmin, exe_ino, exe);
        else
            MAPROW_ADD(seg[i].lo, seg[i].hi, perms, ""); // the .bss tail is anonymous
    }
    if (g_stack_hi) {
        unsigned long lo = (unsigned long)g_stack_lo, hi = (unsigned long)g_stack_hi;
        MAPROW_ADD(lo > 0x1000 ? lo - 0x1000 : 0, lo, "---p", ""); // guard gap below the stack
        MAPROW_ADD(lo, hi, "rw-p", "[stack]");
    }
    // The heap: emit exactly [brk_lo, brk_cur) as [heap], like the kernel (whose heap VMA ends at the
    // break). hl reserves a large brk arena up front (one gmap entry [brk_lo,brk_hi)); the reserved tail
    // above brk_cur is NOT part of the guest-visible heap, so it is dropped -- otherwise maps would show a
    // 256 MB anon region no real container has. jemalloc/glibc-malloc/redis/pmap look for this [heap] line.
    int have_heap = brk_hi && brk_cur > brk_lo;
    if (have_heap) MAPROW_ADD((unsigned long)brk_lo, (unsigned long)((brk_cur + 0xfff) & ~0xfffULL), "rw-p", "[heap]");
    for (size_t i = 0; i < mapping_count; i++) {
        hl_gmap_entry mapping;
        if (!hl_gmap_get(i, &mapping)) continue;
        // report the guest-VISIBLE length (glen) so a mapping's Size/Rss matches the guest's mmap length,
        // not hl's full extent including the 64 KB guard tail it reserves past anon maps (LTP mlock05 Rss).
        // Page-round the end as the kernel does: a VMA spans PAGE_ALIGN(len), so a guest that mmap'd a
        // non-multiple length must still see a page-granular row -- parsers divide the span by the page size.
        unsigned long lo = (unsigned long)mapping.address;
        unsigned long hi = (lo + (unsigned long)mapping.guest_length + 0xffful) & ~0xffful;
        if (g_stack_hi && lo >= (unsigned long)g_stack_lo && hi <= (unsigned long)g_stack_hi)
            continue; // already emitted as [stack]
        if (brk_hi && lo == (unsigned long)brk_lo)
            continue; // the brk arena -- rendered as [heap] above (tail beyond brk is not guest-visible)
        // skip a region already rendered as PT_LOAD segments (the image span the loader tracks as one entry).
        // For a non-PIE the loader's entry sits at the HIGH host address while the rows are at the guest link
        // addresses, so fold the entry back through the bias before comparing.
        int covered = 0;
        if (nseg > 0) {
            unsigned long glo = lo;
            if (g_nonpie_bias && lo >= g_nonpie_lo + g_nonpie_bias && lo < g_nonpie_hi + g_nonpie_bias)
                glo = lo - g_nonpie_bias;
            for (int s = 0; s < nseg; s++)
                if (glo >= seg[s].lo && glo < seg[s].hi) {
                    covered = 1;
                    break;
                }
        }
        if (covered) continue;
        // A file-backed mapping is named from g_filemap, which records the backing dev/inode/offset and
        // keeps a dup of the descriptor open. Without this every shared library -- ld.so included -- showed
        // as an unnamed anon rw-p row, so dladdr-alikes, libunwind and any W^X audit saw no objects at all.
        int fm = filemap_row_index(lo, hi);
        if (fm >= 0) {
            char *nm = names + (size_t)fm * MAPTABLE_NAME_MAX;
            if (!nm[0] && !filemap_guest_path(g_filemap[fm].fd, nm, MAPTABLE_NAME_MAX)) nm[0] = 0;
            MAPROW_ADD_F(lo, hi, "rw-p", g_filemap[fm].offset + (lo - g_filemap[fm].lo),
                         hl_linux_device_major(g_filemap[fm].device), hl_linux_device_minor(g_filemap[fm].device),
                         g_filemap[fm].inode, nm);
        } else
            MAPROW_ADD(lo, hi, "rw-p", "");
    }
    // Apply the live protection registries to every row, splitting where the protection changes inside one.
    // Image rows come from the program headers, so this is what makes a guest's own mprotect visible; the
    // registry rows have no protection of their own at all and would otherwise every one claim rw-p.
    for (int i = 0, collected = nrow; i < collected; i++) {
        uint64_t at = rows[i].lo, end = rows[i].hi;
        char natural[5];
        snprintf(natural, sizeof natural, "%s", rows[i].perms);
        int first = 1;
        while (at < end) {
            char perms[5];
            uint64_t until = end;
            maps_live_perms(at, end, natural, perms, &until);
            if (first) {
                rows[i].hi = until;
                snprintf(rows[i].perms, sizeof rows[i].perms, "%s", perms);
                first = 0;
            } else {
                MAPROW_ADD_F(at, until, perms, rows[i].ino ? rows[i].off + (at - rows[i].lo) : 0, rows[i].dev_major,
                             rows[i].dev_minor, rows[i].ino, rows[i].name);
            }
            at = until;
        }
    }
#undef MAPROW_ADD_F
#undef MAPROW_ADD
    qsort(rows, (size_t)nrow, sizeof *rows, maprow_cmp);
    // Ascending AND non-overlapping is the invariant every sequential parser relies on, and the two
    // sources (phdr segments, guest-map registry) can still collide -- a whole-span loader reservation
    // that a MAP_FIXED replaced in part, most of all. Clip each row to what the rows before it left free;
    // the narrower row sorted first, so the MAP_FIXED sub-mapping survives and the reservation yields, as
    // the kernel's VMA split leaves it.
    int keep = 0;
    uint64_t watermark = 0;
    for (int i = 0; i < nrow; i++) {
        if (rows[i].lo < watermark) {
            uint64_t shift = watermark - rows[i].lo;
            if (rows[i].hi <= watermark) continue; // fully swallowed
            rows[i].lo = watermark;
            if (rows[i].ino) rows[i].off += shift; // a file-backed row's offset tracks its start
        }
        watermark = rows[i].hi;
        rows[keep++] = rows[i];
    }
    t->row = rows;
    t->names = names;
    t->n = keep;
    return 1;
}

// Synthesize /proc/[pid]/maps (smaps=0) or /proc/[pid]/smaps (smaps=1). The [stack] line (with a guard
// line below it, as the kernel shows) is what glibc's pthread_getattr_np scans for; [heap] is what
// jemalloc/glibc-malloc/redis/pmap look for. Returns an anonymous fd holding the content, or -1 on error.
static int proc_maps_fd(int smaps) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    char tn[] = "/tmp/.hl-procXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) {
        maptable_free(&t);
        return -1;
    }
    if (fd < HL_NFD) g_proc_text_ro[fd] = 1;
    unlink(tn);
    char b[5120]; // one row: the header line (a PATH_MAX pathname) plus a full smaps field block, whole --
                  // a truncated row would lose its newline and merge into the next one.
    for (int i = 0; i < t.n; i++) {
        int m = proc_map_region_p(b, sizeof b, t.row[i].lo, t.row[i].hi, t.row[i].perms, t.row[i].off,
                                  t.row[i].dev_major, t.row[i].dev_minor, t.row[i].ino, t.row[i].name, smaps);
        if (write(fd, b, (size_t)m) < 0) {}
    }
    maptable_free(&t);
    lseek(fd, 0, SEEK_SET);
    return fd;
}

// /proc/[pid]/numa_maps -- one line per VMA, ascending, "<start> <policy> [tag] <counters>". Unintercepted
// this fell through to the host and handed the guest the ENGINE's mappings: the engine binary's absolute
// host path, its load address, and every library the host process had open. A containment failure, not a
// completeness gap, so it is synthesized from the same row set maps/smaps use. The kernel prints a bare
// "<start> default" for a VMA with no resident pages, so a PROT_NONE guard needs no counters.
static int proc_numa_maps_fd(void) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    char b[5120];
    char *out = NULL;
    int len = 0;
    for (int i = 0; i < t.n; i++) {
        const struct maprow *r = &t.row[i];
        unsigned long pages = (unsigned long)((r->hi - r->lo) / 4096);
        int resident = (r->perms[0] != '-' || r->perms[1] != '-' || r->perms[2] != '-');
        int m = snprintf(b, sizeof b, "%08lx default", (unsigned long)r->lo);
        if (r->name && !strcmp(r->name, "[heap]"))
            m += snprintf(b + m, sizeof b - (size_t)m, " heap");
        else if (r->name && !strcmp(r->name, "[stack]"))
            m += snprintf(b + m, sizeof b - (size_t)m, " stack");
        else if (r->ino && r->name && r->name[0]) {
            // The kernel escapes whitespace in the pathname as \040 here (numa_maps is space-delimited).
            m += snprintf(b + m, sizeof b - (size_t)m, " file=");
            for (const char *p = r->name; *p && m < (int)sizeof b - 8; p++) {
                if (*p == ' ')
                    m += snprintf(b + m, sizeof b - (size_t)m, "\\040");
                else
                    b[m++] = *p;
            }
            b[m] = 0;
        }
        if (resident && pages) {
            // Attribution matches smaps: a file-backed region's resident pages are page-cache (mapped=),
            // an anonymous one's are private dirty (anon=/dirty=). A summing reader must see both agree.
            if (r->ino)
                m += snprintf(b + m, sizeof b - (size_t)m, " mapped=%lu", pages);
            else
                m += snprintf(b + m, sizeof b - (size_t)m, " anon=%lu dirty=%lu", pages, pages);
            m += snprintf(b + m, sizeof b - (size_t)m, " active=0 N0=%lu kernelpagesize_kB=4", pages);
        }
        m += snprintf(b + m, sizeof b - (size_t)m, "\n");
        char *grown = (char *)realloc(out, (size_t)(len + m + 1));
        if (!grown) break;
        out = grown;
        memcpy(out + len, b, (size_t)m);
        len += m;
    }
    maptable_free(&t);
    int fd = proc_text_fd(out ? out : "", len);
    free(out);
    return fd;
}

// /proc/[pid]/smaps_rollup -- the whole-address-space totals, one "<first>-<last> ---p ... [rollup]" header
// plus the aggregate field block. Same leak as numa_maps unintercepted: the header alone published the
// engine's lowest and highest mapping. The fields are the per-region sums of what smaps already reports, so
// a reader that cross-checks rollup against smaps sees the two agree.
static int proc_smaps_rollup_fd(void) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    unsigned long rss = 0, pclean = 0, sdirty = 0, locked = 0;
    for (int i = 0; i < t.n; i++) {
        const struct maprow *r = &t.row[i];
        if (r->perms[0] == '-' && r->perms[1] == '-' && r->perms[2] == '-') continue; // PROT_NONE: not resident
        unsigned long kb = (unsigned long)((r->hi - r->lo) / 1024);
        rss += kb;
        if (r->ino)
            pclean += kb;
        else
            sdirty += kb;
        locked += (unsigned long)(hl_gmap_lock_region_bytes(r->lo, r->hi) / 1024);
    }
    unsigned long lo = t.n ? t.row[0].lo : 0, hi = t.n ? t.row[t.n - 1].hi : 0;
    maptable_free(&t);
    char b[2048];
    int m = snprintf(b, sizeof b, "%08lx-%08lx ---p 00000000 00:00 0", lo, hi);
    if (m < 72) m += snprintf(b + m, sizeof b - (size_t)m, "%*s", 72 - m, ""); // seq_pad: name at column 73
    m += snprintf(b + m, sizeof b - (size_t)m,
                  " [rollup]\nRss:%20lu kB\nPss:%20lu kB\nPss_Dirty:%14lu kB\nPss_Anon:%15lu kB\n"
                  "Pss_File:%15lu kB\nPss_Shmem:%14d kB\nShared_Clean:%11d kB\nShared_Dirty:%11lu kB\n"
                  "Private_Clean:%10lu kB\nPrivate_Dirty:%10d kB\nReferenced:%13lu kB\nAnonymous:%14lu kB\n"
                  "KSM:%20d kB\nLazyFree:%15d kB\nAnonHugePages:%10d kB\nShmemPmdMapped:%9d kB\n"
                  "FilePmdMapped:%10d kB\nShared_Hugetlb:%9d kB\nPrivate_Hugetlb:%8d kB\nSwap:%19d kB\n"
                  "SwapPss:%16d kB\nLocked:%17lu kB\n",
                  rss, rss, sdirty, sdirty, pclean, 0, 0, sdirty, pclean, 0, rss, sdirty, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                  locked);
    return proc_text_fd(b, m);
}

// The map_files/ entry name for a row: "<start>-<end>" in lowercase hex, unpadded -- the kernel's own
// naming. Only FILE-backed rows have one, which is what makes the directory a list of the objects the
// process has mapped. Returns 0 for an anonymous row.
static int map_files_name(const struct maprow *r, char *out, size_t n) {
    if (!r->ino || !r->name || !r->name[0]) return 0;
    snprintf(out, n, "%llx-%llx", (unsigned long long)r->lo, (unsigned long long)r->hi);
    return 1;
}

#define MAP_FILES_MAX 256

// /proc/[pid]/map_files/ -- a directory of "<start>-<end>" symlinks, one per file-backed VMA, each
// readlink'ing to the mapped path. Unintercepted this listed the ENGINE's own file mappings: its binary,
// the host loader and every host library, by absolute host path. Materialized as symlink placeholders;
// the targets are served by the readlink synth in fs.c (map_files_target).
static int proc_map_files_dir_open(void) {
    struct maptable t;
    if (!maptable_build(&t)) return -1;
    char (*names)[48] = (char (*)[48])calloc(MAP_FILES_MAX, 48);
    const char *ptr[MAP_FILES_MAX + 1];
    int n = 0;
    if (names)
        for (int i = 0; i < t.n && n < MAP_FILES_MAX; i++)
            if (map_files_name(&t.row[i], names[n], 48)) {
                ptr[n] = names[n];
                n++;
            }
    ptr[n] = NULL;
    maptable_free(&t);
    int fd = names ? synth_names_dir_open("/proc/self/map_files", ptr, 1) : -1;
    free(names);
    return fd;
}

// The readlink target of /proc/[pid]/map_files/<start>-<end>: the mapped path, or 0 if no file-backed row
// spans exactly that range (the kernel's names are exact VMA bounds, so a stale name must ENOENT).
static int map_files_target(const char *entry, char *out, size_t n) {
    struct maptable t;
    if (!entry || !entry[0] || !maptable_build(&t)) return 0;
    char nm[48];
    int found = 0;
    for (int i = 0; i < t.n && !found; i++)
        if (map_files_name(&t.row[i], nm, sizeof nm) && !strcmp(nm, entry)) {
            snprintf(out, n, "%s", t.row[i].name);
            found = 1;
        }
    maptable_free(&t);
    return found;
}

// /proc/[pid]/status -- the Name:/State:/VmRSS: key:value format (NOT the stat one-liner). VmRSS/VmSize
// reflect the cgroup memory charge so a reader sees a plausible footprint.
static unsigned long long self_rss_bytes(void); // defined after hl_get_procinfo (real engine resident floor)

// One current per-process footprint sample (resident + virtual, in bytes).
// /proc is live state on Linux: values may legitimately move between separate
// reads. Caching the first sample forever made statm claim that a faulted
// 32 MiB mapping consumed zero pages and that munmap never released anything.
static _Thread_local unsigned long long g_statm_charge;
static _Thread_local unsigned long long g_statm_rss;
static _Thread_local unsigned long long g_statm_vsize;
static _Thread_local int g_statm_sample;

static void self_vm_bytes(unsigned long long *rss, unsigned long long *vsize) {
    unsigned long long pgsz = (unsigned long long)hl_linux_host_page_size();
    unsigned long long r = (self_rss_bytes() / pgsz) * pgsz;
    unsigned long long v;
    if (r < pgsz) r = pgsz;
    v = g_mem_max ? (unsigned long long)g_mem_max : r + (4ull << 20);
    if (v < r) v = r;
    if (rss) *rss = r;
    if (vsize) *vsize = v;
}

static void self_vm_statm_bytes(unsigned long long *rss, unsigned long long *vsize) {
    self_vm_bytes(rss, vsize);
    g_statm_charge = (unsigned long long)atomic_load(&g_mem_charged);
    g_statm_rss = *rss;
    g_statm_vsize = *vsize;
    g_statm_sample = 1;
}

static void self_vm_status_bytes(unsigned long long *rss, unsigned long long *vsize) {
    unsigned long long charge = (unsigned long long)atomic_load(&g_mem_charged);
    if (g_statm_sample && g_statm_charge == charge) {
        *rss = g_statm_rss;
        *vsize = g_statm_vsize;
        g_statm_sample = 0;
        return;
    }
    self_vm_bytes(rss, vsize);
}

// /proc/[pid]/status Cpus_allowed / Cpus_allowed_list. A default container is allowed to run on ALL of its
// online CPUs (contiguous 0..N-1, N = container_online_cpus()), so this MUST agree with sched_getaffinity
// (dispatch.c cpu_online_mask) and nproc -- the old hardcoded "1"/"0" (CPU 0 only) contradicted both, and a
// reader like the JVM/tokio that cross-checks Cpus_allowed against availableProcessors saw an inconsistency
// no real container shows. Linux renders the mask as comma-separated 32-bit hex groups, most-significant
// first, no leading zeros on the top group (e.g. 18 CPUs -> "3ffff"); the list is the "0-(N-1)" range.
static void cpus_allowed_strs(char *mask, size_t mn, char *list, size_t ln) {
    int nc = container_online_cpus();
    if (nc < 1) nc = 1;
    uint32_t w[2] = {0, 0}; // container_online_cpus() caps at 64, so two 32-bit words cover every bit
    for (int c = 0; c < nc && c < 64; c++)
        w[c / 32] |= (uint32_t)1u << (c % 32);
    int hi = (nc - 1) / 32; // most-significant populated word
    int o = 0;
    for (int i = hi; i >= 0 && o < (int)mn; i--)
        o += snprintf(mask + o, mn - (size_t)o, i == hi ? "%x" : ",%08x", w[i]);
    if (nc == 1)
        snprintf(list, ln, "0");
    else
        snprintf(list, ln, "0-%d", nc - 1);
}

static int proc_status_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    int pid = container_pid();
    int ppid = proc_self_guest_ppid(pid);
    unsigned long long vm_rss, vm_vsize;
    self_vm_status_bytes(&vm_rss, &vm_vsize);
    unsigned long rss = (unsigned long)(vm_rss / 1024);
    unsigned long vsz = (unsigned long)(vm_vsize / 1024);
    if (vsz < rss) vsz = rss;
    unsigned long vmlck =
        (unsigned long)(hl_gmap_lock_total_bytes() / 1024); // mlock/mlockall'd bytes (LTP munlockall01)
    char groups[512]; // image-derived supplementary set (runc additionalGids), == getgroups(2)
    groups_status_str(groups, sizeof groups);
    char cpumask[40], cpulist[24];
    cpus_allowed_strs(cpumask, sizeof cpumask, cpulist, sizeof cpulist);
    // Identity must agree with getuid/geteuid/getgid/getegid (syscall/proc.c returns g_ruid/euid/…). A
    // hardcoded 0 made procfs report root even when the guest ran as a configured non-root uid/gid.
    cred_init(); // populate g_ruid/g_suid/… before we read them
    int uid_r = g_ruid, uid_e = cred_euid(), uid_s = g_suid, uid_fs = newfile_uid();
    int gid_r = g_rgid, gid_e = cred_egid(), gid_s = g_sgid, gid_fs = newfile_gid();
    int threads = thread_live_count(); // live pthreads (Threads: hid concurrency at a hardcoded 1)
    return snprintf(
        b, n,
        "Name:\t%s\nUmask:\t%04o\nState:\tR (running)\nTgid:\t%d\nNgid:\t0\nPid:\t%d\nPPid:\t%d\n"
        "TracerPid:\t0\nUid:\t%d\t%d\t%d\t%d\nGid:\t%d\t%d\t%d\t%d\nFDSize:\t256\nGroups:\t%s\n"
        "VmPeak:\t%8lu kB\nVmSize:\t%8lu kB\nVmLck:\t%8lu kB\nVmHWM:\t%8lu kB\nVmRSS:\t%8lu kB\n"
        "VmData:\t%8lu kB\nVmStk:\t     132 kB\nVmExe:\t     512 kB\nVmLib:\t    2048 kB\nVmPTE:\t      32 kB\n"
        "VmSwap:\t       0 kB\nThreads:\t%d\nSigQ:\t0/31000\nSigPnd:\t0000000000000000\n"
        "SigBlk:\t0000000000000000\nSigIgn:\t0000000000000000\nSigCgt:\t0000000000000000\n"
        // Capability + security context. A default `docker run` root container drops all but 14
        // caps: CapPrm/CapEff/CapBnd=00000000a80425fb, CapInh/CapAmb=0. NoNewPrivs follows the
        // sticky prctl flag; the docker default seccomp profile shows Seccomp:2/Seccomp_filters:1.
        // These MUST agree with capget(2) and PR_CAPBSET_READ (see syscall/proc.c). Speculation
        // lines match what the host kernel reports to a container.
        "CapInh:\t%016llx\nCapPrm:\t%016llx\nCapEff:\t%016llx\nCapBnd:\t%016llx\n"
        "CapAmb:\t%016llx\nNoNewPrivs:\t%d\nSeccomp:\t2\nSeccomp_filters:\t1\n"
        "Speculation_Store_Bypass:\tvulnerable\nSpeculationIndirectBranch:\tunknown\n"
        "Cpus_allowed:\t%s\nCpus_allowed_list:\t%s\nvoluntary_ctxt_switches:\t1\n"
        "nonvoluntary_ctxt_switches:\t0\n",
        comm, (unsigned)g_umask, pid, pid, ppid, uid_r, uid_e, uid_s, uid_fs, gid_r, gid_e, gid_s, gid_fs, groups, vsz,
        vsz, vmlck, rss, rss, rss, threads, (unsigned long long)g_cap_inh, (unsigned long long)g_cap_prm,
        (unsigned long long)g_cap_eff, (unsigned long long)g_cap_bnd, (unsigned long long)g_cap_amb, g_nnp, cpumask,
        cpulist);
}

// /proc/[pid]/stat -- the 52-field single line (pid (comm) state ppid ...). Field 23 = vsize (bytes),
// field 24 = rss (pages); the rest are plausible zeros. mongod's FTDC collector parses this.
static void proc_self_terminal_identity(int *tty_device, int *foreground_group) {
    *tty_device = 0;
    *foreground_group = -1;
    for (int descriptor = 0; descriptor <= 2; ++descriptor) {
        if (!isatty(descriptor)) continue;
        struct stat status;
        pid_t foreground = tcgetpgrp(descriptor);
        if (foreground <= 0 || fstat(descriptor, &status) != 0 || !S_ISCHR(status.st_mode)) continue;
        uint32_t device = hl_linux_device_make(hl_host_device_major((uint64_t)status.st_rdev),
                                               hl_host_device_minor((uint64_t)status.st_rdev));
        *tty_device = (int)device;
        *foreground_group = guest_pgid_from_host((int)foreground);
        if (*foreground_group == 0) *foreground_group = -1;
        return;
    }
}

static int proc_stat_text(char *b, size_t n) {
    char comm[16];
    proc_comm(comm, sizeof comm);
    int pid = container_pid();
    int ppid = proc_self_guest_ppid(pid);
    // Fields 5 (pgrp) and 6 (session) must match the guest's getpgrp()/getsid() -- for a forked child those
    // are its real host process group / session (init's real group/session mapped to guest 1), NOT the
    // child's own pid. The old code printed pid,pid, so a supervisor reconstructed a wrong process tree.
    int hpgrp = (int)getpgid(0), hsid = (int)getsid(0);
    int gpgrp = guest_pgid_from_host(hpgrp);
    int gsid = guest_sid_from_host(hsid);
    if (gpgrp <= 0) gpgrp = pid;
    if (gsid <= 0) gsid = pid;
    int tty_device, foreground_group;
    proc_self_terminal_identity(&tty_device, &foreground_group);
    unsigned long pgsz = (unsigned long)hl_linux_host_page_size();
    unsigned long long vm_rss, vm_vsize;
    self_vm_bytes(&vm_rss, &vm_vsize);
    unsigned long rss_pg = (unsigned long)(vm_rss / pgsz);
    unsigned long vsize = (unsigned long)vm_vsize;
    // 26/27 startcode/endcode, 45/46 start_data/end_data, 47 start_brk. Field 38 (exit_signal, SIGCHLD=17)
    // used to sit at 39: one zero too many followed it field 25, which shifted every field from 26 up by
    // one, so a reader indexing by position got the wrong column for all of them.
    uint64_t sc, ec, sd, ed;
    maps_code_data_bounds(&sc, &ec, &sd, &ed);
    return snprintf(b, n,
                    "%d (%s) R %d %d %d %d %d 4194560 0 0 0 0 0 0 0 0 20 0 1 0 100 %lu %lu 18446744073709551615 "
                    "%llu %llu 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 %llu %llu %llu 0 0 0 0 0\n",
                    pid, comm, ppid, gpgrp, gsid, tty_device, foreground_group, vsize, rss_pg,
                    (unsigned long long)sc, (unsigned long long)ec, (unsigned long long)sd, (unsigned long long)ed,
                    (unsigned long long)brk_lo);
}

// /proc/[pid]/environ -- the guest environment as NUL-separated KEY=VALUE. The authoritative source is
// HL_GUEST_ENV (the serialized guest environment, "K=V\nK=V"); absent it (direct mode), fall
// back to the same defaults build_stack hands the guest. Returns the byte count written.
// The running process's FINAL environment (container env + merged engine defaults), captured by build_stack
// -- the exact set placed on the guest stack, i.e. what hl_option_get() sees. /proc/self/environ was generated from
// the raw HL_GUEST_ENV instead, omitting the defaults (HOME/LANG/…) build_stack adds, so procfs disagreed
// with getenv. Using this blob makes them consistent. (build_stack in elf.c is compiled after vfs.c.)
static char g_self_environ[16384];
static int g_self_environ_len = 0;
static int g_self_environ_valid = 0;
