// memccpy / memmem / memrchr / mempcpy GNU/POSIX memory ops. Portable verdicts.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>

int main(void) {
    char dst[16] = {0};
    void *stop = memccpy(dst, "abcXdef", 'X', sizeof dst);
    int d1 = strncmp(dst, "abcX", 4) == 0 && (char *)stop == dst + 4;

    const char *hay = "the quick brown fox";
    void *m = memmem(hay, strlen(hay), "brown", 5);
    int d2 = m == hay + 10;
    int d3 = memmem(hay, strlen(hay), "zzz", 3) == NULL;

    const char *s = "aXbXc";
    int d4 = memrchr(s, 'X', 5) == s + 3;

    char buf[8];
    char *end = mempcpy(buf, "hi", 2);
    int d5 = end == buf + 2 && buf[0] == 'h';
    printf("mem_extra d1=%d d2=%d d3=%d d4=%d d5=%d\n", d1, d2, d3, d4, d5);
    return 0;
}
