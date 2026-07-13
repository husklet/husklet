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
__attribute__((noinline, optimize("no-optimize-sibling-calls")))
static uint64_t fib(uint32_t n) {
    if (n < 2) return n;
    return fib(n - 1) + fib(n - 2);
}

int main(void) {
    // ---- (1) 128-target megamorphic computed-goto VM ----
    // 128 distinct label addresses (GNU computed goto). The `goto *` below is the megamorphic site.
    static const void *const tab[128] = {
        &&L0,&&L1,&&L2,&&L3,&&L4,&&L5,&&L6,&&L7,&&L8,&&L9,&&L10,&&L11,&&L12,&&L13,&&L14,&&L15,&&L16,
        &&L17,&&L18,&&L19,&&L20,&&L21,&&L22,&&L23,&&L24,&&L25,&&L26,&&L27,&&L28,&&L29,&&L30,&&L31,
        &&L32,&&L33,&&L34,&&L35,&&L36,&&L37,&&L38,&&L39,&&L40,&&L41,&&L42,&&L43,&&L44,&&L45,&&L46,
        &&L47,&&L48,&&L49,&&L50,&&L51,&&L52,&&L53,&&L54,&&L55,&&L56,&&L57,&&L58,&&L59,&&L60,&&L61,
        &&L62,&&L63,&&L64,&&L65,&&L66,&&L67,&&L68,&&L69,&&L70,&&L71,&&L72,&&L73,&&L74,&&L75,&&L76,
        &&L77,&&L78,&&L79,&&L80,&&L81,&&L82,&&L83,&&L84,&&L85,&&L86,&&L87,&&L88,&&L89,&&L90,&&L91,
        &&L92,&&L93,&&L94,&&L95,&&L96,&&L97,&&L98,&&L99,&&L100,&&L101,&&L102,&&L103,&&L104,&&L105,
        &&L106,&&L107,&&L108,&&L109,&&L110,&&L111,&&L112,&&L113,&&L114,&&L115,&&L116,&&L117,&&L118,
        &&L119,&&L120,&&L121,&&L122,&&L123,&&L124,&&L125,&&L126,&&L127,
    };
    // Zipfian opcode stream: opcode k gets weight ~ 1/(k+1). Build a 4096-entry program by drawing
    // from a harmonic CDF with an LCG -> a hot handful of opcodes dominate, the tail still hits all
    // 128. Deterministic (fixed seed).
    enum { PROG = 4096 };
    static unsigned char prog[PROG];
    static double cdf[128];
    { double h = 0; for (int k = 0; k < 128; k++) h += 1.0 / (k + 1); 
      double run = 0; for (int k = 0; k < 128; k++) { run += (1.0 / (k + 1)) / h; cdf[k] = run; } }
    uint64_t s = 0x1234567ull;
    for (int i = 0; i < PROG; i++) {
        s = s * 6364136223846793005ull + 1442695040888963407ull;
        double u = (double)((s >> 11) & 0x1FFFFFFFFFFFFFull) / (double)0x20000000000000ull; // [0,1)
        int k = 0; while (k < 128 - 1 && u > cdf[k]) k++;
        prog[i] = (unsigned char)k;
    }
    uint64_t acc = 0;
    uint64_t pc = 0;
    uint64_t budget = 20000000ull; // ~20M dispatches through the megamorphic site
    goto *tab[prog[pc]];
L0: acc = acc * 1ull + 40503ull; goto next;
L1: acc = acc * 3ull + 40502ull; goto next;
L2: acc = acc * 5ull + 40501ull; goto next;
L3: acc = acc * 7ull + 40500ull; goto next;
L4: acc = acc * 9ull + 40499ull; goto next;
L5: acc = acc * 11ull + 40498ull; goto next;
L6: acc = acc * 13ull + 40497ull; goto next;
L7: acc = acc * 15ull + 40496ull; goto next;
L8: acc = acc * 17ull + 40511ull; goto next;
L9: acc = acc * 19ull + 40510ull; goto next;
L10: acc = acc * 21ull + 40509ull; goto next;
L11: acc = acc * 23ull + 40508ull; goto next;
L12: acc = acc * 25ull + 40507ull; goto next;
L13: acc = acc * 27ull + 40506ull; goto next;
L14: acc = acc * 29ull + 40505ull; goto next;
L15: acc = acc * 31ull + 40504ull; goto next;
L16: acc = acc * 33ull + 40487ull; goto next;
L17: acc = acc * 35ull + 40486ull; goto next;
L18: acc = acc * 37ull + 40485ull; goto next;
L19: acc = acc * 39ull + 40484ull; goto next;
L20: acc = acc * 41ull + 40483ull; goto next;
L21: acc = acc * 43ull + 40482ull; goto next;
L22: acc = acc * 45ull + 40481ull; goto next;
L23: acc = acc * 47ull + 40480ull; goto next;
L24: acc = acc * 49ull + 40495ull; goto next;
L25: acc = acc * 51ull + 40494ull; goto next;
L26: acc = acc * 53ull + 40493ull; goto next;
L27: acc = acc * 55ull + 40492ull; goto next;
L28: acc = acc * 57ull + 40491ull; goto next;
L29: acc = acc * 59ull + 40490ull; goto next;
L30: acc = acc * 61ull + 40489ull; goto next;
L31: acc = acc * 63ull + 40488ull; goto next;
L32: acc = acc * 65ull + 40471ull; goto next;
L33: acc = acc * 67ull + 40470ull; goto next;
L34: acc = acc * 69ull + 40469ull; goto next;
L35: acc = acc * 71ull + 40468ull; goto next;
L36: acc = acc * 73ull + 40467ull; goto next;
L37: acc = acc * 75ull + 40466ull; goto next;
L38: acc = acc * 77ull + 40465ull; goto next;
L39: acc = acc * 79ull + 40464ull; goto next;
L40: acc = acc * 81ull + 40479ull; goto next;
L41: acc = acc * 83ull + 40478ull; goto next;
L42: acc = acc * 85ull + 40477ull; goto next;
L43: acc = acc * 87ull + 40476ull; goto next;
L44: acc = acc * 89ull + 40475ull; goto next;
L45: acc = acc * 91ull + 40474ull; goto next;
L46: acc = acc * 93ull + 40473ull; goto next;
L47: acc = acc * 95ull + 40472ull; goto next;
L48: acc = acc * 97ull + 40455ull; goto next;
L49: acc = acc * 99ull + 40454ull; goto next;
L50: acc = acc * 101ull + 40453ull; goto next;
L51: acc = acc * 103ull + 40452ull; goto next;
L52: acc = acc * 105ull + 40451ull; goto next;
L53: acc = acc * 107ull + 40450ull; goto next;
L54: acc = acc * 109ull + 40449ull; goto next;
L55: acc = acc * 111ull + 40448ull; goto next;
L56: acc = acc * 113ull + 40463ull; goto next;
L57: acc = acc * 115ull + 40462ull; goto next;
L58: acc = acc * 117ull + 40461ull; goto next;
L59: acc = acc * 119ull + 40460ull; goto next;
L60: acc = acc * 121ull + 40459ull; goto next;
L61: acc = acc * 123ull + 40458ull; goto next;
L62: acc = acc * 125ull + 40457ull; goto next;
L63: acc = acc * 127ull + 40456ull; goto next;
L64: acc = acc * 129ull + 40567ull; goto next;
L65: acc = acc * 131ull + 40566ull; goto next;
L66: acc = acc * 133ull + 40565ull; goto next;
L67: acc = acc * 135ull + 40564ull; goto next;
L68: acc = acc * 137ull + 40563ull; goto next;
L69: acc = acc * 139ull + 40562ull; goto next;
L70: acc = acc * 141ull + 40561ull; goto next;
L71: acc = acc * 143ull + 40560ull; goto next;
L72: acc = acc * 145ull + 40575ull; goto next;
L73: acc = acc * 147ull + 40574ull; goto next;
L74: acc = acc * 149ull + 40573ull; goto next;
L75: acc = acc * 151ull + 40572ull; goto next;
L76: acc = acc * 153ull + 40571ull; goto next;
L77: acc = acc * 155ull + 40570ull; goto next;
L78: acc = acc * 157ull + 40569ull; goto next;
L79: acc = acc * 159ull + 40568ull; goto next;
L80: acc = acc * 161ull + 40551ull; goto next;
L81: acc = acc * 163ull + 40550ull; goto next;
L82: acc = acc * 165ull + 40549ull; goto next;
L83: acc = acc * 167ull + 40548ull; goto next;
L84: acc = acc * 169ull + 40547ull; goto next;
L85: acc = acc * 171ull + 40546ull; goto next;
L86: acc = acc * 173ull + 40545ull; goto next;
L87: acc = acc * 175ull + 40544ull; goto next;
L88: acc = acc * 177ull + 40559ull; goto next;
L89: acc = acc * 179ull + 40558ull; goto next;
L90: acc = acc * 181ull + 40557ull; goto next;
L91: acc = acc * 183ull + 40556ull; goto next;
L92: acc = acc * 185ull + 40555ull; goto next;
L93: acc = acc * 187ull + 40554ull; goto next;
L94: acc = acc * 189ull + 40553ull; goto next;
L95: acc = acc * 191ull + 40552ull; goto next;
L96: acc = acc * 193ull + 40535ull; goto next;
L97: acc = acc * 195ull + 40534ull; goto next;
L98: acc = acc * 197ull + 40533ull; goto next;
L99: acc = acc * 199ull + 40532ull; goto next;
L100: acc = acc * 201ull + 40531ull; goto next;
L101: acc = acc * 203ull + 40530ull; goto next;
L102: acc = acc * 205ull + 40529ull; goto next;
L103: acc = acc * 207ull + 40528ull; goto next;
L104: acc = acc * 209ull + 40543ull; goto next;
L105: acc = acc * 211ull + 40542ull; goto next;
L106: acc = acc * 213ull + 40541ull; goto next;
L107: acc = acc * 215ull + 40540ull; goto next;
L108: acc = acc * 217ull + 40539ull; goto next;
L109: acc = acc * 219ull + 40538ull; goto next;
L110: acc = acc * 221ull + 40537ull; goto next;
L111: acc = acc * 223ull + 40536ull; goto next;
L112: acc = acc * 225ull + 40519ull; goto next;
L113: acc = acc * 227ull + 40518ull; goto next;
L114: acc = acc * 229ull + 40517ull; goto next;
L115: acc = acc * 231ull + 40516ull; goto next;
L116: acc = acc * 233ull + 40515ull; goto next;
L117: acc = acc * 235ull + 40514ull; goto next;
L118: acc = acc * 237ull + 40513ull; goto next;
L119: acc = acc * 239ull + 40512ull; goto next;
L120: acc = acc * 241ull + 40527ull; goto next;
L121: acc = acc * 243ull + 40526ull; goto next;
L122: acc = acc * 245ull + 40525ull; goto next;
L123: acc = acc * 247ull + 40524ull; goto next;
L124: acc = acc * 249ull + 40523ull; goto next;
L125: acc = acc * 251ull + 40522ull; goto next;
L126: acc = acc * 253ull + 40521ull; goto next;
L127: acc = acc * 255ull + 40520ull; goto next;
next:
    if (--budget == 0) goto vmdone;
    pc = (pc + 1) & (PROG - 1);
    goto *tab[prog[pc]];
vmdone:;

    // ---- (2) monomorphic call/ret traffic: fib(32) ~ 3.5M real calls, all rets monomorphic-per-site ----
    uint64_t rec = fib(32); // fib(32) = 2178309

    // Mix both into one deterministic checksum.
    uint64_t chk = acc ^ (rec * 1099511628211ull);
    printf("ibtc vm=%llu rec=%llu chk=%llu\n", (unsigned long long)acc, (unsigned long long)rec, (unsigned long long)chk);
    return 0;
}
