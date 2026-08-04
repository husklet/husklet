// FCW's rounding control (RC, bits 11:10) and precision control (PC, bits 9:8) applied to x87 ARITHMETIC and
// to the narrowing stores -- not just to FRNDINT/FIST, which were the only two that honoured them.
//
// RC: `1/3` divided under RC=up and stored to m64 is 3fd5555555555556 on hardware; ignoring RC gives ...555.
// `fstp m32` of 1/3 under RC=down is 3eaaaaaa, not 3eaaaaab.
//
// PC=24 is a real, separate arithmetic mode -- float precision inside a register with the full 15-bit
// exponent range -- and `1 + 2^-30` there is exactly 1.0 with #P set, not 1.0000000009313226. Every value
// below is chosen to round differently at 24 bits than at 53.
//
// Deliberately NOT covered: operands whose x87 result needs an exponent outside binary64's 11-bit range
// (DBL_MAX*3 is finite in an 80-bit register and overflows only at the m64 store), and PC=64's 11 bits of
// extra significand. Those need a genuinely 80-bit register file, and every case here is chosen to be
// decided by RC/PC alone.
#include <stdint.h>
#include <stdio.h>

static unsigned fsw(void) {
    unsigned short s;
    __asm__ volatile("fnstsw %0" : "=am"(s));
    return s;
}
static void init(void) { __asm__ volatile("fninit"); }
static void setcw(unsigned short c) { __asm__ volatile("fldcw %0" : : "m"(c)); }

static const unsigned short RC[4] = {0x037f, 0x077f, 0x0b7f, 0x0f7f}; // near/down/up/zero at PC=64
static const unsigned short RC53[4] = {0x027f, 0x067f, 0x0a7f, 0x0e7f};
static const unsigned short RC24[4] = {0x003f, 0x043f, 0x083f, 0x0c3f};
static const char *RCNAME[4] = {"near", "down", "up  ", "zero"};

static void f64(const char *tag, unsigned short cw, double a, double b, int op) {
    double o;
    init();
    setcw(cw);
    switch (op) {
    case 0: __asm__ volatile("fldl %1\n\tfaddl %2\n\tfstpl %0" : "=m"(o) : "m"(a), "m"(b) : "memory"); break;
    case 1: __asm__ volatile("fldl %1\n\tfsubl %2\n\tfstpl %0" : "=m"(o) : "m"(a), "m"(b) : "memory"); break;
    case 2: __asm__ volatile("fldl %1\n\tfmull %2\n\tfstpl %0" : "=m"(o) : "m"(a), "m"(b) : "memory"); break;
    case 3: __asm__ volatile("fldl %1\n\tfdivl %2\n\tfstpl %0" : "=m"(o) : "m"(a), "m"(b) : "memory"); break;
    default: __asm__ volatile("fldl %1\n\tfsqrt\n\tfstpl %0" : "=m"(o) : "m"(a) : "memory"); break;
    }
    printf("%-22s cw=%04x f64=%016llx exc=%02x\n", tag, cw, (unsigned long long)*(uint64_t *)&o, fsw() & 0x3f);
}

static void m32(const char *tag, unsigned short cw, double a) {
    float o;
    init();
    setcw(cw);
    __asm__ volatile("fldl %1\n\tfstps %0" : "=m"(o) : "m"(a) : "memory");
    printf("%-22s cw=%04x m32=%08x exc=%02x\n", tag, cw, (unsigned)*(uint32_t *)&o, fsw() & 0x3f);
}

