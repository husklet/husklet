static void cpumask_hex(char *out, size_t n, int nc, int all, int bit, int ndig) {
    if (!out || n == 0) return;
    if (nc < 1) nc = 1;
    if (nc > 64) nc = 64;
    if (ndig < 1) ndig = 1;
    if (ndig > 8) ndig = 8;
    unsigned long long v = all ? (nc >= 64 ? ~0ULL : ((1ULL << nc) - 1ULL)) : (1ULL << (bit & 63));
    if (nc <= 32) {
        snprintf(out, n, "%0*llx", ndig, v & 0xffffffffULL);
        return;
    }
    int hidig = ((nc - 32) + 3) / 4;
    if (hidig < 1) hidig = 1;
    snprintf(out, n, "%0*x,%08x", hidig, (unsigned)(v >> 32), (unsigned)(v & 0xffffffffULL));
}

// The CONTENT of one /sys/devices/system/cpu/cpuN/topology/<leaf> attribute. hl advertises a FLAT topology:
// single socket (physical_package_id 0), no SMT (each logical CPU is its own core -> core_id = cpuN, thread
// siblings = {cpuN}), all online CPUs in one package. lscpu/util-linux reconstruct sockets/cores/threads
// from exactly these files; real docker always serves them, so an ENOENT here is a engine-specific divergence that
// makes lscpu mis-count or error. Returns the NUL-terminated length, or -1 if `leaf` is not one we serve.
static int syscpu_topology_str(const char *leaf, int cpuN, int nc, char *out, size_t n) {
    int ndig = (nc + 3) / 4;
    if (ndig < 1) ndig = 1;
    if (!strcmp(leaf, "core_id")) return snprintf(out, n, "%d\n", cpuN);
    if (!strcmp(leaf, "physical_package_id") || !strcmp(leaf, "cluster_id")) return snprintf(out, n, "0\n");
    if (!strcmp(leaf, "thread_siblings_list") || !strcmp(leaf, "core_cpus_list")) return snprintf(out, n, "%d\n", cpuN);
    if (!strcmp(leaf, "core_siblings_list") || !strcmp(leaf, "package_cpus_list") || !strcmp(leaf, "cluster_cpus_list"))
        return nc > 1 ? snprintf(out, n, "0-%d\n", nc - 1) : snprintf(out, n, "0\n");
    char m[96];
    if (!strcmp(leaf, "thread_siblings") || !strcmp(leaf, "core_cpus")) {
        cpumask_hex(m, sizeof m, nc, 0, cpuN, ndig);
        return snprintf(out, n, "%s\n", m);
    }
    if (!strcmp(leaf, "core_siblings") || !strcmp(leaf, "package_cpus") || !strcmp(leaf, "cluster_cpus")) {
        cpumask_hex(m, sizeof m, nc, 1, 0, ndig);
        return snprintf(out, n, "%s\n", m);
    }
    return -1;
}

// Parse+serve a full /sys/devices/system/cpu/cpuN/topology/<leaf> path. Returns content length (out is
// NUL-terminated) or -1 if `rp` is not a topology file we synthesize (bad cpuN, unknown leaf, wrong shape).
static int syscpu_topology_content(const char *rp, char *out, size_t n) {
    if (!rp || strncmp(rp, "/sys/devices/system/cpu/cpu", 27)) return -1;
    const char *d = rp + 27;
    if (*d < '0' || *d > '9') return -1;
    int cpuN = 0;
    for (; *d >= '0' && *d <= '9'; d++)
        cpuN = cpuN * 10 + (*d - '0');
    if (strncmp(d, "/topology/", 10)) return -1;
    const char *leaf = d + 10;
    if (!*leaf || strchr(leaf, '/')) return -1;
    int nc = container_online_cpus();
    if (cpuN < 0 || cpuN >= nc) return -1;
    return syscpu_topology_str(leaf, cpuN, nc, out, n);
}

// Format 16 raw bytes as a Linux UUID string ("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx\n"), stamping the
// RFC-4122 version-4 (b[6]) and variant (b[8]) bits so the result parses as a valid random UUID. Writes
// 37 bytes (36 + '\n') plus a NUL into out (needs >= 38). Returns the byte count (37).
static int uuid_fmt(char *out, size_t cap, uint8_t b[16]) {
    b[6] = (uint8_t)((b[6] & 0x0f) | 0x40);
    b[8] = (uint8_t)((b[8] & 0x3f) | 0x80);
    return snprintf(out, cap, "%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-%02x%02x%02x%02x%02x%02x\n", b[0], b[1],
                    b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]);
}

// The 16 raw bytes of the container's boot identity. Must be STABLE for the container's whole life AND
// IDENTICAL across every process in it (each guest process is a separate host engine, so a per-process
// arc4random value would disagree between peers). Derived DETERMINISTICALLY from the per-container
// registry key (HL_NETNS, minted at startup and inherited across fork/execve so every peer
// agrees -- see proc_reg_key) via FNV-1a expanded to 16 bytes. Same container -> same bytes everywhere;
// different containers -> different bytes. Backs both boot_id (UUID) and machine-id (32 hex).
static void boot_id_bytes(uint8_t b[16]) {
    char key[80];
    proc_reg_key(key, sizeof key);       // HL_NETNS -> HL_HOSTNAME -> session id fallback
    uint64_t h = 1469598103934665603ULL; // FNV-1a offset basis
    for (const char *p = key; *p; p++) {
        h ^= (uint8_t)*p;
        h *= 1099511628211ULL;
    }
    for (int i = 0; i < 16; i++) {
        b[i] = (uint8_t)(h >> ((i & 7) * 8));
        if ((i & 7) == 7) h = h * 6364136223846793005ULL + 1442695040888963407ULL; // advance for hi 8 bytes
    }
}

// /proc/sys/kernel/random/boot_id (systemd/dbus/libuuid/journald key machine state off it).
static int proc_boot_id(char *out, size_t cap) {
    uint8_t b[16];
    boot_id_bytes(b);
    return uuid_fmt(out, cap, b);
}

// /proc/[self|<pid>]/limits -- the rlimit table (Go runtime, nginx, java, systemd read RLIMIT_NOFILE from
// it). Values mirror the engine's own getrlimit/prlimit answers (svc_fill_rlimit: stack 8MB, nofile
// 20480/1048576, everything else unlimited) so the file and the syscall agree.
static int proc_limits_text(char *buf, size_t cap) {
    // name, soft, hard, units ("" -> no unit column value). "unlimited" for RLIM_INFINITY rows.
    static const struct {
        const char *nm, *soft, *hard, *unit;
    } L[] = {
        {"Max cpu time", "unlimited", "unlimited", "seconds"},
        {"Max file size", "unlimited", "unlimited", "bytes"},
        {"Max data size", "unlimited", "unlimited", "bytes"},
        {"Max stack size", "8388608", "unlimited", "bytes"},
        {"Max core file size", "0", "unlimited", "bytes"}, // cores OFF (soft=0), matching getrlimit(RLIMIT_CORE)
        {"Max resident set", "unlimited", "unlimited", "bytes"},
        {"Max processes", "unlimited", "unlimited", "processes"},
        {"Max open files", "20480", "1048576", "files"}, // oracle (docker default soft): was 1024
        {"Max locked memory", "unlimited", "unlimited", "bytes"},
        {"Max address space", "unlimited", "unlimited", "bytes"},
        {"Max file locks", "unlimited", "unlimited", "locks"},
        {"Max pending signals", "unlimited", "unlimited", "signals"},
        {"Max msgqueue size", "unlimited", "unlimited", "bytes"},
        {"Max nice priority", "0", "0", ""},
        {"Max realtime priority", "0", "0", ""},
        {"Max realtime timeout", "unlimited", "unlimited", "us"},
    };

    // NOFILE hard cap is the enforceable guest fd ceiling (hl_engine_guest_fd_limit, derived from the host
    // RLIMIT_NOFILE and HL_LINUX_FD_LIMIT). getrlimit/prlimit64 report exactly this value (svc_fill_rlimit),
    // so the /proc row must render the same number rather than a stale hard-coded 1048576 -- otherwise the
    // syscall surface and /proc/self/limits disagree (glibc/JVM/systemd read both).
    char nofile_hard[24];
    {
        uint32_t guest_limit = hl_engine_guest_fd_limit();
        snprintf(nofile_hard, sizeof nofile_hard, "%u", guest_limit > 0 ? guest_limit : 20480u);
    }

    int n = snprintf(buf, cap, "%-25s %-20s %-20s %-10s\n", "Limit", "Soft Limit", "Hard Limit", "Units");
    for (size_t i = 0; i < sizeof L / sizeof *L; i++) {
        const char *soft = L[i].soft, *hard = L[i].hard;
        if (i == 7) hard = nofile_hard; // RLIMIT_NOFILE: mirror getrlimit's enforceable hard cap
        // docker --ulimit override (g_limits, resource number == table index): render the requested values
        // so /proc/self/limits agrees with getrlimit (svc_fill_rlimit). RLIM_INFINITY -> "unlimited".
        char sb[24], hb[24];
        uint64_t current, maximum;
        if (i < HL_LIMIT_COUNT && hl_limit_table_get(&g_limits, (int)i, &current, &maximum)) {
            if (current == ~0ull)
                soft = "unlimited";
            else {
                snprintf(sb, sizeof sb, "%llu", (unsigned long long)current);
                soft = sb;
            }
            if (maximum == ~0ull)
                hard = "unlimited";
            else {
                snprintf(hb, sizeof hb, "%llu", (unsigned long long)maximum);
                hard = hb;
            }
        }
        n += snprintf(buf + n, cap - (size_t)n, "%-25s %-20s %-20s %-10s\n", L[i].nm, soft, hard, L[i].unit);
    }
    return n;
}

// ---- runc/containerd MaskedPaths + ReadonlyPaths (container isolation, spec.go DefaultSpec) ----
// Masked paths must EXIST but be empty/inaccessible (NOT ENOENT), so monitoring agents and systemd unit
// `ConditionPathExists` checks that stat them behave as under runc. Kind: 1 = masked FILE (opens as an empty
// file, reads 0 bytes -- runc binds /dev/null over it); 2 = masked DIR (opens as an empty dir -- runc mounts
// an empty tmpfs). `rp` is the container-absolute path. Exact list = containerd pkg/oci spec.go MaskedPaths.
static int proc_masked_kind(const char *rp) {
    if (!rp) return 0;
    static const char *const files[] = {"/proc/kcore",
                                        "/proc/keys",
                                        "/proc/latency_stats",
                                        "/proc/timer_list",
                                        "/proc/timer_stats",
                                        "/proc/sched_debug",
                                        0};
    static const char *const dirs[] = {
        "/proc/asound", "/proc/acpi", "/proc/scsi", "/sys/firmware", "/sys/devices/virtual/powercap", 0};
    for (int i = 0; files[i]; i++)
        if (!strcmp(rp, files[i])) return 1;
    for (int i = 0; dirs[i]; i++) {
        size_t L = strlen(dirs[i]);
        if (!strncmp(rp, dirs[i], L) && (rp[L] == 0 || rp[L] == '/')) return 2; // the dir or anything within it
    }
    return 0;
}

