#include <stdio.h>
#include <time.h>

int main() {
    struct timespec a, b;
    clock_gettime(CLOCK_MONOTONIC, &a);
    for (volatile long i = 0; i < 1000000; i++)
        ;
    clock_gettime(CLOCK_MONOTONIC, &b);
    printf("CLOCK=%s\n", (b.tv_sec > a.tv_sec || (b.tv_sec == a.tv_sec && b.tv_nsec >= a.tv_nsec)) ? "ok" : "no");
    return 0;
}
