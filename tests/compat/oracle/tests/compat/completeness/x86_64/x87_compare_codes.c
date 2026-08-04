// x87 comparison condition codes, the whole 8x8 cross product over {1, 2, -1, +0, -0, +inf, -inf, NaN}.
// FCOM/FUCOM report C3:C2:C0 in the status word and differ only in which NaN raises #IA (FUCOM is quiet
// for a QNaN); FCOMI/FUCOMI write ZF/PF/CF in EFLAGS instead and leave the condition codes alone. Four
// encodings, four different flag destinations, one ordering -- and the corpus barely touched any of it.
// TOP is printed too, so a comparison that pops when it must not moves the whole line.
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static unsigned fsw(void) {
    unsigned short s;
    __asm__ volatile("fnstsw %0" : "=am"(s));
    return s;
}
static unsigned fcw(void) {
    unsigned short c;
    __asm__ volatile("fnstcw %0" : "=m"(c));
    return c;
}
static void setcw(unsigned short c) { __asm__ volatile("fldcw %0" : : "m"(c)); }
static void init(void) { __asm__ volatile("fninit"); }

#define C0 0x0100u
#define C1 0x0200u
#define C2 0x0400u
#define C3 0x4000u
#define CC (C0 | C1 | C2 | C3)
#define TOP 0x3800u

static void cc(const char *tag, unsigned s) {
    printf("%-22s fsw=%04x cc=%x top=%u exc=%02x\n", tag, s, (unsigned)(((s & C3) >> 11) | ((s & C2) >> 8) |
                                                                       ((s & C1) >> 8) | ((s & C0) >> 8)),
           (s & TOP) >> 11, s & 0x3f);
}

/* ---- FCOM / FUCOM / FCOMI condition codes over the whole cross product incl. NaN. */
static void compares(void) {
    static const double v[] = {1.0, 2.0, -1.0, 0.0, -0.0, 1.0 / 0.0, -1.0 / 0.0, 0.0 / 0.0};
    for (unsigned i = 0; i < sizeof v / sizeof v[0]; i++)
        for (unsigned j = 0; j < sizeof v / sizeof v[0]; j++) {
            char tag[48];
            init();
            __asm__ volatile("fldl %0\n\tfldl %1\n\tfxch\n\tfcom %%st(1)" : : "m"(v[i]), "m"(v[j]));
            snprintf(tag, sizeof tag, "fcom %u,%u", i, j);
            cc(tag, fsw());
            init();
            __asm__ volatile("fldl %0\n\tfldl %1\n\tfxch\n\tfucom %%st(1)" : : "m"(v[i]), "m"(v[j]));
            snprintf(tag, sizeof tag, "fucom %u,%u", i, j);
            cc(tag, fsw());
            init();
            unsigned long fl;
            __asm__ volatile("fldl %1\n\tfldl %2\n\tfxch\n\tfucomi %%st(1)\n\tpushfq\n\tpop %0"
                             : "=r"(fl)
                             : "m"(v[i]), "m"(v[j]));
            printf("fucomi %u,%u fl=%03lx fsw=%04x\n", i, j, fl & 0x8d5, fsw());
            init();
            __asm__ volatile("fldl %1\n\tfldl %2\n\tfxch\n\tfcomi %%st(1)\n\tpushfq\n\tpop %0"
                             : "=r"(fl)
                             : "m"(v[i]), "m"(v[j]));
            printf("fcomi  %u,%u fl=%03lx fsw=%04x\n", i, j, fl & 0x8d5, fsw());
        }
}

int main(void) {
    compares();
    return 0;
}
