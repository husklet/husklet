// x87 control word: fnstcw must report the LIVE control word (not a hardcoded 0x037f), fldcw must take
// effect, and fist/fistp must round per the control word's RC field -- x87 defaults to round-to-NEAREST-even,
// not truncation. The old engine hardcoded fnstcw=0x037f, ignored fldcw, and emitted a bare FCVTZS (truncate)
// so fistp(2.7) gave 2 instead of 3 and RC changes had no effect. Oracle-diffed vs qemu (control words +
// converted integers across all four rounding modes).
#include <stdio.h>

static unsigned short getcw(void) {
    unsigned short c;
    __asm__ volatile("fnstcw %0" : "=m"(c));
    return c;
}
static void setcw(unsigned short c) { __asm__ volatile("fldcw %0" ::"m"(c)); }
static int fistp(double x) {
    int r;
    __asm__ volatile("fldl %1\n\tfistpl %0\n\t" : "=m"(r) : "m"(x) : "memory");
    return r;
}

// RC field = control-word bits [11:10]: 0=nearest, 1=down(-inf), 2=up(+inf), 3=truncate(0).
static int conv(unsigned short def, int rc, double x) {
    setcw((unsigned short)((def & ~0x0c00) | (rc << 10)));
    int r = fistp(x);
    setcw(def);
    return r;
}

int main(void) {
    unsigned short def = getcw();
    // control-word round-trip: fnstcw must reflect what fldcw loaded
    setcw((unsigned short)((def & ~0x0c00) | 0x0400));
    unsigned short got = getcw();
    setcw(def);
    printf("cw_def=%04x cw_after_down=%04x\n", def, got);
    // 2.7 and -2.7 across nearest/down/up/trunc
    printf("near  2.7=%d -2.7=%d\n", conv(def, 0, 2.7), conv(def, 0, -2.7));
    printf("down  2.7=%d -2.7=%d\n", conv(def, 1, 2.7), conv(def, 1, -2.7));
    printf("up    2.7=%d -2.7=%d\n", conv(def, 2, 2.7), conv(def, 2, -2.7));
    printf("trunc 2.7=%d -2.7=%d\n", conv(def, 3, 2.7), conv(def, 3, -2.7));
    // nearest-even tie handling (2.5 -> 2, 3.5 -> 4)
    printf("ties  2.5=%d 3.5=%d\n", conv(def, 0, 2.5), conv(def, 0, 3.5));
    return 0;
}
