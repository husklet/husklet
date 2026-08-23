#include <stdio.h>

int main() {
    long s = 0;
    for (long i = 1; i <= 1000; i++)
        s += i;
    printf("CC=%ld\n", s);
    return 0;
}
