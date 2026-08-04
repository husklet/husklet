// One CPU model, every discovery surface. A guest learns what CPU it is on from three places -- the auxv
// the loader planted, /proc/cpuinfo, and the ISA's own feature instruction (CPUID / the aarch64 HWCAP
// contract) -- and they must be three renderings of ONE model, not three independent claims. Nothing gated
// that: the auxval fixture reads glibc's _dl_hwcap (which comes from CPUID, so it cannot see AT_HWCAP at
// all) and selfauxv only checks AT_PAGESZ/AT_PHENT, so AT_HWCAP and the auxv ids were unenforced and
// /proc/cpuinfo drifted (aarch64 advertised nine HWCAP features and printed two).
//
// So: read /proc/self/auxv DIRECTLY -- no getauxval, which answers from glibc's own cache -- and assert
//   x86-64: auxv[AT_HWCAP] == CPUID.1:EDX, and every flag in the table below is in /proc/cpuinfo's `flags`
//           iff its CPUID bit is set (so withholding MOVBE must drop `movbe` from both surfaces).
//   aarch64: /proc/cpuinfo's `Features` lists exactly the named AT_HWCAP/AT_HWCAP2 bits, both directions.
//   both:    AT_UID/AT_EUID/AT_GID/AT_EGID == getuid()/geteuid()/getgid()/getegid().
// Bits/tokens the table does not name are ignored, so a kernel newer than the table stays green natively
// while the engine -- which emits only named ones -- is checked in full.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <sys/types.h>
#include <unistd.h>
#include "pf.h"

#define AT_PAGESZ 6
#define AT_UID 11
#define AT_EUID 12
#define AT_GID 13
#define AT_EGID 14
#define AT_HWCAP 16
#define AT_HWCAP2 26

static unsigned long g_type[64], g_val[64];
static int g_naux;

static int auxv_load(void) {
    unsigned char raw[2048];
    int n = pf_read("/proc/self/auxv", (char *)raw, (int)sizeof raw);
    if (n <= 0) return 0;
    for (int o = 0; o + 16 <= n && g_naux < 64; o += 16) {
        unsigned long t, v;
        memcpy(&t, raw + o, sizeof t);
        memcpy(&v, raw + o + 8, sizeof v);
        if (!t) break; // AT_NULL
        g_type[g_naux] = t;
        g_val[g_naux] = v;
        g_naux++;
    }
    return g_naux > 0;
}

static int auxv_get(unsigned long type, unsigned long *out) {
    for (int i = 0; i < g_naux; i++)
        if (g_type[i] == type) {
            *out = g_val[i];
            return 1;
        }
    return 0;
}

// Whole-token search in a space-separated list (so "sse" never matches inside "ssse3", nor "aes" in "vaes").
static int token_in(const char *list, const char *tok) {
    size_t tl = strlen(tok);
    for (const char *p = list; *p;) {
        while (*p == ' ' || *p == '\t')
            p++;
        const char *e = p;
        while (*e && *e != ' ' && *e != '\t')
            e++;
        if ((size_t)(e - p) == tl && !memcmp(p, tok, tl)) return 1;
        p = e;
    }
    return 0;
}

