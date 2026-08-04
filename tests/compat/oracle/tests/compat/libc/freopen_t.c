// freopen redirects a stream to a tmpfile and back-reads. Portable verdicts.
#include <stdio.h>
#include <string.h>

int main(void) {
    FILE *f = tmpfile();
    fputs("first", f);
    fflush(f);
    // Reopen same stream in read mode from start.
    FILE *g = freopen(NULL, "r", f);
    int d1 = g != NULL;
    rewind(g);
    char buf[16] = {0}; fread(buf, 1, 5, g);
    int d2 = strcmp(buf, "first") == 0;
    fclose(g);
    printf("freopen d1=%d d2=%d\n", d1, d2);
    return 0;
}