// 1 if `rp` is a runc ReadonlyPath (/proc/bus /proc/fs /proc/irq /proc/sys /proc/sysrq-trigger): reads are
// allowed (served by the /proc synth or an empty dir), writes fail EROFS -- runc bind-mounts these read-only.
static int proc_ro_path(const char *rp) {
    if (!rp) return 0;
    if (!strcmp(rp, "/proc/sysrq-trigger")) return 1;
    static const char *const dirs[] = {"/proc/bus", "/proc/fs", "/proc/irq", "/proc/sys", 0};
    for (int i = 0; dirs[i]; i++) {
        size_t L = strlen(dirs[i]);
        if (!strncmp(rp, dirs[i], L) && (rp[L] == 0 || rp[L] == '/')) return 1;
    }
    return 0;
}

// 1 if `rp` is one of the ReadonlyPath DIRECTORIES that has no other synth (so stat/opendir see an empty,
// read-only directory). /proc/sys is served by proc_open; /proc/sysrq-trigger is a file (handled separately).
static int proc_ro_dir(const char *rp) {
    if (!rp) return 0;
    static const char *const dirs[] = {"/proc/bus", "/proc/fs", "/proc/irq", 0};
    for (int i = 0; dirs[i]; i++) {
        size_t L = strlen(dirs[i]);
        if (!strncmp(rp, dirs[i], L) && (rp[L] == 0 || rp[L] == '/')) return 1;
    }
    return 0;
}

// Materialize a fresh EMPTY temp directory and return an O_DIRECTORY fd to it (reaped when the guest closes
// the fd, via the shared g_procfd_dirs machinery). Backs masked dirs + read-only proc dirs: getdents yields
// nothing, exactly like runc's empty-tmpfs mask. -1 on error.
static int empty_dir_fd(const char *guestpath) {
    static int registered = 0;
    if (!registered) {
        atexit(procfd_dirs_atexit);
        registered = 1;
    }
    procfd_dirs_reap(0);
    char tmpl[] = "/tmp/.hl-maskXXXXXX";
    if (!mkdtemp(tmpl)) return -1;
    int fd = open(tmpl, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        procfd_dir_rm(tmpl);
        return -1;
    }
    proc_dir_register(fd, tmpl, guestpath);
    return fd;
}

// Serve a masked / read-only-dir proc path as an open fd (empty file or empty dir). Returns the fd, or -2 if
// `rp` is not one hl masks (so the caller falls through to the normal path). Reserved for READ opens; the
// write-intent EROFS for ReadonlyPaths is enforced in openat before this is reached.
static int proc_masked_open(const char *rp) {
    int mk = proc_masked_kind(rp);
    if (mk == 1) return proc_text_fd("", 0);                            // empty regular file
    if (mk == 2) return empty_dir_fd(rp);                               // empty directory
    if (proc_ro_dir(rp)) return empty_dir_fd(rp);                       // /proc/bus,/fs,/irq: exist, empty, read-only
    if (!strcmp(rp, "/proc/sysrq-trigger")) return proc_text_fd("", 0); // exists, empty on read
    return -2;
}

// Real macOS stat -> Linux struct stat (the fake S_IFCHR version corrupted libc buffering).
// fill_linux_stat (the guest struct-stat layout) is per-arch -> translator/guest/<arch>/stat.c
// Synthesize the common /proc files Linux programs read (macOS has no /proc). Returns an fd
// holding the content, -1 on mkstemp error, or -2 if rp isn't a path we synthesize.
// Guest ISA from the auxv AT_PLATFORM string (type 15: "x86_64" vs "aarch64") the loader planted -- lets
// this shared TU tailor arch-specific pseudo-file content (e.g. /proc/cpuinfo) without a per-arch macro.
static int guest_is_x86(void) {
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == 15 && v) return strncmp((const char *)(uintptr_t)v, "x86", 3) == 0;
    }
    return 0;
}

// ---- /proc/cpuinfo, one CPU model, two renderings --------------------------
// Both blocks below are DERIVED, never restated: the guest must not be able to get two different answers
// to "what CPU is this" from CPUID/auxv and from /proc. Each side reads the same single source the auxv
// reads -- hl_x86_cpuid() for the x86-64 guest, AT_HWCAP/AT_HWCAP2 (copied verbatim out of
// g_aarch64_cpu_model by the loader) for the aarch64 guest. tests/compat/procfs/cpumodel.c gates both.
// The arch is a compile-time property of the engine binary (one guest frontend per build), so the split is
// the same G_* seam every other per-guest detail uses -- and only the x86-64 build links hl_x86_cpuid.
#if G_SECCOMP_ARCH == 0xC000003Eu // AUDIT_ARCH_X86_64
#include "../../../translator/guest/x86_64/cpuid.h"

// One CPUID leaf/subleaf, exactly as the guest's own CPUID instruction answers it -> {eax,ebx,ecx,edx}.
static void cpuinfo_cpuid(uint32_t leaf, uint32_t sub, uint32_t out[4]) {
    struct cpu probe = {0}; // hl_x86_cpuid reads RAX/RCX and writes RAX..RDX; nothing else is touched
    probe.r[RAX] = leaf;
    probe.r[RCX] = sub;
    hl_x86_cpuid(&probe);
    out[0] = (uint32_t)probe.r[RAX];
    out[1] = (uint32_t)probe.r[RBX];
    out[2] = (uint32_t)probe.r[RCX];
    out[3] = (uint32_t)probe.r[RDX];
}

// CPUID bit -> /proc/cpuinfo flag token, in the order Linux prints them (x86_cap_flags word order).
// `reg` indexes {eax,ebx,ecx,edx}. constant_tsc/nonstop_tsc are both the one invariant-TSC bit; `cpuid`
// and `nopl` are Linux synthetics every long-mode CPU gets, so they hang off LM. Nothing here is a
// standing claim: a flag appears iff hl_x86_cpuid sets its bit, so withholding MOVBE drops `movbe` too.
static const struct {
    uint32_t leaf, sub;
    uint8_t reg, bit;
    const char *name;
} X86_FLAG[] = {
    {1, 0, 3, 0, "fpu"},
    {1, 0, 3, 4, "tsc"},
    {1, 0, 3, 8, "cx8"},
    {1, 0, 3, 11, "sep"},
    {1, 0, 3, 13, "pge"},
    {1, 0, 3, 15, "cmov"},
    {1, 0, 3, 19, "clflush"},
    {1, 0, 3, 23, "mmx"},
    {1, 0, 3, 24, "fxsr"},
    {1, 0, 3, 25, "sse"},
    {1, 0, 3, 26, "sse2"},
    {0x80000001, 0, 3, 11, "syscall"},
    {0x80000001, 0, 3, 20, "nx"},
    {0x80000001, 0, 3, 27, "rdtscp"},
    {0x80000001, 0, 3, 29, "lm"},
    {0x80000007, 0, 3, 8, "constant_tsc"},
    {0x80000007, 0, 3, 8, "nonstop_tsc"},
    {0x80000001, 0, 3, 29, "cpuid"},
    {0x80000001, 0, 3, 29, "nopl"},
    {1, 0, 2, 0, "pni"},
    {1, 0, 2, 1, "pclmulqdq"},
    {1, 0, 2, 9, "ssse3"},
    {1, 0, 2, 13, "cx16"},
    {1, 0, 2, 19, "sse4_1"},
    {1, 0, 2, 20, "sse4_2"},
    {1, 0, 2, 22, "movbe"},
    {1, 0, 2, 23, "popcnt"},
    {1, 0, 2, 25, "aes"},
    {0x80000001, 0, 2, 0, "lahf_lm"},
    {7, 0, 1, 3, "bmi1"},
    {7, 0, 1, 8, "bmi2"},
    {7, 0, 1, 9, "erms"},
    {7, 0, 1, 29, "sha_ni"},
    {7, 0, 3, 4, "fsrm"},
};

// x86-64 /proc/cpuinfo block for one logical CPU: vendor, family/model/stepping, brand string, cpuid
// level, address sizes and the flag list all decoded out of the CPUID leaves themselves.
static int cpuinfo_x86_block(char *b, size_t n, int idx, int ncpu) {
    uint32_t l0[4], l1[4], ext[4], sizes[4];
    cpuinfo_cpuid(0, 0, l0);
    cpuinfo_cpuid(1, 0, l1);
    cpuinfo_cpuid(0x80000000u, 0, ext);
    cpuinfo_cpuid(0x80000008u, 0, sizes);
    char vendor[13];
    memcpy(vendor, &l0[1], 4);
    memcpy(vendor + 4, &l0[3], 4);
    memcpy(vendor + 8, &l0[2], 4);
    vendor[12] = 0;
    unsigned family = (l1[0] >> 8) & 0xf, model = (l1[0] >> 4) & 0xf;
    if (family == 0xf) family += (l1[0] >> 20) & 0xff;
    if (family == 6 || family == 0xf) model |= ((l1[0] >> 16) & 0xf) << 4;
    char brand[49] = {0}; // brand leaves are space-padded; Linux prints the trimmed string
    if (ext[0] >= 0x80000004u)
        for (uint32_t i = 0; i < 3; i++) {
            uint32_t r[4];
            cpuinfo_cpuid(0x80000002u + i, 0, r);
            memcpy(brand + i * 16, r, 16);
        }
    const char *name = brand;
    while (*name == ' ')
        name++;
    char flags[512];
    int fn = 0;
    flags[0] = 0;
    for (size_t i = 0; i < sizeof X86_FLAG / sizeof X86_FLAG[0]; i++) {
        uint32_t r[4];
        cpuinfo_cpuid(X86_FLAG[i].leaf, X86_FLAG[i].sub, r);
        if (!((r[X86_FLAG[i].reg] >> X86_FLAG[i].bit) & 1u)) continue;
        int w = snprintf(flags + fn, sizeof flags - (size_t)fn, "%s%s", fn ? " " : "", X86_FLAG[i].name);
        if (w < 0 || (size_t)w >= sizeof flags - (size_t)fn) break;
        fn += w;
    }
    return snprintf(b, n,
                    "processor\t: %d\nvendor_id\t: %s\ncpu family\t: %u\nmodel\t\t: %u\n"
                    "model name\t: %s\nstepping\t: %u\nmicrocode\t: 0x1\ncpu MHz\t\t: 2500.000\n"
                    "cache size\t: 8192 KB\nphysical id\t: 0\nsiblings\t: %d\ncore id\t\t: %d\ncpu cores\t: %d\n"
                    "apicid\t\t: %d\ninitial apicid\t: %d\nfpu\t\t: yes\nfpu_exception\t: yes\ncpuid level\t: %u\n"
                    "wp\t\t: yes\nflags\t\t: %s\n"
                    "bugs\t\t:\nbogomips\t: 5000.00\nclflush size\t: 64\ncache_alignment\t: 64\n"
                    "address sizes\t: %u bits physical, %u bits virtual\npower management:\n\n",
                    idx, vendor, family, model, name, l1[0] & 0xf, ncpu, idx, ncpu, idx, idx, l0[0], flags,
                    sizes[0] & 0xff, (sizes[0] >> 8) & 0xff);
}

#define cpuinfo_block(b, n, i, nc) cpuinfo_x86_block((b), (n), (i), (nc))
#else
// HWCAP/HWCAP2 bit -> the token arch/arm64/kernel/cpuinfo.c prints; NULL is a bit Linux does not name.
static const char *const ARM_HWCAP[64] = {
    "fp",    "asimd",    "evtstrm", "aes",   "pmull",  "sha1",  "sha2", "crc32", "atomics", "fphp",    "asimdhp",
    "cpuid", "asimdrdm", "jscvt",   "fcma",  "lrcpc",  "dcpop", "sha3", "sm3",   "sm4",     "asimddp", "sha512",
    "sve",   "asimdfhm", "dit",     "uscat", "ilrcpc", "flagm", "ssbs", "sb",    "paca",    "pacg"};
