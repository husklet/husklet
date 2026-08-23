// guests/ibtc_dispatch.c -- IBTC stress + regression guest (perf lever #4).
//
// TWO indirect-dispatch shapes the DBT's inline IBTC must handle, both hammered hard:
//   (1) a MEGAMORPHIC computed-goto bytecode VM -- ONE `goto *dispatch[op]` site with 128 distinct
//       opcode-handler targets, driven by a SKEWED (Zipfian) opcode stream so a small hot working
//       set dominates while the long tail still exercises every one of the 128 targets. This is the
//       shape of CPython's eval loop / a SQLite VDBE switch, and its locality is what a higher-
//       associativity IBTC captures.
//   (2) a MONOMORPHIC deep recursion -- fib() built with __attribute__((noinline,optimize("no-
//       optimize-sibling-calls"))) so a REAL call/ret executes (not a compiler-rewritten loop). Its
//       `ret` always returns into one of fib's two call sites -> the temporally-monomorphic call/ret
//       traffic that deep recursion + qsort lean on.
//
// The uint64 checksum is deterministic, so it is GOLDEN-checked byte-identically on the aarch64 and
// x86_64 engines: a wrong IBTC prediction that jumped to the wrong handler/return would corrupt it.
// Pure C + stdio -> static-pie portable. Doubles as the IBPROF / IBTC-associativity measurement load.
#include <stdio.h>
#include <stdint.h>

// ---- (2) monomorphic real recursion. noinline + no-sibling-calls => a genuine bl/ret pair; the ret
// target is one of fib's two call sites (temporally monomorphic per site). Tree recursion cannot be
// lowered to a loop, so the call/ret traffic is real.
__attribute__((noinline, optimize("no-optimize-sibling-calls"))) static uint64_t fib(uint32_t n) {
    if (n < 2) return n;
    return fib(n - 1) + fib(n - 2);
}

