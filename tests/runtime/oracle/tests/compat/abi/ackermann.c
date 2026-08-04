/* Ackermann + mutual recursion (is_even/is_odd) + a hand-rolled deep tail-recursive sum. Stresses
   call/return, frame setup, and the translator's handling of many nested activation records.
   Deterministic, arch-neutral. */
#include <stdio.h>

static unsigned ack(unsigned m, unsigned n) {
    if (m == 0) return n + 1;
    if (n == 0) return ack(m - 1, 1);
    return ack(m - 1, ack(m, n - 1));
}

static int is_odd(unsigned n);
static int is_even(unsigned n) { return n == 0 ? 1 : is_odd(n - 1); }
static int is_odd(unsigned n) { return n == 0 ? 0 : is_even(n - 1); }

static unsigned long tsum(unsigned n, unsigned long acc) {
    if (n == 0) return acc;
    return tsum(n - 1, acc + n);   /* tail call */
}

int main(void) {
    printf("ack(2,3)=%u ack(3,3)=%u ack(3,5)=%u\n", ack(2, 3), ack(3, 3), ack(3, 5));
    printf("even(1000)=%d odd(1001)=%d even(9999)=%d\n",
           is_even(1000), is_odd(1001), is_even(9999));
    printf("tsum(100000)=%lu\n", tsum(100000, 0));
    return 0;
}