int main(void) {
    static const struct {
        double a, b;
        int op;
        const char *name;
    } cases[] = {
        {1.0, 3.0, 3, "1/3"},         {10.0, 7.0, 3, "10/7"},   {1.0, 49.0, 3, "1/49"},
        {2.0, 0.0, 4, "sqrt2"},       {3.0, 0.0, 4, "sqrt3"},   {10.0, 0.0, 4, "sqrt10"},
        {1e16, 3.0, 0, "1e16+3"},     {1.0, 1e-17, 0, "1+1e-17"}, {0.1, 0.2, 0, "0.1+0.2"},
        {1.0, 1e-17, 1, "1-1e-17"},   {3.0, 7.0, 2, "3*7"},     {0.1, 0.3, 2, "0.1*0.3"},
    };
    puts("== RC at PC=64 (the FNINIT default)");
    for (unsigned i = 0; i < sizeof cases / sizeof cases[0]; i++)
        for (int r = 0; r < 4; r++) {
            char tag[40];
            snprintf(tag, sizeof tag, "%s %s", cases[i].name, RCNAME[r]);
            f64(tag, RC[r], cases[i].a, cases[i].b, cases[i].op);
        }
    puts("== RC at PC=53 (exactly the carrier's width)");
    for (unsigned i = 0; i < sizeof cases / sizeof cases[0]; i++)
        for (int r = 0; r < 4; r++) {
            char tag[40];
            snprintf(tag, sizeof tag, "%s %s", cases[i].name, RCNAME[r]);
            f64(tag, RC53[r], cases[i].a, cases[i].b, cases[i].op);
        }
    puts("== PC=24: float-precision arithmetic, full exponent range");
    for (unsigned i = 0; i < sizeof cases / sizeof cases[0]; i++)
        for (int r = 0; r < 4; r++) {
            char tag[40];
            snprintf(tag, sizeof tag, "%s %s", cases[i].name, RCNAME[r]);
            f64(tag, RC24[r], cases[i].a, cases[i].b, cases[i].op);
        }
    puts("== PC=24 on a sum that is exact at 53 bits and not at 24");
    {
        static const double eps[] = {1.0 / 1073741824.0 /*2^-30*/, 1.0 / 16777216.0 /*2^-24*/,
                                     1.0 / 8388608.0 /*2^-23*/, 1.0 / 33554432.0 /*2^-25*/};
        for (unsigned i = 0; i < sizeof eps / sizeof eps[0]; i++)
            for (int r = 0; r < 4; r++) {
                char tag[40];
                snprintf(tag, sizeof tag, "1+eps[%u] %s", i, RCNAME[r]);
                f64(tag, RC24[r], 1.0, eps[i], 0);
                snprintf(tag, sizeof tag, "pc53 1+eps[%u] %s", i, RCNAME[r]);
                f64(tag, RC53[r], 1.0, eps[i], 0);
            }
    }
    puts("== PC=24 keeps the 15-bit exponent range (no float overflow/underflow)");
    for (int r = 0; r < 4; r++) {
        char tag[40];
        snprintf(tag, sizeof tag, "1e300*1e-8 %s", RCNAME[r]);
        f64(tag, RC24[r], 1e300, 1e-8, 2);
        snprintf(tag, sizeof tag, "1e-300/3 %s", RCNAME[r]);
        f64(tag, RC24[r], 1e-300, 3.0, 3);
    }
    puts("== RC on the narrowing m32 store");
    {
        static const double v[] = {1.0 / 3.0, 10.0 / 7.0, 0.1, 1.0e-45, 3.4028235677973366e38, -1.0 / 3.0};
        for (unsigned i = 0; i < sizeof v / sizeof v[0]; i++)
            for (int r = 0; r < 4; r++) {
                char tag[40];
                snprintf(tag, sizeof tag, "m32[%u] %s", i, RCNAME[r]);
                m32(tag, RC[r], v[i]);
            }
    }
    puts("== FRNDINT and FIST already honoured RC; keep them honest");
    for (int r = 0; r < 4; r++) {
        static const double v[] = {2.5, -2.5, 3.5, 0.5, -0.5, 1.4999999999999998};
        for (unsigned i = 0; i < sizeof v / sizeof v[0]; i++) {
            double o;
            int32_t iv;
            init();
            setcw(RC[r]);
            __asm__ volatile("fldl %1\n\tfrndint\n\tfstpl %0" : "=m"(o) : "m"(v[i]) : "memory");
            init();
            setcw(RC[r]);
            __asm__ volatile("fldl %1\n\tfistpl %0" : "=m"(iv) : "m"(v[i]) : "memory");
            printf("rnd %s v[%u] frndint=%.17g fistp=%d c1=%u\n", RCNAME[r], i, o, iv, (fsw() >> 9) & 1);
        }
    }
    return 0;
}