int main(void) {
    char cpuinfo[16384];
    int cn = pf_read("/proc/cpuinfo", cpuinfo, sizeof cpuinfo);
    int have_auxv = auxv_load();

    unsigned long hwcap = 0, hwcap2 = 0, pagesz = 0;
    int have_hwcap = auxv_get(AT_HWCAP, &hwcap);
    auxv_get(AT_HWCAP2, &hwcap2);

    // The auxv ids are the process's own credentials, not zeros.
    unsigned long uid = 0, euid = 0, gid = 0, egid = 0;
    int ids_ok = auxv_get(AT_UID, &uid) && auxv_get(AT_EUID, &euid) && auxv_get(AT_GID, &gid) &&
                 auxv_get(AT_EGID, &egid) && uid == (unsigned long)getuid() && euid == (unsigned long)geteuid() &&
                 gid == (unsigned long)getgid() && egid == (unsigned long)getegid() && auxv_get(AT_PAGESZ, &pagesz) &&
                 pagesz == (unsigned long)sysconf(_SC_PAGESIZE);

    char flags[4096] = {0};
    int model_ok = 0, hwcap_ok = 0;

#if defined(__x86_64__)
    // CPUID bit -> /proc/cpuinfo token. reg indexes {eax,ebx,ecx,edx}; the two invariant-TSC names share
    // one bit, and cpuid/nopl are the synthetics every long-mode CPU reports.
    static const struct {
        unsigned leaf, sub;
        unsigned char reg, bit;
        const char *name;
    } TBL[] = {
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

    unsigned r[4];
    __asm__ volatile("cpuid" : "=a"(r[0]), "=b"(r[1]), "=c"(r[2]), "=d"(r[3]) : "a"(1u), "c"(0u));
    hwcap_ok = have_hwcap && hwcap == (unsigned long)r[3]; // AT_HWCAP on x86-64 IS CPUID.1:EDX

    pf_line_val(cpuinfo, "flags", flags, (int)sizeof flags);
    model_ok = flags[0] != 0;
    for (size_t i = 0; i < sizeof TBL / sizeof TBL[0]; i++) {
        unsigned q[4];
        __asm__ volatile("cpuid" : "=a"(q[0]), "=b"(q[1]), "=c"(q[2]), "=d"(q[3]) : "a"(TBL[i].leaf), "c"(TBL[i].sub));
        int advertised = (q[TBL[i].reg] >> TBL[i].bit) & 1u;
        if (advertised != token_in(flags, TBL[i].name)) {
            model_ok = 0;
            fprintf(stderr, "cpumodel: %s cpuid=%d cpuinfo=%d\n", TBL[i].name, advertised,
                    token_in(flags, TBL[i].name));
        }
    }
#elif defined(__aarch64__)
    // AT_HWCAP is the model here: hl copies g_aarch64_cpu_model.hwcap into it verbatim, and the CPUID
    // feature registers are not user-readable (HWCAP_CPUID, bit 11, is deliberately clear). Names in kernel
    // (arch/arm64/kernel/cpuinfo.c) order.
    static const char *const HWCAP_NAME[64] = {
        "fp",    "asimd",    "evtstrm", "aes",   "pmull",  "sha1",  "sha2", "crc32", "atomics", "fphp",    "asimdhp",
        "cpuid", "asimdrdm", "jscvt",   "fcma",  "lrcpc",  "dcpop", "sha3", "sm3",   "sm4",     "asimddp", "sha512",
        "sve",   "asimdfhm", "dit",     "uscat", "ilrcpc", "flagm", "ssbs", "sb",    "paca",    "pacg"};
    static const char *const HWCAP2_NAME[64] = {"dcpodp",  "sve2",   "sveaes", "svepmull", "svebitperm", "svesha3",
                                                "svesm4",  "flagm2", "frint",  "svei8mm",  "svef32mm",   "svef64mm",
                                                "svebf16", "i8mm",   "bf16",   "dgh",      "rng",        "bti",
                                                "mte",     "ecv",    "afp",    "rpres"};
    hwcap_ok = have_hwcap && hwcap != 0;
    pf_line_val(cpuinfo, "Features", flags, (int)sizeof flags);
    model_ok = flags[0] != 0;
    const unsigned long caps[2] = {hwcap, hwcap2};
    const char *const *names[2] = {HWCAP_NAME, HWCAP2_NAME};
    for (int w = 0; w < 2; w++)
        for (int i = 0; i < 64; i++) {
            if (!names[w][i]) continue;
            int advertised = (int)((caps[w] >> i) & 1u);
            if (advertised != token_in(flags, names[w][i])) {
                model_ok = 0;
                fprintf(stderr, "cpumodel: %s hwcap%d=%d cpuinfo=%d\n", names[w][i], w + 1, advertised,
                        token_in(flags, names[w][i]));
            }
        }
#endif

    int ok = cn > 0 && have_auxv && hwcap_ok && ids_ok && model_ok;
    printf("cpumodel auxv=%d hwcap=%d ids=%d cpuinfo=%d ok=%d\n", have_auxv, hwcap_ok, ids_ok, model_ok, ok);
    return 0;
}
