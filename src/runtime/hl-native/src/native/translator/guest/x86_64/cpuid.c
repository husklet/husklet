#include "cpuid.h"

#include <stdint.h>

#include "cpu.h"

#include <string.h>

void hl_x86_cpuid(struct cpu *c) {
    uint32_t leaf = (uint32_t)c->r[RAX], a = 0, b = 0, cc = 0, d = 0;
    switch (leaf) {
    case 0:
        a = 7;
        b = 0x756e6547;
        d = 0x49656e69;
        cc = 0x6c65746e;
        break; // max-leaf=7, "GenuineIntel"
    case 1:
        // Report a pre-AVX Intel Westmere (family 6, model 0x2c). Rationale: hl deliberately withholds
        // AVX (xgetbv reports only x87+SSE), so guest glibc takes its SSE path for memcpy/memmove. glibc's
        // memmove ifunc, when AVX is unusable, picks `__memmove_ssse3` (NO rep movsb) UNLESS the internal
        // `Fast_Unaligned_Copy` preferred bit is set -- and glibc only sets that bit for Nehalem/Westmere
        // models (Haswell+ is assumed to have AVX, so its SSE fast-copy heuristic was never wired). A
        // Haswell id (old value 0x000306c3) therefore left memcpy on the non-rep ssse3 path; Westmere makes
        // glibc select `__memmove_sse2_unaligned_erms` -> `rep movsb` above threshold, which hl lowers to a
        // single host memcpy. All features hl advertises (SSE4.2/AES-NI/PCLMUL/POPCNT below, BMI2/SHA/ERMS/
        // FSRM in leaf 7) are consumed by guests via their CPUID FEATURE bits, which stay set regardless of
        // this model id -- the model only steers glibc's SSE-vs-rep tuning toward the engine's fast rep path.
        a = 0x000206c2; // Intel Westmere (fam 6, model 0x2c) -- trips glibc Fast_Unaligned_Copy (SSE memcpy -> rep
                        // movsb)
        d = (1u << 0) | (1u << 4) | (1u << 8) | (1u << 11) | (1u << 13) | (1u << 15) | (1u << 19) | (1u << 23) |
            (1u << 24) | (1u << 25) | (1u << 26); // FPU,TSC,CX8,SEP,PGE,CMOV,CLFSH,MMX,FXSR,SSE,SSE2
        // SSE3, PCLMULQDQ, SSSE3, CMPXCHG16B, SSE4.1, SSE4.2, POPCNT, AES-NI -- all backed by the
        // legacy/0F38/0F3A lowerings; none need YMM state (XMM only, covered by xgetbv's SSE bit).
        // MOVBE (bit 22) is deliberately NOT advertised even though hl lowers it inline: real Westmere
        // has no MOVBE, and "MOVBE && !XSAVE" is the Intel-Atom fingerprint openssl's ia32cap checks key
        // their SLOW Atom-tuned paths on (2-block ghash instead of the 4-block Karatsuba ghash, 6-block
        // movbe ctr32 instead of the 8-block loop) -- advertising it cost ~40% of real AES-GCM throughput.
        cc = (1u << 0) | (1u << 1) | (1u << 9) | (1u << 13) | (1u << 19) | (1u << 20) | (1u << 23) | (1u << 25);
        break;
    case 7:
        if ((uint32_t)c->r[RCX] == 0) {                         // subleaf 0
            b = (1u << 3) | (1u << 8) | (1u << 9) | (1u << 29); // BMI1, BMI2, ERMS, SHA
            // ERMS (EBX[9]) + FSRM (EDX[4]): "REP MOVSB/STOSB is fast" + "fast for short lengths". We
            // advertise these because hl lowers `rep movs`/`rep stos` to ONE bit-exact host memcpy/memset
            // (translate/repstr.c: forward, all widths/alignments/0-length, forward-overlap smear replayed
            // element-granular; DF=1/backward falls back to the exact per-element scalar loop). glibc's
            // memcpy/memmove/memset ERMS paths only ever issue `rep movsb/stosb` in the FORWARD direction
            // (they take the backward SSE loop for dst>src overlap), which is exactly the case hl's fast
            // path serves -- so routing bulk libc copies here is byte-exact and avoids translating glibc's
            // SSE/AVX copy loops (which also hit the SSSE3/SSE4 softmulator exits). max-subleaf stays 0
            // (a==0): FSRM lives in subleaf 0, so no subleaf-1 emulation is needed.
            d = (1u << 4); // FSRM (Fast Short REP MOV); only bit 4 set -- no AVX512/security bits implied
        }
        break;
    case 0x80000000: a = 0x80000008; break; // max ext leaf 0x80000008 (brand string 2..4 + addr sizes below)
    case 0x80000001:
        // EDX: SYSCALL(11), NX(20), RDTSCP(27), LM(29). NX is a paging/MMU capability EVERY x86-64 CPU
        // reports; guests never touch page tables, and hl honors PROT_EXEC page permissions in its mmap
        // emulation (os/linux/syscall/mem.c), so software that requires NX (glibc, dynamic loaders, managed
        // runtimes) is satisfied. RDTSCP is lowered inline (translate.c: rdtscp -> cntvct, ecx=TSC_AUX=0),
        // so it is real; advertise it. ECX: LAHF/SAHF-in-long-mode (bit 0), which the engine implements.
        d = (1u << 11) | (1u << 20) | (1u << 27) | (1u << 29);
        cc = (1u << 0);
        break;
    case 0x80000002:
    case 0x80000003:
    case 0x80000004: { // processor brand string (48 bytes across 3 leaves); MUST match /proc/cpuinfo model name
        static const char brand[48] = "hl JIT x86-64 processor";
        const uint8_t *p = (const uint8_t *)brand + (leaf - 0x80000002) * 16;
        memcpy(&a, p, 4);
        memcpy(&b, p + 4, 4);
        memcpy(&cc, p + 8, 4);
        memcpy(&d, p + 12, 4);
        break;
    }
    case 0x80000007: d = (1u << 8); break; // EDX[8] Invariant TSC (ARM cntvct is fixed-rate) -> constant/nonstop_tsc
    case 0x80000008: a = 0x3027; break;    // EAX[7:0]=39 phys bits, EAX[15:8]=48 virt bits
    default: break;
    }
    c->r[RAX] = a;
    c->r[RBX] = b;
    c->r[RCX] = cc;
    c->r[RDX] = d;
}