static const char *const ARM_HWCAP2[64] = {"dcpodp",  "sve2",   "sveaes", "svepmull", "svebitperm", "svesha3",
                                           "svesm4",  "flagm2", "frint",  "svei8mm",  "svef32mm",   "svef64mm",
                                           "svebf16", "i8mm",   "bf16",   "dgh",      "rng",        "bti",
                                           "mte",     "ecv",    "afp",    "rpres"};

// The value the loader planted for auxv entry `type`, or 0 when there is none.
static uint64_t guest_auxv(uint64_t type) {
    for (int i = 0; i + 16 <= g_auxv_len; i += 16) {
        uint64_t t, v;
        memcpy(&t, g_auxv_data + i, 8);
        memcpy(&v, g_auxv_data + i + 8, 8);
        if (t == type) return v;
    }
    return 0;
}

// aarch64 /proc/cpuinfo block for one logical CPU. `Features` is the decode of the SAME AT_HWCAP/AT_HWCAP2
// pair the guest reads from its own auxv, so the seven features hl advertises beyond fp/asimd (aes pmull
// sha1 sha2 crc32 atomics asimddp) can no longer be missing from one surface and present on the other.
static int cpuinfo_arm_block(char *b, size_t n, int idx) {
    const uint64_t caps[2] = {guest_auxv(16), guest_auxv(26)};
    const char *const *names[2] = {ARM_HWCAP, ARM_HWCAP2};
    char feat[512];
    int fn = 0;
    feat[0] = 0;
    for (int word = 0; word < 2; word++)
        for (int i = 0; i < 64; i++) {
            if (!((caps[word] >> i) & 1u) || !names[word][i]) continue;
            int w = snprintf(feat + fn, sizeof feat - (size_t)fn, "%s%s", fn ? " " : "", names[word][i]);
            if (w < 0 || (size_t)w >= sizeof feat - (size_t)fn) break;
            fn += w;
        }
    return snprintf(b, n,
                    "processor\t: %d\nBogoMIPS\t: 100.00\nFeatures\t: %s\nCPU implementer\t: 0x61\n"
                    "CPU architecture: 8\nCPU variant\t: 0x0\nCPU part\t: 0x000\nCPU revision\t: 0\n\n",
                    idx, feat);
}

#define cpuinfo_block(b, n, i, nc) ((void)(nc), cpuinfo_arm_block((b), (n), (i)))
#endif

// Defined later in netns.c (same TU, included after vfs.c): emit the LISTEN rows for /proc/net/tcp[6].
static int netns_tcp_emit(char *out, size_t cap, int v6);

static int proc_canonical_path(const char *rp, char *out, size_t cap) {
    // Per-thread files mirror the main process for a single-threaded proc: fold
    // /proc/<pid>/task/<tid>/<leaf> -> /proc/<pid>/<leaf> so htop's per-thread reads are served.
    {
        const char *t = strstr(rp, "/task/");
        if (t && !strncmp(rp, "/proc/", 6)) {
            const char *q = rp + 6;
            int k = 0;
            while (q[k] >= '0' && q[k] <= '9')
                k++;
            const char *s = t + 6; // after "/task/"
            while (*s >= '0' && *s <= '9')
                s++;
            if (s > t + 6 && *s == '/') { // a real /task/<tid>/ segment with a trailing leaf
                // The pid segment between /proc/ and /task is EITHER numeric OR the "self"/"thread-self"
                // magic name -- /proc/self/task/<tid>/<leaf> must fold just like the numeric form (else a
                // task walker that descends /proc/self/task/<tid> can list but not open its files).
                int seglen = (int)(t - q);
                int is_self =
                    (seglen == 4 && !strncmp(q, "self", 4)) || (seglen == 11 && !strncmp(q, "thread-self", 11));
                int is_num = (k > 0 && q + k == t);
                if (!is_self && !is_num) return -1;
                char tbuf[16];
                int tlen = (int)(s - (t + 6));
                tlen = tlen < (int)sizeof tbuf ? tlen : (int)sizeof tbuf - 1;
                memcpy(tbuf, t + 6, (size_t)tlen);
                tbuf[tlen] = 0;
                int pid = is_self ? container_pid() : atoi(q);
                if (!proc_task_tid_visible(pid, atoi(tbuf))) return -2;
                int head = (int)(t - rp);
                snprintf(out, cap, "%.*s%s", head, rp, s);
                rp = out;
            }
        }
    }
    // the per-process network files are namespaced but a container is one net-namespace, so
    // /proc/[self|<pid>]/net/<leaf> mirrors the shared /proc/net/<leaf>. Fold it (ss/some Go/netlink
    // fallbacks read /proc/self/net/*). Without this those reads ENOENT'd under hl.
    if (!strncmp(rp, "/proc/", 6)) {
        const char *q = rp + 6;
        const char *leaf2 = NULL;
        if (!strncmp(q, "self/net/", 9))
            leaf2 = q + 9;
        else {
            const char *d = q;
            while (*d >= '0' && *d <= '9')
                d++;
            if (d > q && !strncmp(d, "/net/", 5)) leaf2 = d + 5;
        }
        if (leaf2) {
            snprintf(out, cap, "/proc/net/%s", leaf2);
            rp = out;
        }
    }
    if (rp != out) snprintf(out, cap, "%s", rp);
    return 0;
}

static int proc_open_self_process(const char *rp) {
    char buf[8192];
    int n = -1;
    // Per-process files for the guest's own pid: /proc/[self|pid]/{fd,maps,smaps,status,stat,environ}.
    const char *leaf = proc_self_leaf(rp);
    if (leaf) {
        if (!strncmp(leaf, "ns/", 3) && leaf[3] && ns_clone_flag(leaf + 3)) {
            char desc[64];
            snprintf(desc, sizeof desc, "namespace:%s", leaf + 3);
            return proc_text_fd_tagged("", 0, desc);
        }
        if (!strcmp(leaf, "fd")) return proc_fd_dir_open();
        if (!strncmp(leaf, "fdinfo/", 7) && leaf[7]) { // /proc/self/fdinfo/<N> body
            int isnum = 1;
            for (const char *t = leaf + 7; *t; t++)
                if (*t < '0' || *t > '9') isnum = 0;
            if (isnum) {
                int fn = atoi(leaf + 7);
                int m = proc_fdinfo_text(fn, buf, sizeof buf);
                if (m < 0) return -2; // closed/invalid fd -> ENOENT
                return proc_text_fd(buf, m);
            }
        }
        if (!strcmp(leaf, "pagemap")) {
            // VA-indexed binary pagemap: back it with an empty seekable regular fd (lseek to vaddr/pg*8
            // works natively) and synthesize the 8-byte-per-page entries on read (io.c). LTP mmap12.
            int fd = proc_text_fd("", 0);
            if (fd >= 0 && fd < HL_NFD) g_pagemap_fd[fd] = 1;
            return fd;
        }
        if (!strcmp(leaf, "maps") || !strcmp(leaf, "task/1/maps")) return proc_maps_fd(0);
        if (!strcmp(leaf, "smaps")) return proc_maps_fd(1);
        if (!strcmp(leaf, "numa_maps")) return proc_numa_maps_fd();
        if (!strcmp(leaf, "smaps_rollup")) return proc_smaps_rollup_fd();
        // /proc/self/mem is the process's OWN address space. Unintercepted it was the host open, i.e. the
        // ENGINE's address space: a guest could pread the engine's text and pwrite it back (pwrite there
        // bypasses page protection), which is an escape, not a leak. The guest's memory is the engine's
        // memory at a different address, so there is no correct pass-through -- deny it. EACCES is what a
        // reader without PTRACE_MODE_ATTACH already gets, so callers have the path.
        if (!strcmp(leaf, "mem")) {
            errno = EACCES;
            return -1;
        }
        // /proc/self/syscall published the ENGINE's stack pointer and program counter -- its ASLR slide.
        // The guest is never mid-syscall when it reads its own, so the kernel's "running" form is right.
        if (!strcmp(leaf, "syscall"))
            n = snprintf(buf, sizeof buf, "running\n");
        else if (!strcmp(leaf, "mountstats"))
            // The host's whole mount table, device names included, came through here while mounts/mountinfo
            // were intercepted. Same view as those two, in mountstats' "device X mounted on Y" form.
            n = proc_mountstats_text(buf, sizeof buf);
        else if (!strcmp(leaf, "status"))
            n = proc_status_text(buf, sizeof buf);
        else if (!strcmp(leaf, "stat"))
            n = proc_stat_text(buf, sizeof buf);
        else if (!strcmp(leaf, "statm"))
            n = proc_statm_text(buf, sizeof buf);
        else if (!strcmp(leaf, "environ"))
            n = proc_environ_text(buf, sizeof buf);
        else if (!strcmp(leaf, "cmdline"))
            n = proc_cmdline_text(buf, sizeof buf);
        else if (!strcmp(leaf, "comm"))
            n = proc_comm_text(buf, sizeof buf);
        else if (!strcmp(leaf, "mountinfo"))
            n = proc_mountinfo_text(buf, sizeof buf);
        else if (!strcmp(leaf, "limits"))
            n = proc_limits_text(buf, sizeof buf); // rlimit table
        else if (!strcmp(leaf, "oom_score_adj"))
            n = snprintf(buf, sizeof buf, "%d\n", g_proc_oom_score_adj);
        else if (!strcmp(leaf, "oom_adj") || !strcmp(leaf, "oom_score"))
            n = snprintf(buf, sizeof buf, "0\n");
        else if (!strcmp(leaf, "loginuid"))
            n = snprintf(buf, sizeof buf, "4294967295\n"); // unset (pam)
        else if (!strcmp(leaf, "cgroup"))
            n = snprintf(buf, sizeof buf, "0::/\n"); // cgroup v2 unified; also reached as /proc/<ourpid>/cgroup
        else if (!strcmp(leaf, "io"))
            // Per-process IO accounting. Monitoring agents (cAdvisor, language runtimes) read it
            // opportunistically; hl tracks no real per-process byte counters, so present the canonical
            // key set with a deterministic baseline (structural fidelity, like memory.stat/cpu.stat).
            n = snprintf(buf, sizeof buf,
                         "rchar: 0\nwchar: 0\nsyscr: 0\nsyscw: 0\nread_bytes: 0\nwrite_bytes: 0\n"
                         "cancelled_write_bytes: 0\n");
        if (n >= 0) {
            char desc[64];
            snprintf(desc, sizeof desc, "self:%s", leaf);
            return proc_text_fd_tagged(buf, n, desc);
        }
    }
    return INT_MIN;
}

