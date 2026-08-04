// div/ldiv/lldiv/imaxdiv quotient+remainder including negatives. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>
#include <inttypes.h>

int main(void) {
    div_t a = div(-7, 2); // C truncates toward zero: q=-3 r=-1
    int d1 = a.quot == -3 && a.rem == -1;
    ldiv_t b = ldiv(17L, 5L);
    int d2 = b.quot == 3 && b.rem == 2;
    lldiv_t c = lldiv(-100LL, 7LL);
    int d3 = c.quot == -14 && c.rem == -2;
    imaxdiv_t e = imaxdiv((intmax_t)1000000000007LL, 3);
    int d4 = e.quot == 333333333335LL && e.rem == 2;
    printf("div_family d1=%d d2=%d d3=%d d4=%d\n", d1, d2, d3, d4);
    return 0;
}