int main(void) {
    // ---- (1) 128-target megamorphic computed-goto VM ----
    // 128 distinct label addresses (GNU computed goto). The `goto *` below is the megamorphic site.
    static const void *const tab[128] = {
        &&L0,   &&L1,   &&L2,   &&L3,   &&L4,   &&L5,   &&L6,   &&L7,   &&L8,   &&L9,   &&L10,  &&L11,  &&L12,
        &&L13,  &&L14,  &&L15,  &&L16,  &&L17,  &&L18,  &&L19,  &&L20,  &&L21,  &&L22,  &&L23,  &&L24,  &&L25,
        &&L26,  &&L27,  &&L28,  &&L29,  &&L30,  &&L31,  &&L32,  &&L33,  &&L34,  &&L35,  &&L36,  &&L37,  &&L38,
        &&L39,  &&L40,  &&L41,  &&L42,  &&L43,  &&L44,  &&L45,  &&L46,  &&L47,  &&L48,  &&L49,  &&L50,  &&L51,
        &&L52,  &&L53,  &&L54,  &&L55,  &&L56,  &&L57,  &&L58,  &&L59,  &&L60,  &&L61,  &&L62,  &&L63,  &&L64,
        &&L65,  &&L66,  &&L67,  &&L68,  &&L69,  &&L70,  &&L71,  &&L72,  &&L73,  &&L74,  &&L75,  &&L76,  &&L77,
        &&L78,  &&L79,  &&L80,  &&L81,  &&L82,  &&L83,  &&L84,  &&L85,  &&L86,  &&L87,  &&L88,  &&L89,  &&L90,
        &&L91,  &&L92,  &&L93,  &&L94,  &&L95,  &&L96,  &&L97,  &&L98,  &&L99,  &&L100, &&L101, &&L102, &&L103,
        &&L104, &&L105, &&L106, &&L107, &&L108, &&L109, &&L110, &&L111, &&L112, &&L113, &&L114, &&L115, &&L116,
        &&L117, &&L118, &&L119, &&L120, &&L121, &&L122, &&L123, &&L124, &&L125, &&L126, &&L127,
    };

    // Zipfian opcode stream: opcode k gets weight ~ 1/(k+1). Build a 4096-entry program by drawing
    // from a harmonic CDF with an LCG -> a hot handful of opcodes dominate, the tail still hits all
    // 128. Deterministic (fixed seed).
    enum { PROG = 4096 };

    static unsigned char prog[PROG];
    static double cdf[128];
    {
        double h = 0;
        for (int k = 0; k < 128; k++)
            h += 1.0 / (k + 1);
        double run = 0;
        for (int k = 0; k < 128; k++) {
            run += (1.0 / (k + 1)) / h;
            cdf[k] = run;
        }
    }
    uint64_t s = 0x1234567ull;
    for (int i = 0; i < PROG; i++) {
        s = s * 6364136223846793005ull + 1442695040888963407ull;
        double u = (double)((s >> 11) & 0x1FFFFFFFFFFFFFull) / (double)0x20000000000000ull; // [0,1)
        int k = 0;
        while (k < 128 - 1 && u > cdf[k])
            k++;
        prog[i] = (unsigned char)k;
    }
    uint64_t acc = 0;
    uint64_t pc = 0;
    uint64_t budget = 20000000ull; // ~20M dispatches through the megamorphic site
    goto *tab[prog[pc]];
#define VM_OP(label, multiplier, increment)                                                                            \
    label:                                                                                                             \
    acc = acc * multiplier##ull + increment##ull;                                                                      \
    goto next;
    VM_OP(L0, 1, 40503)
    VM_OP(L1, 3, 40502)
    VM_OP(L2, 5, 40501)
    VM_OP(L3, 7, 40500)
    VM_OP(L4, 9, 40499)
    VM_OP(L5, 11, 40498)
    VM_OP(L6, 13, 40497)
    VM_OP(L7, 15, 40496)
    VM_OP(L8, 17, 40511)
    VM_OP(L9, 19, 40510)
    VM_OP(L10, 21, 40509)
    VM_OP(L11, 23, 40508)
    VM_OP(L12, 25, 40507)
    VM_OP(L13, 27, 40506)
    VM_OP(L14, 29, 40505)
    VM_OP(L15, 31, 40504)
    VM_OP(L16, 33, 40487)
    VM_OP(L17, 35, 40486)
    VM_OP(L18, 37, 40485)
    VM_OP(L19, 39, 40484)
    VM_OP(L20, 41, 40483)
    VM_OP(L21, 43, 40482)
    VM_OP(L22, 45, 40481)
    VM_OP(L23, 47, 40480)
    VM_OP(L24, 49, 40495)
    VM_OP(L25, 51, 40494)
    VM_OP(L26, 53, 40493)
    VM_OP(L27, 55, 40492)
    VM_OP(L28, 57, 40491)
    VM_OP(L29, 59, 40490)
    VM_OP(L30, 61, 40489)
    VM_OP(L31, 63, 40488)
    VM_OP(L32, 65, 40471)
    VM_OP(L33, 67, 40470)
    VM_OP(L34, 69, 40469)
    VM_OP(L35, 71, 40468)
    VM_OP(L36, 73, 40467)
    VM_OP(L37, 75, 40466)
    VM_OP(L38, 77, 40465)
    VM_OP(L39, 79, 40464)
    VM_OP(L40, 81, 40479)
    VM_OP(L41, 83, 40478)
    VM_OP(L42, 85, 40477)
    VM_OP(L43, 87, 40476)
    VM_OP(L44, 89, 40475)
    VM_OP(L45, 91, 40474)
    VM_OP(L46, 93, 40473)
    VM_OP(L47, 95, 40472)
    VM_OP(L48, 97, 40455)
    VM_OP(L49, 99, 40454)
    VM_OP(L50, 101, 40453)
    VM_OP(L51, 103, 40452)
    VM_OP(L52, 105, 40451)
    VM_OP(L53, 107, 40450)
    VM_OP(L54, 109, 40449)
    VM_OP(L55, 111, 40448)
    VM_OP(L56, 113, 40463)
    VM_OP(L57, 115, 40462)
    VM_OP(L58, 117, 40461)
    VM_OP(L59, 119, 40460)
    VM_OP(L60, 121, 40459)
    VM_OP(L61, 123, 40458)
    VM_OP(L62, 125, 40457)
    VM_OP(L63, 127, 40456)
    VM_OP(L64, 129, 40567)
    VM_OP(L65, 131, 40566)
    VM_OP(L66, 133, 40565)
    VM_OP(L67, 135, 40564)
    VM_OP(L68, 137, 40563)
    VM_OP(L69, 139, 40562)
    VM_OP(L70, 141, 40561)
    VM_OP(L71, 143, 40560)
    VM_OP(L72, 145, 40575)
    VM_OP(L73, 147, 40574)
    VM_OP(L74, 149, 40573)
    VM_OP(L75, 151, 40572)
    VM_OP(L76, 153, 40571)
    VM_OP(L77, 155, 40570)
    VM_OP(L78, 157, 40569)
    VM_OP(L79, 159, 40568)
    VM_OP(L80, 161, 40551)
    VM_OP(L81, 163, 40550)
    VM_OP(L82, 165, 40549)
    VM_OP(L83, 167, 40548)
    VM_OP(L84, 169, 40547)
    VM_OP(L85, 171, 40546)
    VM_OP(L86, 173, 40545)
    VM_OP(L87, 175, 40544)
    VM_OP(L88, 177, 40559)
    VM_OP(L89, 179, 40558)
    VM_OP(L90, 181, 40557)
    VM_OP(L91, 183, 40556)
    VM_OP(L92, 185, 40555)
    VM_OP(L93, 187, 40554)
    VM_OP(L94, 189, 40553)
    VM_OP(L95, 191, 40552)
    VM_OP(L96, 193, 40535)
    VM_OP(L97, 195, 40534)
    VM_OP(L98, 197, 40533)
    VM_OP(L99, 199, 40532)
    VM_OP(L100, 201, 40531)
    VM_OP(L101, 203, 40530)
    VM_OP(L102, 205, 40529)
    VM_OP(L103, 207, 40528)
    VM_OP(L104, 209, 40543)
    VM_OP(L105, 211, 40542)
    VM_OP(L106, 213, 40541)
    VM_OP(L107, 215, 40540)
    VM_OP(L108, 217, 40539)
    VM_OP(L109, 219, 40538)
    VM_OP(L110, 221, 40537)
    VM_OP(L111, 223, 40536)
    VM_OP(L112, 225, 40519)
    VM_OP(L113, 227, 40518)
    VM_OP(L114, 229, 40517)
    VM_OP(L115, 231, 40516)
    VM_OP(L116, 233, 40515)
    VM_OP(L117, 235, 40514)
    VM_OP(L118, 237, 40513)
    VM_OP(L119, 239, 40512)
    VM_OP(L120, 241, 40527)
    VM_OP(L121, 243, 40526)
    VM_OP(L122, 245, 40525)
    VM_OP(L123, 247, 40524)
    VM_OP(L124, 249, 40523)
    VM_OP(L125, 251, 40522)
    VM_OP(L126, 253, 40521)
    VM_OP(L127, 255, 40520)
#undef VM_OP
next:
    if (--budget == 0) goto vmdone;
    pc = (pc + 1) & (PROG - 1);
    goto *tab[prog[pc]];
vmdone:;

    // ---- (2) monomorphic call/ret traffic: fib(32) ~ 3.5M real calls, all rets monomorphic-per-site ----
    uint64_t rec = fib(32); // fib(32) = 2178309

    // Mix both into one deterministic checksum.
    uint64_t chk = acc ^ (rec * 1099511628211ull);
    printf("ibtc vm=%llu rec=%llu chk=%llu\n", (unsigned long long)acc, (unsigned long long)rec,
           (unsigned long long)chk);
    return 0;
}
