#include <stdio.h>

static long f(long n) {
    return n < 2 ? n : f(n - 1) + f(n - 2);
}

int main(void) {
    printf("FIB %ld\n", f(40));
    return 0;
}
