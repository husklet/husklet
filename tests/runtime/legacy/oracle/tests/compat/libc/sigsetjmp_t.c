// sigsetjmp/siglongjmp control transfer with value passing. Portable verdicts.
#include <stdio.h>
#include <setjmp.h>

static sigjmp_buf jb;

static void deep(int v) { siglongjmp(jb, v); }

int main(void) {
    volatile int hops = 0; int got = 0;
    int r = sigsetjmp(jb, 1);
    if (r == 0) { hops++; deep(7); }
    else { got = r; }
    int d1 = got == 7;
    int d2 = hops == 1; // body executed once before the jump
    printf("sigsetjmp d1=%d d2=%d\n", d1, d2);
    return 0;
}