static int proc_open_peer_process(const char *rp) {
    char buf[8192];
    int n = -1;
    // A PEER container process: /proc/<otherpid>/{stat,status,cmdline,comm}. proc_self_leaf matched only
    // our own pid above, so a numeric pid reaching here is a peer -- synthesize from the registry (guest
    // comm/argv) + host process stats (live rss/cpu/state). This is what makes ps/top/htop show the whole
    // container.
    {
        int gp2;
        const char *fl = proc_any_leaf(rp, &gp2);
        if (fl && gp2 > 0) {
            int host;
            int is_oom_leaf = !strcmp(fl, "oom_score_adj") || !strcmp(fl, "oom_adj") || !strcmp(fl, "oom_score");
            if (proc_pid_member(gp2, &host) ||
                (is_oom_leaf && (host = (gp2 == 1 && g_init_hostpid) ? g_init_hostpid : gp2) > 0 &&
                 !(kill(host, 0) != 0 && errno == ESRCH))) {
                if (!strncmp(fl, "ns/", 3) && fl[3] && ns_clone_flag(fl + 3)) {
                    char desc[64];
                    snprintf(desc, sizeof desc, "namespace:%s", fl + 3);
                    return proc_text_fd_tagged("", 0, desc);
                }
                // Peer /proc/<pid>/fd: a listable dir of symlinks built from the peer descriptor snapshot, so
                // each entry readlinks to the fd's target. (Opening a peer fd link stays deferred -- needs
                // cross-process fd passing; see proc_fd_dir_pid_open.)
                if (!strcmp(fl, "fd")) return proc_fd_dir_pid_open(gp2, host);
                if (!strcmp(fl, "stat"))
                    n = proc_stat_pid_text(buf, sizeof buf, gp2, host);
                else if (!strcmp(fl, "status"))
                    n = proc_status_pid_text(buf, sizeof buf, gp2, host);
                else if (!strcmp(fl, "statm"))
                    n = proc_statm_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "maps"))
                    return proc_maps_pid_fd(gp2, host);
                else if (!strcmp(fl, "cmdline"))
                    n = proc_cmdline_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "comm"))
                    n = proc_comm_pid_text(buf, sizeof buf, host);
                else if (!strcmp(fl, "oom_score_adj") || !strcmp(fl, "oom_adj") || !strcmp(fl, "oom_score"))
                    n = snprintf(buf, sizeof buf, "0\n");
                else if (!strcmp(fl, "cgroup"))
                    // A container is ONE cgroup, so a peer's line is our own. Previously unserved, so it fell
                    // through to the host and published the engine's real cgroup path (a user@1000.service
                    // scope under a desktop session) as the guest's.
                    n = snprintf(buf, sizeof buf, "0::/\n");
                if (n >= 0) {
                    char desc[64];
                    snprintf(desc, sizeof desc, "pid:%d:%s", gp2, fl);
                    return proc_text_fd_tagged(buf, n, desc);
                }
            }
        }
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_system_metrics(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/proc/cpuinfo")) {
        int nc = container_online_cpus(); // docker --cpus cap (state.c), else all host cores
        // One block per online CPU, and container_online_cpus() caps at 64. The x86 block is 656 bytes today
        // and its flag list is derived, so bound it by that list's own 512-byte ceiling rather than by a
        // measurement: 1KB/CPU covers any model. (640 did not even cover today's block, and the shared 8KB
        // `buf` silently truncated cpuinfo to ~14 processors on a many-core host.) Each snprintf is still
        // clamped so a would-be overflow cannot inflate `cn` -- proc_text_fd writes exactly `cn` bytes.
        char cib[64 * 1024]; // per-call (proc_open is reentrant across guest threads); 64KB stack
        int cn = 0;
        for (int i = 0; i < nc; i++) {
            size_t rem = sizeof cib - (size_t)cn;
            int w = cpuinfo_block(cib + cn, rem, i, nc);
            if (w < 0 || (size_t)w >= rem) break; // truncated -> stop rather than over-report length
            cn += w;
        }
        return proc_text_fd(cib, cn);
    } else if (!strcmp(rp, "/proc/meminfo")) {
        // Real-ish figures: a cgroup memory.max caps MemTotal (used = the tracked anon charge); otherwise
        // report the host backend's memory snapshot so htop's memory meter reflects a believable,
        // non-zero footprint instead of "0K used".
        unsigned long long tot, fre, avail, cached;
        if (g_mem_max) {
            tot = g_mem_max / 1024;
            unsigned long long used = (unsigned long long)atomic_load(&g_mem_charged) / 1024;
            fre = tot > used ? tot - used : 0;
            avail = fre;
            cached = 0;
        } else {
            host_mem(&tot, &fre, &avail, &cached);
        }
        // Present the standard field set common procfs consumers read (Active/Inactive/Dirty/AnonPages/…);
        // omitting them disabled monitoring heuristics. Accounting figures hl does not track are zero.
        n = snprintf(buf, sizeof buf,
                     "MemTotal:    %11llu kB\nMemFree:     %11llu kB\n"
                     "MemAvailable:%11llu kB\nBuffers:               0 kB\nCached:      %11llu kB\n"
                     "SwapCached:            0 kB\nActive:                0 kB\nInactive:              0 kB\n"
                     "Active(anon):          0 kB\nInactive(anon):        0 kB\nActive(file):          0 kB\n"
                     "Inactive(file):        0 kB\nUnevictable:           0 kB\nMlocked:               0 kB\n"
                     "SwapTotal:             0 kB\nSwapFree:              0 kB\n"
                     "Dirty:                 0 kB\nWriteback:             0 kB\nAnonPages:             0 kB\n"
                     "Mapped:                0 kB\nShmem:                 0 kB\nKReclaimable:          0 kB\n"
                     "Slab:                  0 kB\nSReclaimable:          0 kB\nSUnreclaim:            0 kB\n"
                     "KernelStack:           0 kB\nPageTables:            0 kB\nWritebackTmp:          0 kB\n"
                     "CommitLimit: %11llu kB\nCommitted_AS:          0 kB\nVmallocTotal:   34359738367 kB\n"
                     "VmallocUsed:           0 kB\nVmallocChunk:          0 kB\n",
                     tot, fre, avail, cached, tot);
    } else if (!strcmp(rp, "/proc/stat")) {
        // Real host CPU jiffies -> the cpu line increments between reads, so htop/top meters move. The
        // aggregate `cpu` line and each per-core `cpuN` line come from the host system snapshot. The old code
        // split the aggregate EVENLY across cores (aggregate/ncpu), so every cpuN line was byte-identical
        // and htop/top showed every core meter moving in lockstep at the same %. Per-core real ticks make
        // the deltas differ, so a busy core reads hot while idle cores read cold -- exactly like Linux.
        unsigned long long t[4];
        host_cpu_ticks(t);
        int nc = container_online_cpus(); // docker --cpus cap (state.c), else all host cores
        n = snprintf(buf, sizeof buf, "cpu  %llu %llu %llu %llu 0 0 0 0 0 0\n", t[0], t[3], t[1], t[2]);
        hl_host_cpu_ticks cores[64];
        hl_host_system_info system_info;
        int have_cores = hl_host_system_read(&system_info, cores, sizeof cores / sizeof cores[0]);
        for (int i = 0; i < nc; i++) {
            unsigned long long u, ni, sy, id;
            if (have_cores && i < (int)system_info.reported_cores) {
                u = cores[i].user;
                ni = cores[i].nice;
                sy = cores[i].system;
                id = cores[i].idle;
            } else { // API failed, or --cpus capped ABOVE the host core count: fall back to the even split
                u = t[0] / (unsigned)nc;
                ni = t[3] / (unsigned)nc;
                sy = t[1] / (unsigned)nc;
                id = t[2] / (unsigned)nc;
            }
            n += snprintf(buf + n, sizeof buf - (size_t)n, "cpu%d %llu %llu %llu %llu 0 0 0 0 0 0\n", i, u, ni, sy, id);
        }
        // intr/ctxt are cumulative-since-boot counters; monitoring heuristics divide by the interval and
        // treat a flat 0 as a dead system. Derive a monotone nonzero from host jiffies so consumers see live
        // counters. `processes` is cumulative forks since boot (Linux), not the live registry count.
        unsigned long long jif = t[0] + t[1] + t[2] + t[3];
        n += snprintf(buf + n, sizeof buf - (size_t)n,
                      "intr %llu\nctxt %llu\nbtime %ld\nprocesses %llu\nprocs_running 1\nprocs_blocked 0\n",
                      jif * 137ull + 1, jif * 509ull + 1, host_btime(),
                      (unsigned long long)atomic_load(&g_forks_since_boot) + 256ull);
    } else if (!strcmp(rp, "/proc/mounts") || !strcmp(rp, "/proc/self/mounts")) {
        // The fstab-style mount table (mirror of mountinfo). Name the root mount "overlay", not the legacy
        // "rootfs": busybox/util-linux df filters out a pseudo "rootfs" entry, leaving df unable to find the
        // mount for "/". The pseudo-filesystems are listed too so a reader enumerating mounts sees them.
        // Mirror of proc_mountinfo_text in fstab form (6 fields). Same set of pseudo-mounts docker lists so a
        // reader enumerating mounts (df/mount/findmnt) sees /dev/shm, /dev/pts, /dev/mqueue and the cgroup2
        // hierarchy. sysfs is ro (runc binds it ro); the /dev tmpfs carries its size/mode; /dev/shm is a
        // separate tmpfs with src "shm". Verified field-for-field vs the docker (runc) oracle.
        n = snprintf(buf, sizeof buf,
                     "overlay / overlay rw,relatime 0 0\n"
                     "proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0\n"
                     "tmpfs /dev tmpfs rw,nosuid,size=65536k,mode=755 0 0\n"
                     "devpts /dev/pts devpts rw,nosuid,noexec,relatime,gid=5,mode=620,ptmxmode=666 0 0\n"
                     "sysfs /sys sysfs ro,nosuid,nodev,noexec,relatime 0 0\n"
                     "cgroup /sys/fs/cgroup cgroup2 ro,nosuid,nodev,noexec,relatime,nsdelegate 0 0\n"
                     "mqueue /dev/mqueue mqueue rw,nosuid,nodev,noexec,relatime 0 0\n"
                     "shm /dev/shm tmpfs rw,nosuid,nodev,noexec,relatime,size=65536k 0 0\n");
        if (n > 0 && (size_t)n < sizeof buf) n = (int)mount_binds_append(buf, sizeof buf, (size_t)n, 1);
    } else if (!strcmp(rp, "/proc/uptime")) {
        unsigned long long t[4];
        host_cpu_ticks(t);
        long hz = sysconf(_SC_CLK_TCK);
        if (hz <= 0) hz = 100;
        double up = (double)(time(NULL) - host_btime());
        n = snprintf(buf, sizeof buf, "%.2f %.2f\n", up > 0 ? up : 0.0, (double)t[2] / (double)hz);
    } else if (!strcmp(rp, "/proc/loadavg")) {
        double la[3] = {0, 0, 0};
        getloadavg(la, 3);
        n = snprintf(buf, sizeof buf, "%.2f %.2f %.2f 1/%d %d\n", la[0], la[1], la[2], proc_reg_count(),
                     container_pid());
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_system_identity(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/proc/sys/vm/overcommit_memory")) {
        // OrbStack/docker default is 1 (heuristic-off, "always overcommit"). redis-server prints
        // "WARNING overcommit_memory is set to 0! Background save may fail..." when it reads anything but 1,
        // so serving 0 made hl emit a startup warning a real-docker user never sees. Match the oracle: 1.
        n = snprintf(buf, sizeof buf, "1\n");
    } else if (!strcmp(rp, "/proc/sys/kernel/hostname")) {
        // UTS ns (hostname cmd reads this)
        n = snprintf(buf, sizeof buf, "%s\n", g_hostname[0] ? g_hostname : "jit");
    } else if (!strcmp(rp, "/proc/sys/kernel/random/boot_id")) {
        // stable per-boot UUID (systemd/dbus/libuuid/curl/journald read it; without it tools print
        // "cannot find current boot id"). Deterministic from the container key -> same for every peer.
        n = proc_boot_id(buf, sizeof buf);
    } else if (!strcmp(rp, "/proc/sys/kernel/random/uuid")) {
        // Linux yields a FRESH type-4 UUID on every read of this file -- glibc/libuuid use it as a
        // uuid_generate_random source, so it must differ each open.
        uint8_t b[16];
        arc4random_buf(b, sizeof b);
        n = uuid_fmt(buf, sizeof buf, b);
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_system_version(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/proc/sys/kernel/random/entropy_avail")) {
        n = snprintf(buf, sizeof buf, "256\n"); // pool always "full" (host arc4random backs /dev/*random)
    } else if (!strcmp(rp, "/proc/sys/kernel/ostype")) {
        n = snprintf(buf, sizeof buf, "Linux\n");
    } else if (!strcmp(rp, "/proc/sys/kernel/osrelease")) {
        n = snprintf(buf, sizeof buf, "6.1.0\n");
    } else if (!strcmp(rp, "/proc/sys/kernel/version")) {
        n = snprintf(buf, sizeof buf, "#1 SMP hl-engine\n");
    } else if (!strcmp(rp, "/proc/self/cgroup")) {
        // cgroup v2 unified
        n = snprintf(buf, sizeof buf, "0::/\n");
    } else if (!strcmp(rp, "/proc/version")) {
        // The version banner embeds the build ISA; x86_64 guests see `uname -m`=x86_64, so /proc/version
        // must agree (a mismatched aarch64 token here confuses platform probes and diagnostics).
        n = snprintf(buf, sizeof buf, "Linux version 6.1.0 (hl-engine) %s\n", guest_is_x86() ? "x86_64" : "aarch64");
        // ---- container network introspection: lo + eth0 (see netif_* in state.c) --------------
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_network_protocols(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/proc/net/dev")) {
        // per-interface counters; zeros are fine (hl runs no real stack -- this is introspection only).
        // --network none: loopback-only, so eth0 is omitted (only the lo counters line).
        n = snprintf(buf, sizeof buf,
                     "Inter-|   Receive                                                |  Transmit\n"
                     " face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets "
                     "errs drop fifo colls carrier compressed\n"
                     "    lo: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n%s",
                     net_isolate() ? "" : "  eth0: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n");
    } else if (!strcmp(rp, "/proc/net/route")) {
        // Destination/Gateway/Mask are %08X of the network-order addr (netif_* already store that form).
        // --network none: no eth0 routes -> just the header (loopback carries no routing table entries).
        if (net_isolate()) {
            n = snprintf(buf, sizeof buf,
                         "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n");
        } else {
            uint32_t net = netif_eth0_net(), gw = netif_eth0_gw();
            int pfx = netif_eth0_prefix();
            uint32_t mask = pfx >= 32 ? 0xffffffffu : ((1u << pfx) - 1u);
            n = snprintf(buf, sizeof buf,
                         "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n"
                         "eth0\t00000000\t%08X\t0003\t0\t0\t0\t00000000\t0\t0\t0\n"
                         "eth0\t%08X\t00000000\t0001\t0\t0\t0\t%08X\t0\t0\t0\n",
                         gw, net, mask);
        }
    } else if (!strcmp(rp, "/proc/net/if_inet6")) {
        // addr(32 hex) ifindex(hex) prefix(hex) scope(hex) flags(hex) devname -- lo ::1 only.
        n = snprintf(buf, sizeof buf, "00000000000000000000000000000001 01 80 10 80        lo\n");
    } else if (!strcmp(rp, "/proc/net/tcp")) {
        // v4 table: header + a LISTEN row per socket the guest bind()+listen()ed (ss/netstat -l depend on it).
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  "
                     "timeout inode\n");
        n += netns_tcp_emit(buf + n, sizeof buf - n, 0);
    } else if (!strcmp(rp, "/proc/net/tcp6")) {
        // tcp6 has a DISTINCT header from tcp4: the v6 address columns are 32 hex wide and the second column
        // is "remote_address" (not "rem_address"). Reusing the v4 header here was a engine-specific divergence.
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address                         remote_address                        st "
                     "tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n");
        n += netns_tcp_emit(buf + n, sizeof buf - n, 1);
    } else if (!strcmp(rp, "/proc/net/udp")) {
        n = snprintf(buf, sizeof buf,
                     "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  "
                     "timeout inode ref pointer drops\n");
    } else if (!strcmp(rp, "/proc/net/udp6")) {
        n = snprintf(buf, sizeof buf,
                     "  sl  local_address                         remote_address                        st "
                     "tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n");
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_network_device_text(const char *file, int islo, char *buf, size_t cap) {
    if (!strcmp(file, "address")) {
        if (islo) return snprintf(buf, cap, "00:00:00:00:00:00\n");
        uint8_t mac[6];
        netif_eth0_mac(mac);
        return snprintf(buf, cap, "%02x:%02x:%02x:%02x:%02x:%02x\n", mac[0], mac[1], mac[2], mac[3], mac[4],
                        mac[5]);
    }
    if (!strcmp(file, "addr_len")) return snprintf(buf, cap, "6\n");
    if (!strcmp(file, "broadcast"))
        return snprintf(buf, cap, islo ? "00:00:00:00:00:00\n" : "ff:ff:ff:ff:ff:ff\n");
    if (!strcmp(file, "flags")) return snprintf(buf, cap, islo ? "0x9\n" : "0x1003\n");
    if (!strcmp(file, "mtu")) return snprintf(buf, cap, islo ? "65536\n" : "1500\n");
    if (!strcmp(file, "operstate")) return snprintf(buf, cap, islo ? "unknown\n" : "up\n");
    if (!strcmp(file, "type")) return snprintf(buf, cap, islo ? "772\n" : "1\n");
    if (!strcmp(file, "carrier")) return snprintf(buf, cap, "1\n");
    if (!strcmp(file, "ifindex") || !strcmp(file, "iflink")) return snprintf(buf, cap, islo ? "1\n" : "2\n");
    if (!strcmp(file, "tx_queue_len")) return snprintf(buf, cap, islo ? "0\n" : "1000\n");
    if (!strcmp(file, "speed")) return snprintf(buf, cap, "-1\n");
    if (!strcmp(file, "duplex")) return snprintf(buf, cap, "unknown\n");
    if (!strcmp(file, "carrier_changes") || (!strncmp(file, "statistics/", 11) && file[11]))
        return snprintf(buf, cap, "0\n");
    return -1;
}

static int proc_open_network_device(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strncmp(rp, "/sys/class/net/", 15)) {
        // per-interface attribute files tools stat/read (address, flags, mtu, operstate, type, ...).
        const char *rest = rp + 15;
        // --network none: eth0 does not exist, so its attribute files must not be served through the
        // direct (non-readdir) read path either -- otherwise a tool that reads /sys/class/net/eth0/address
        // sees an interface that readdir hid.
        int islo = !strncmp(rest, "lo/", 3), iseth = !net_isolate() && !strncmp(rest, "eth0/", 5);
        const char *file = islo ? rest + 3 : iseth ? rest + 5 : NULL;
        if (file) n = proc_network_device_text(file, islo, buf, sizeof buf);
        // cgroup v2: memory limit
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_cgroup_limits(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/sys/fs/cgroup/memory.max")) {
        if (g_mem_max)
            n = snprintf(buf, sizeof buf, "%llu\n", (unsigned long long)g_mem_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.current")) {
        n = snprintf(buf, sizeof buf, "%llu\n", cgroup_mem_current()); // container-wide (all engine procs)
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.max")) {
        if (g_pids_max)
            n = snprintf(buf, sizeof buf, "%d\n", g_pids_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_cgroup_capacity(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/sys/fs/cgroup/pids.current")) {
        n = snprintf(buf, sizeof buf, "%d\n", acct_pids_total()); // container-wide task count (all engine procs)
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.peak")) {
        n = snprintf(buf, sizeof buf, "%d\n", acct_pids_total()); // no historical peak tracked -> live
    } else if (!strcmp(rp, "/sys/fs/cgroup/pids.events") || !strcmp(rp, "/sys/fs/cgroup/pids.events.local")) {
        n = snprintf(buf, sizeof buf, "max 0\n"); // pids limit never hit (structural)
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpuset.cpus.effective") || !strcmp(rp, "/sys/fs/cgroup/cpuset.cpus")) {
        // The CPUs this cgroup may run on. cpuset.cpus.effective is what cpuset-aware runtimes read; advertise
        // the container's online set so a cpuset walk sees a populated range (was ENOENT -> walk failed).
        int nc = container_online_cpus();
        if (nc < 1) nc = 1;
        n = (nc == 1) ? snprintf(buf, sizeof buf, "0\n") : snprintf(buf, sizeof buf, "0-%d\n", nc - 1);
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpuset.mems.effective") || !strcmp(rp, "/sys/fs/cgroup/cpuset.mems")) {
        n = snprintf(buf, sizeof buf, "0\n"); // single (emulated) memory node
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_cgroup_local(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/sys/fs/cgroup/cpu.stat.local")) {
        n = snprintf(buf, sizeof buf, "throttled_usec 0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.oom.group")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.events")) {
        n = snprintf(buf, sizeof buf, "high 0\nmax 0\nfail 0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.peak")) {
        n = snprintf(buf, sizeof buf, "0\n");
        // ---- cgroup v2 unified-hierarchy surface real runtimes SIZE THEMSELVES from ----------------------
        // The JVM (-XX:+UseContainerSupport), the Go runtime (GOMAXPROCS/GOMEMLIMIT tooling), Node/libuv, and
        // systemd read these to pick heap size, GC/CommonPool/worker thread counts, and to detect that they are
        // in a v2 container at all. Values MUST reflect the docker --cpus/--memory caps (state.c g_cpu_max /
        // g_mem_max); unconstrained -> the kernel "max" sentinels. Verified byte-identical to runc (OrbStack
        // Docker 29.4) both unconstrained and under --memory=512m --cpus=2. Host-variant accounting figures
        // (memory.stat/cpu.stat live counters) are structural-only: the KEYS a runtime parses must be present,
        // the values are informational so we report zeros (a bare-guest deterministic baseline).
        // ---- cgroup core interface files (v2 markers a runtime detects the unified hierarchy by) ----------
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.controllers")) {
        // The controllers available in this cgroup. runc enables exactly these for a container leaf.
        n = snprintf(buf, sizeof buf, "cpuset cpu io memory pids\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.subtree_control")) {
        n = 0;
        buf[0] = 0; // a leaf cgroup delegates nothing downward -> empty (matches runc)
    }
    if (n < 0 && !strcmp(rp, "/sys/fs/cgroup/cgroup.type")) {
        n = snprintf(buf, sizeof buf, "domain\n");
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_cgroup_membership(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/sys/fs/cgroup/cgroup.procs")) {
        // The processes in this cgroup. The container is ONE cgroup, so this is EVERY guest process (init +
        // every forked child), enumerated from the cross-process registry -- not just container_pid().
        n = cgroup_procs_text(buf, sizeof buf, 0);
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.threads")) {
        // Every task (thread) in the cgroup: the per-process registry set plus THIS process's extra threads.
        n = cgroup_procs_text(buf, sizeof buf, 1);
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.events")) {
        n = snprintf(buf, sizeof buf, "populated 1\nfrozen 0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.max.depth") || !strcmp(rp, "/sys/fs/cgroup/cgroup.max.descendants")) {
        n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cgroup.stat")) {
        n = snprintf(buf, sizeof buf, "nr_descendants 0\nnr_dying_descendants 0\n");
        // ---- memory controller: JVM UseContainerSupport + GOMEMLIMIT tooling read memory.max/.high/.swap ---
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.min") || !strcmp(rp, "/sys/fs/cgroup/memory.low")) {
        n = snprintf(buf, sizeof buf, "0\n"); // no reclaim protection reserved (runc default)
    }
    if (n < 0 && !strcmp(rp, "/sys/fs/cgroup/memory.high")) {
        n = snprintf(buf, sizeof buf, "max\n"); // docker sets only the hard limit (memory.max), never .high
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_cgroup_memory(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.max")) {
        // v2 memory.swap.max is the SWAP-ONLY ceiling. Docker's default --memory-swap (unset) = 2*--memory,
        // and runc writes swap.max = memoryswap - memory = --memory. So under --memory it equals g_mem_max;
        // unconstrained -> "max". (Verified: --memory=512m -> 536870912, matching --memory bytes.)
        if (g_mem_max)
            n = snprintf(buf, sizeof buf, "%llu\n", (unsigned long long)g_mem_max);
        else
            n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.current")) {
        n = snprintf(buf, sizeof buf, "0\n"); // no swap accounted (hl runs no swap)
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.swap.high")) {
        n = snprintf(buf, sizeof buf, "max\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.peak")) {
        n = snprintf(buf, sizeof buf, "%llu\n", cgroup_mem_current()); // container-wide (no historical peak)
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.stat")) {
        // The per-type breakdown. The JVM's CgroupSubsystemController reads this for "file" (page cache) to
        // refine its container-memory estimate; the exact byte figures are host-variant, so we present the
        // full canonical key set with the tracked anon charge and zeros elsewhere (structural fidelity).
        unsigned long long anon = (unsigned long long)atomic_load(&g_mem_charged);
        n = snprintf(buf, sizeof buf,
                     "anon %llu\nfile 0\nkernel %llu\nkernel_stack 0\npagetables 0\nsec_pagetables 0\n"
                     "percpu 0\nsock 0\nvmalloc 0\nshmem 0\nfile_mapped 0\nfile_dirty 0\nfile_writeback 0\n"
                     "swapcached 0\nanon_thp 0\nfile_thp 0\nshmem_thp 0\ninactive_anon %llu\nactive_anon 0\n"
                     "inactive_file 0\nactive_file 0\nunevictable 0\nslab_reclaimable 0\nslab_unreclaimable 0\n"
                     "slab 0\nworkingset_refault_anon 0\nworkingset_refault_file 0\npgfault 0\npgmajfault 0\n",
                     anon, anon, anon);
    } else if (!strcmp(rp, "/sys/fs/cgroup/memory.events") || !strcmp(rp, "/sys/fs/cgroup/memory.events.local")) {
        n = snprintf(buf, sizeof buf, "low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\noom_group_kill 0\n");
        // ---- cpu controller: JVM ActiveProcessorCount + Go GOMAXPROCS derive from cpu.max quota/period ------
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_cgroup_cpu(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/sys/fs/cgroup/cpu.max")) {
        // "<quota> <period>" under --cpus, "max <period>" unconstrained. Docker's period is 100000us; the
        // quota is --cpus * period. g_cpu_max is the container's integer core allotment (state.c). A runtime
        // computes cpus = quota/period, so this is what makes a --cpus=2 container self-size Go GOMAXPROCS /
        // JVM availableProcessors to 2. (Verified: --cpus=2 -> "200000 100000".)
        if (g_cpu_max > 0)
            n = snprintf(buf, sizeof buf, "%lld 100000\n", (long long)g_cpu_max * 100000);
        else
            n = snprintf(buf, sizeof buf, "max 100000\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.max.burst")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.weight")) {
        n = snprintf(buf, sizeof buf, "100\n"); // docker default share weight (no --cpu-shares override)
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.weight.nice")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.idle")) {
        n = snprintf(buf, sizeof buf, "0\n");
    } else if (!strcmp(rp, "/sys/fs/cgroup/cpu.stat")) {
        // usage/throttling counters. The KEY NAMES are what a runtime/systemd parse; the values are
        // host-variant accounting, so zeros are a correct deterministic baseline (hl tracks no per-cgroup
        // cpu accounting). nr_throttled/throttled_usec present so a throttle-aware scheduler sees "0".
        n = snprintf(buf, sizeof buf,
                     "usage_usec 0\nuser_usec 0\nsystem_usec 0\nnr_periods 0\nnr_throttled 0\n"
                     "throttled_usec 0\nnr_bursts 0\nburst_usec 0\n");
        // ---- io controller (lower value; present so a full-cgroup walk finds it) --------------------------
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_cgroup_io(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/sys/fs/cgroup/io.max")) {
        n = 0;
        buf[0] = 0; // no per-device io limits set (docker without --device-*-bps) -> empty
    } else if (!strcmp(rp, "/sys/fs/cgroup/io.stat")) {
        n = 0;
        buf[0] = 0; // no real block device backs the overlay -> empty (host-variant otherwise)
    } else if (!strcmp(rp, "/sys/fs/cgroup/io.weight")) {
        n = snprintf(buf, sizeof buf, "default 100\n");
        // ---- the broad /proc + /proc/sys surface real software reads --------------------------------
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_system_tables(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/proc/cmdline")) {
        n = snprintf(buf, sizeof buf, "root=/dev/sda1 ro quiet\n"); // kernel cmdline (distinct from self/cmdline)
    } else if (!strcmp(rp, "/proc/filesystems")) {
        n = snprintf(buf, sizeof buf,
                     "nodev\tsysfs\nnodev\ttmpfs\nnodev\tproc\nnodev\tdevtmpfs\nnodev\tdevpts\n"
                     "nodev\tmqueue\nnodev\tcgroup2\nnodev\toverlay\n\text3\n\text2\n\text4\n");
    } else if (!strcmp(rp, "/proc/cgroups")) {
        // The v1 subsystem summary. On a pure-v2 (unified) host every controller lives in hierarchy 0; some
        // older runtimes (and `lscgroup`) read this to enumerate available controllers. Mirror the OrbStack
        // oracle: all subsystems enabled, hierarchy 0 (v2 unified), num_cgroups is host-variant -> report 1.
        n = snprintf(buf, sizeof buf,
                     "#subsys_name\thierarchy\tnum_cgroups\tenabled\n"
                     "cpuset\t0\t1\t1\ncpu\t0\t1\t1\ncpuacct\t0\t1\t1\nblkio\t0\t1\t1\nmemory\t0\t1\t1\n"
                     "devices\t0\t1\t1\nfreezer\t0\t1\t1\nnet_cls\t0\t1\t1\nperf_event\t0\t1\t1\n"
                     "net_prio\t0\t1\t1\npids\t0\t1\t1\n");
    } else if (!strcmp(rp, "/proc/swaps")) {
        n = snprintf(buf, sizeof buf, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n"); // no swap
    } else if (!strcmp(rp, "/proc/modules")) {
        n = 0;
        buf[0] = 0; // no loadable modules
    } else if (!strcmp(rp, "/proc/devices")) {
        // The block-device section must list standard majors (loop/sd/device-mapper/blkext) or installers
        // and device-major discovery see a false empty device surface.
        n = snprintf(buf, sizeof buf,
                     "Character devices:\n  1 mem\n  5 /dev/tty\n  5 /dev/console\n  5 /dev/ptmx\n"
                     "136 pts\n\nBlock devices:\n  7 loop\n  8 sd\n 253 device-mapper\n 259 blkext\n");
    } else if (!strcmp(rp, "/proc/tty/drivers")) {
        // tty driver table (`/proc/tty/drivers`) tty-discovery tools read; the exact rows are host-variant,
        // so present the canonical container set (pty pair + console/serial) so the file is non-empty.
        n = snprintf(buf, sizeof buf,
                     "/dev/tty             /dev/tty        5       0 system:/dev/tty\n"
                     "/dev/console         /dev/console    5       1 system:console\n"
                     "/dev/ptmx            /dev/ptmx       5       2 system\n"
                     "unknown              /dev/tty        4    1-63 console\n"
                     "pty_slave            /dev/pts      136 0-1048575 pty:slave\n"
                     "pty_master           /dev/ptm      128 0-1048575 pty:master\n");
    } else if (!strcmp(rp, "/proc/vmstat")) {
        n = snprintf(buf, sizeof buf,
                     "nr_free_pages 262144\nnr_zone_inactive_anon 0\nnr_zone_active_anon 0\n"
                     "nr_dirty 0\nnr_writeback 0\nnr_slab_reclaimable 0\nnr_slab_unreclaimable 0\n"
                     "pgpgin 0\npgpgout 0\npswpin 0\npswpout 0\npgfault 0\npgmajfault 0\n");
    } else if (!strcmp(rp, "/proc/net/sockstat")) {
        // Socket accounting (`ss -s`, monitoring agents). hl runs no real IP stack, so the counters are a
        // deterministic zero baseline -- but the SECTIONS must exist with the exact kernel key names.
        n = snprintf(buf, sizeof buf,
                     "sockets: used 1\nTCP: inuse 0 orphan 0 tw 0 alloc 0 mem 0\n"
                     "UDP: inuse 0 mem 0\nUDPLITE: inuse 0\nRAW: inuse 0\n"
                     "FRAG: inuse 0 memory 0\n");
    } else if (!strcmp(rp, "/proc/net/sockstat6")) {
        n = snprintf(buf, sizeof buf,
                     "TCP6: inuse 0\nUDP6: inuse 0\nUDPLITE6: inuse 0\nRAW6: inuse 0\nFRAG6: inuse 0 memory 0\n");
    } else if (!strcmp(rp, "/proc/net/unix")) {
        n = snprintf(buf, sizeof buf, "Num       RefCount Protocol Flags    Type St Inode Path\n");
        // One row per live guest-bound AF_UNIX socket (socket-inventory tools read this). Columns match the
        // kernel: a bound listener is Flags 00010000, St 01 (LISTEN); the inode is a stable synthetic id.
        for (int fd = 0; fd < HL_NFD && n < (int)sizeof buf - 128; fd++) {
            if (!g_unix_bind[fd][0]) continue;
            if (fcntl(fd, F_GETFD) == -1) {
                g_unix_bind[fd][0] = 0;
                continue;
            } // closed -> drop
            n += snprintf(buf + n, sizeof buf - (size_t)n, "%016x: %08x %08x %08x %04x %02x %5d %s\n", fd, 2u, 0u,
                          0x10000u, 1u, 1u, 100000 + fd, g_unix_bind[fd]);
        }
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_network_tables(const char *rp) {
    char buf[8192];
    int n = -1;
    if (!strcmp(rp, "/proc/net/snmp")) {
        // The full protocol-counter table `netstat -s` / `ss -s` parse: paired header+value lines for
        // Ip/Icmp/IcmpMsg/Tcp/Udp/UdpLite. hl runs no real IP stack, so the counters are zero -- but the
        // SECTIONS must exist with the exact kernel column names or the parser aborts. Tcp's RtoAlgorithm/
        // RtoMin/RtoMax/MaxConn carry the conventional 1/200/120000/-1 the kernel reports.
        n = snprintf(
            buf, sizeof buf,
            "Ip: Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos "
            "InDiscards InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs "
            "ReasmFails FragOKs FragFails FragCreates\n"
            "Ip: 2 64 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
            "Icmp: InMsgs InErrors InCsumErrors InDestUnreachs InTimeExcds InParmProbs InSrcQuenchs "
            "InRedirects InEchos InEchoReps InTimestamps InTimestampReps InAddrMasks InAddrMaskReps OutMsgs "
            "OutErrors OutDestUnreachs OutTimeExcds OutParmProbs OutSrcQuenchs OutRedirects OutEchos "
            "OutEchoReps OutTimestamps OutTimestampReps OutAddrMasks OutAddrMaskReps\n"
            "Icmp: 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n"
            "IcmpMsg: InType3 OutType3\nIcmpMsg: 0 0\n"
            "Tcp: RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets "
            "CurrEstab InSegs OutSegs RetransSegs InErrs OutRsts InCsumErrors\n"
            "Tcp: 1 200 120000 -1 0 0 0 0 0 0 0 0 0 0 0\n"
            "Udp: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti "
            "MemErrors\n"
            "Udp: 0 0 0 0 0 0 0 0 0\n"
            "UdpLite: InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors "
            "IgnoredMulti MemErrors\n"
            "UdpLite: 0 0 0 0 0 0 0 0 0\n");
    } else if (!strcmp(rp, "/proc/net/netstat")) {
        // `netstat -s` / `ss -s` parse the TcpExt + IpExt extended-counter tables. hl runs no IP stack, so
        // every counter is zero -- but the SECTIONS with the exact kernel column names must exist (a missing
        // file makes those stats silently vanish). The zero value-line is generated with exactly as many
        // fields as its header (one " 0" per space) so a positional parser stays aligned.
        static const char *const th =
            "TcpExt: SyncookiesSent SyncookiesRecv SyncookiesFailed EmbryonicRsts PruneCalled RcvPruned "
            "OfoPruned OutOfWindowIcmps LockDroppedIcmps ArpFilter TW TWRecycled TWKilled PAWSActive "
            "PAWSEstab BeyondWindow TSEcrRejected PAWSOldAck PAWSTimewait DelayedACKs DelayedACKLocked "
            "DelayedACKLost ListenOverflows ListenDrops TCPHPHits TCPPureAcks TCPHPAcks TCPRenoRecovery "
            "TCPSackRecovery TCPSACKReneging TCPSACKReorder TCPRenoReorder TCPTSReorder TCPFullUndo "
            "TCPPartialUndo TCPDSACKUndo TCPLossUndo TCPLostRetransmit TCPRenoFailures TCPSackFailures "
            "TCPLossFailures TCPFastRetrans TCPSlowStartRetrans TCPTimeouts TCPLossProbes "
            "TCPLossProbeRecovery TCPRenoRecoveryFail TCPSackRecoveryFail TCPRcvCollapsed TCPBacklogCoalesce "
            "TCPDSACKOldSent TCPDSACKOfoSent TCPDSACKRecv TCPDSACKOfoRecv TCPAbortOnData TCPAbortOnClose "
            "TCPAbortOnMemory TCPAbortOnTimeout TCPAbortOnLinger TCPAbortFailed TCPMemoryPressures "
            "TCPMemoryPressuresChrono TCPSACKDiscard TCPDSACKIgnoredOld TCPDSACKIgnoredNoUndo TCPSpuriousRTOs "
            "TCPMD5NotFound TCPMD5Unexpected TCPMD5Failure TCPSackShifted TCPSackMerged TCPSackShiftFallback "
            "TCPBacklogDrop PFMemallocDrop TCPMinTTLDrop TCPDeferAcceptDrop IPReversePathFilter "
            "TCPTimeWaitOverflow TCPReqQFullDoCookies TCPReqQFullDrop TCPRetransFail TCPRcvCoalesce "
            "TCPOFOQueue TCPOFODrop TCPOFOMerge TCPChallengeACK TCPSYNChallenge TCPFastOpenActive "
            "TCPFastOpenActiveFail TCPFastOpenPassive TCPFastOpenPassiveFail TCPFastOpenListenOverflow "
            "TCPFastOpenCookieReqd TCPFastOpenBlackhole TCPSpuriousRtxHostQueues BusyPollRxPackets "
            "TCPAutoCorking TCPFromZeroWindowAdv TCPToZeroWindowAdv TCPWantZeroWindowAdv TCPSynRetrans "
            "TCPOrigDataSent TCPHystartTrainDetect TCPHystartTrainCwnd TCPHystartDelayDetect "
            "TCPHystartDelayCwnd TCPACKSkippedSynRecv TCPACKSkippedPAWS TCPACKSkippedSeq TCPACKSkippedFinWait2 "
            "TCPACKSkippedTimeWait TCPACKSkippedChallenge TCPWinProbe TCPKeepAlive TCPMTUPFail TCPMTUPSuccess "
            "TCPDelivered TCPDeliveredCE TCPAckCompressed TCPZeroWindowDrop TCPRcvQDrop TCPWqueueTooBig "
            "TCPFastOpenPassiveAltKey TcpTimeoutRehash TcpDuplicateDataRehash TCPDSACKRecvSegs "
            "TCPDSACKIgnoredDubious TCPMigrateReqSuccess TCPMigrateReqFailure TCPPLBRehash TCPAORequired "
            "TCPAOBad TCPAOKeyNotFound TCPAOGood TCPAODroppedIcmps";
        static const char *const ih =
            "IpExt: InNoRoutes InTruncatedPkts InMcastPkts OutMcastPkts InBcastPkts OutBcastPkts InOctets "
            "OutOctets InMcastOctets OutMcastOctets InBcastOctets OutBcastOctets InCsumErrors InNoECTPkts "
            "InECT1Pkts InECT0Pkts InCEPkts ReasmOverlaps";
        n = 0;
        const char *hdrs[2] = {th, ih};
        const char *labs[2] = {"TcpExt:", "IpExt:"};
        for (int pass = 0; pass < 2; pass++) {
            int fields = 0;
            for (const char *p = hdrs[pass]; *p; p++)
                if (*p == ' ') fields++;
            n += snprintf(buf + n, sizeof buf - n, "%s\n%s", hdrs[pass], labs[pass]);
            for (int i = 0; i < fields && n < (int)sizeof buf - 4; i++)
                n += snprintf(buf + n, sizeof buf - n, " 0");
            n += snprintf(buf + n, sizeof buf - n, "\n");
        }
    } else if (!strcmp(rp, "/proc/net/ipv6_route")) {
        // `ip -6 route` / `netstat -6 -r` parse this. Loopback-only container v6 routing table (matches a
        // real --network bridge container that has no global v6): the ::/0-ish + ::1 host route on lo.
        n = snprintf(buf, sizeof buf,
                     "00000000000000000000000000000000 00 00000000000000000000000000000000 00 "
                     "00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo\n"
                     "00000000000000000000000000000001 80 00000000000000000000000000000000 00 "
                     "00000000000000000000000000000000 00000000 00000002 00000000 80200001       lo\n"
                     "00000000000000000000000000000000 00 00000000000000000000000000000000 00 "
                     "00000000000000000000000000000000 ffffffff 00000001 00000000 00200200       lo\n");
    } else if (!strcmp(rp, "/proc/net/snmp6")) {
        // IPv6 counter table `netstat -s` reads for its "Ip6/Icmp6/Udp6" sections. Zero counters (no real
        // stack); the KEY NAMES must match the kernel or the section is dropped.
        n = snprintf(buf, sizeof buf,
                     "Ip6InReceives                   \t0\nIp6InHdrErrors                  \t0\n"
                     "Ip6InTooBigErrors               \t0\nIp6InNoRoutes                   \t0\n"
                     "Ip6InAddrErrors                 \t0\nIp6InUnknownProtos              \t0\n"
                     "Ip6InTruncatedPkts              \t0\nIp6InDiscards                   \t0\n"
                     "Ip6InDelivers                   \t0\nIp6OutForwDatagrams             \t0\n"
                     "Ip6OutRequests                  \t0\nIp6OutDiscards                  \t0\n"
                     "Ip6OutNoRoutes                  \t0\nIp6ReasmTimeout                 \t0\n"
                     "Ip6ReasmReqds                   \t0\nIp6ReasmOKs                     \t0\n"
                     "Ip6ReasmFails                   \t0\nIp6FragOKs                      \t0\n"
                     "Ip6FragFails                    \t0\nIp6FragCreates                  \t0\n"
                     "Ip6InMcastPkts                  \t0\nIp6OutMcastPkts                 \t0\n"
                     "Ip6InOctets                     \t0\nIp6OutOctets                    \t0\n"
                     "Icmp6InMsgs                     \t0\nIcmp6InErrors                   \t0\n"
                     "Icmp6OutMsgs                    \t0\nIcmp6OutErrors                  \t0\n"
                     "Udp6InDatagrams                 \t0\nUdp6NoPorts                     \t0\n"
                     "Udp6InErrors                    \t0\nUdp6OutDatagrams                \t0\n"
                     "Udp6RcvbufErrors                \t0\nUdp6SndbufErrors                \t0\n"
                     "Udp6InCsumErrors                \t0\nUdp6IgnoredMulti                \t0\n"
                     "Udp6MemErrors                   \t0\n");
    } else if (!strcmp(rp, "/proc/net/arp")) {
        // Neighbour table (`arp -a`, `ip neigh`). The container is its own net namespace: it must NOT expose
        // the HOST's ARP cache (gateway/neighbour MACs) that the raw host /proc/net/arp passthrough leaked.
        // A freshly-started bridge container has resolved no neighbours yet, so the correct, container-safe
        // view is the header with an empty table -- well-formed for any parser.
        n = snprintf(buf, sizeof buf,
                     "IP address       HW type     Flags       HW address            Mask     Device\n");
    } else if (!strcmp(rp, "/proc/net/igmp")) {
        // Multicast group memberships per interface. Must reflect the SAME container interface set as
        // /proc/net/dev (lo [+ eth0]) -- the host passthrough leaked the host's docker0/host-iface rows,
        // an isolation break and an iface-set inconsistency vs the synthesized /proc/net/dev. Every up
        // multicast interface joins the all-hosts group 224.0.0.1 (010000E0, little-endian hex).
        n = snprintf(buf, sizeof buf,
                     "Idx\tDevice    : Count Querier\tGroup    Users Timer\tReporter\n"
                     "1\tlo        :     1      V3\n\t\t\t\t010000E0     1 0:00000000\t\t0\n");
        if (!net_isolate())
            n += snprintf(buf + n, sizeof buf - (size_t)n,
                          "2\teth0      :     1      V3\n\t\t\t\t010000E0     1 0:00000000\t\t0\n");
    } else if (!strncmp(rp, "/proc/net/", 10)) {
        // Isolation backstop: every /proc/net leaf the container legitimately exposes is synthesized above
        // (a container view). Any remaining /proc/net/<leaf> -- fib_trie, rt_cache, netlink, packet,
        // softnet_stat, protocols, dev_mcast, icmp, raw, xfrm_stat, ... -- would otherwise fall through to a
        // raw host open and leak the HOST network stack (host routes/subnets, host processes' sockets, host
        // CPU count, host-wide socket counts). Serve a well-formed EMPTY table instead of the host file: the
        // namespaced file exists (open succeeds) but carries no host data.
        n = 0;
        buf[0] = 0;
    } else if (!strcmp(rp, "/proc/pressure/cpu")) {
        n = snprintf(buf, sizeof buf, "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n");
    } else if (!strcmp(rp, "/proc/pressure/memory") || !strcmp(rp, "/proc/pressure/io")) {
        n = snprintf(buf, sizeof buf,
                     "some avg10=0.00 avg60=0.00 avg300=0.00 total=0\n"
                     "full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n");
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open_constants(const char *rp) {
    char buf[8192];
    int n = -1;
    {
        // Constant sysctl-style files (values mirror a modern Linux default). A single table keeps the
        // /proc/sys/{kernel,vm,net,fs} surface complete for the sysctl/config probes Go/JVM/nginx/redis/
        // postgres/systemd issue. Multi-value files use TAB separators exactly like the kernel.
        static const struct {
            const char *p, *v;
        } K[] = {
            // kernel
            {"/proc/sys/kernel/pid_max", "4194304\n"},
            {"/proc/sys/kernel/threads-max", "63488\n"},
            {"/proc/sys/kernel/cap_last_cap", "40\n"},
            {"/proc/sys/kernel/ngroups_max", "65536\n"},
            {"/proc/sys/kernel/tainted", "0\n"},
            {"/proc/sys/kernel/domainname", "(none)\n"},
            {"/proc/sys/kernel/overflowuid", "65534\n"},
            {"/proc/sys/kernel/overflowgid", "65534\n"},
            {"/proc/sys/kernel/core_pattern", "core\n"},
            {"/proc/sys/kernel/sched_child_runs_first", "0\n"},
            {"/proc/sys/kernel/shmmax", "18446744073692774399\n"},
            {"/proc/sys/kernel/shmall", "18446744073692774399\n"},
            {"/proc/sys/kernel/shmmni", "4096\n"},
            {"/proc/sys/kernel/sem", "256\t131072\t500\t512\n"},
            {"/proc/sys/kernel/msgmax", "8192\n"},
            {"/proc/sys/kernel/msgmnb", "16384\n"},
            {"/proc/sys/kernel/msgmni", "512\n"},
            {"/proc/sys/kernel/yama/ptrace_scope", "1\n"},
            {"/proc/sys/kernel/random/poolsize", "256\n"},
            {"/proc/sys/kernel/printk", "4\t4\t1\t7\n"},
            {"/proc/sys/kernel/panic", "10\n"}, // oracle: 10s reboot-on-panic (was 0)
            // ASLR posture. A guest/security probe (Go's runtime, glibc, hardening scanners) reads this to
            // learn whether the kernel randomizes mmap/stack/brk; hl omitted it -> ENOENT where real docker
            // serves 2 (full ASLR: mmap + stack + brk + VDSO). Oracle: 2.
            {"/proc/sys/kernel/randomize_va_space", "2\n"},
            // vm
            {"/proc/sys/vm/overcommit_ratio", "50\n"},
            {"/proc/sys/vm/overcommit_kbytes", "0\n"},
            // elasticsearch REFUSES to start if max_map_count < 262144. hl served 65530 -> ES bootstrap
            // check fails, a warning/refusal a real-docker user never sees. Oracle: 1048576.
            {"/proc/sys/vm/max_map_count", "1048576\n"},
            {"/proc/sys/vm/mmap_min_addr", "32768\n"}, // oracle (was 65536)
            {"/proc/sys/vm/swappiness", "20\n"},       // oracle (was 60)
            {"/proc/sys/vm/dirty_ratio", "20\n"},
            {"/proc/sys/vm/dirty_background_ratio", "10\n"},
            {"/proc/sys/vm/nr_hugepages", "0\n"},
            {"/proc/sys/vm/panic_on_oom", "0\n"},
            {"/proc/sys/vm/vfs_cache_pressure", "100\n"},
            // net.core
            {"/proc/sys/net/core/somaxconn", "4096\n"},
            {"/proc/sys/net/core/netdev_max_backlog", "1000\n"},
            {"/proc/sys/net/core/rmem_max", "7500000\n"},    // oracle (was 212992)
            {"/proc/sys/net/core/wmem_max", "7500000\n"},    // oracle (was 212992)
            {"/proc/sys/net/core/rmem_default", "229376\n"}, // oracle (was 212992)
            {"/proc/sys/net/core/wmem_default", "229376\n"}, // oracle (was 212992)
            {"/proc/sys/net/core/optmem_max", "131072\n"},   // oracle (was 20480)
            // net.ipv4
            {"/proc/sys/net/ipv4/ip_local_port_range", "32768\t60999\n"},
            {"/proc/sys/net/ipv4/ip_unprivileged_port_start", "0\n"}, // oracle (was 1024)
            {"/proc/sys/net/ipv4/ip_forward", "1\n"},                 // oracle (was 0)
            {"/proc/sys/net/ipv4/ip_nonlocal_bind", "0\n"},
            {"/proc/sys/net/ipv4/tcp_fin_timeout", "60\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_time", "7200\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_intvl", "75\n"},
            {"/proc/sys/net/ipv4/tcp_keepalive_probes", "9\n"},
            {"/proc/sys/net/ipv4/tcp_max_syn_backlog", "1024\n"}, // oracle (was 128)
            {"/proc/sys/net/ipv4/tcp_syncookies", "1\n"},
            {"/proc/sys/net/ipv4/tcp_tw_reuse", "2\n"},
            {"/proc/sys/net/ipv4/tcp_rmem", "4096\t131072\t33554432\n"}, // oracle max (was 6291456)
            {"/proc/sys/net/ipv4/tcp_wmem", "4096\t16384\t4194304\n"},
            {"/proc/sys/net/ipv4/tcp_congestion_control", "cubic\n"},
            {"/proc/sys/net/ipv4/tcp_available_congestion_control", "reno cubic\n"},
            // fs. On modern (cgroup-era) kernels the global file-max cap is effectively removed: the oracle
            // reports LONG_MAX for file-max and the file-nr high-water field. Serving 1048576 made programs
            // that size their fd budget off file-max under-provision vs a real-docker run.
            {"/proc/sys/fs/file-max", "9223372036854775807\n"},         // oracle LONG_MAX (was 1048576)
            {"/proc/sys/fs/nr_open", "2147483584\n"},                   // oracle (was 1048576)
            {"/proc/sys/fs/file-nr", "1024\t0\t9223372036854775807\n"}, // 3rd field == file-max (was 1048576)
            {"/proc/sys/fs/pipe-max-size", "1048576\n"},
            {"/proc/sys/fs/pipe-user-pages-hard", "0\n"},
            {"/proc/sys/fs/pipe-user-pages-soft", "16384\n"},
            {"/proc/sys/fs/aio-max-nr", "1048576\n"}, // oracle (was 65536)
            {"/proc/sys/fs/aio-nr", "0\n"},
            {"/proc/sys/fs/protected_hardlinks", "1\n"},
            {"/proc/sys/fs/protected_symlinks", "1\n"},
            {"/proc/sys/fs/suid_dumpable", "2\n"}, // oracle (was 0)
            {"/proc/sys/fs/inotify/max_user_watches", "524288\n"},
            // VS Code / node chokidar / systemd watchers exhaust these and print "ENOSPC: inotify watch
            // limit reached" when they are low. Oracle bumps both far above the old 128 / 16384.
            {"/proc/sys/fs/inotify/max_user_instances", "524288\n"}, // oracle (was 128)
            {"/proc/sys/fs/inotify/max_queued_events", "1048576\n"}, // oracle (was 16384)
            // POSIX message-queue limits (fs/mqueue/*) -- hl omitted these entirely, so a reader (glibc
            // mq_* tuning, systemd) got ENOENT where real docker serves a value. Oracle kernel defaults.
            {"/proc/sys/fs/mqueue/msg_max", "10\n"},
            {"/proc/sys/fs/mqueue/msgsize_max", "8192\n"},
            {"/proc/sys/fs/mqueue/queues_max", "256\n"},
            {"/proc/sys/fs/mqueue/msg_default", "10\n"},
            {"/proc/sys/fs/mqueue/msgsize_default", "8192\n"},
            // Transparent-hugepage policy. jemalloc/tcmalloc, the JVM (-XX:+UseTransparentHugePages), redis
            // (THP warning), and mongod all read this; hl omitted it -> ENOENT, where real docker exposes the
            // host's setting with the active mode bracketed. Oracle: "always [madvise] never".
            {"/sys/kernel/mm/transparent_hugepage/enabled", "always [madvise] never\n"},
        };

        for (size_t i = 0; i < sizeof K / sizeof *K; i++)
            if (!strcmp(rp, K[i].p)) {
                n = snprintf(buf, sizeof buf, "%s", K[i].v);
                break;
            }
    }
    if (n < 0) return INT_MIN;
    return proc_text_fd(buf, n);
}

static int proc_open(const char *requested_path) {
    char canonical_path[4200];
    if (proc_canonical_path(requested_path, canonical_path, sizeof canonical_path) < 0) return -2;
    const char *rp = canonical_path;
    int result = proc_open_self_process(rp);
    if (result != INT_MIN) return result;
    result = proc_open_peer_process(rp);
    if (result != INT_MIN) return result;
    result = proc_open_system_metrics(rp);
    if (result != INT_MIN) return result;
    result = proc_open_system_identity(rp);
    if (result != INT_MIN) return result;
    result = proc_open_system_version(rp);
    if (result != INT_MIN) return result;
    result = proc_open_network_protocols(rp);
    if (result != INT_MIN) return result;
    result = proc_open_network_device(rp);
    if (result != INT_MIN) return result;
    result = proc_open_cgroup_limits(rp);
    if (result != INT_MIN) return result;
    result = proc_open_cgroup_capacity(rp);
    if (result != INT_MIN) return result;
    result = proc_open_cgroup_local(rp);
    if (result != INT_MIN) return result;
    result = proc_open_cgroup_membership(rp);
    if (result != INT_MIN) return result;
    result = proc_open_cgroup_memory(rp);
    if (result != INT_MIN) return result;
    result = proc_open_cgroup_cpu(rp);
    if (result != INT_MIN) return result;
    result = proc_open_cgroup_io(rp);
    if (result != INT_MIN) return result;
    result = proc_open_system_tables(rp);
    if (result != INT_MIN) return result;
    result = proc_open_network_tables(rp);
    if (result != INT_MIN) return result;
    result = proc_open_constants(rp);
    return result == INT_MIN ? -2 : result;
}

// Linux-layout stat for a synthesized /proc or /sys file (so stat()/access() see it -- find, du,
// container runtimes that stat /etc/mtab -> /proc/mounts, JVM that stats cgroup files, etc.).
static void fill_linux_stat(uint8_t *d, const struct stat *s, const char *hostpath, int fd);

// The pseudo /dev nodes the rootfs lacks but open() (fs.c) backs with a real host device. Returns the
// host path open() would use, else NULL. stat()/access() consult this so the nodes report as EXISTING
// character devices -- e.g. libgcrypt detects its RNG via access("/dev/urandom",R_OK); an ENOENT there
// makes it abort ("no entropy gathering module detected"), which breaks gpgv and thus `apt-get update`.
// The container's controlling terminal. `docker run -t` makes the daemon call login_tty, which hands the
// guest fd 0/1/2 as ONE pty slave. On Linux/devpts that slave is /dev/pts/0, but hl's host pty is a mac
// /dev/ttysNNN (or a host /dev/pts/N) whose raw name would otherwise leak into the guest via
// F_GETPATH -- so `tty`, ttyname(3), the `ps` TTY column, and any program that reopens open(ttyname(0))
// would see a device that doesn't exist in the container. We present it uniformly as /dev/pts/0.
// ctty_anchor() returns the host fd that IS the controlling terminal (the first of 0/1/2 that is a tty),
// or -1 when stdio is piped (no tty) -- exactly matching real docker, where a non -t container has no tty.
