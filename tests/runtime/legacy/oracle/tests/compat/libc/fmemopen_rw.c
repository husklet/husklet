// fmemopen read + write buffering; null-termination on write. Portable verdicts.
#include <stdio.h>
#include <string.h>

int main(void) {
    char src[] = "hello\nworld";
    FILE *r = fmemopen(src, strlen(src), "r");
    char l1[16] = {0}; fgets(l1, sizeof l1, r);
    int d1 = strcmp(l1, "hello\n") == 0;
    fclose(r);

    char buf[32]; memset(buf, 'Z', sizeof buf);
    FILE *w = fmemopen(buf, sizeof buf, "w");
    fputs("abc", w);
    fflush(w);
    int d2 = memcmp(buf, "abc", 3) == 0 && buf[3] == '\0'; // glibc null-terminates
    long pos = ftell(w);
    int d3 = pos == 3;
    fclose(w);
    printf("fmemopen d1=%d d2=%d d3=%d\n", d1, d2, d3);
    return 0;
}
