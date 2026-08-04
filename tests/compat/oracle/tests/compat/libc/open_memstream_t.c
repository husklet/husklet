// open_memstream grows buffer and reports size on flush. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    char *buf = NULL; size_t sz = 0;
    FILE *m = open_memstream(&buf, &sz);
    for (int i = 0; i < 100; i++) fprintf(m, "%02d", i);
    fflush(m);
    int d1 = sz == 200;
    int d2 = strncmp(buf, "000102", 6) == 0;
    int d3 = strncmp(buf + 196, "9899", 4) == 0;
    fclose(m);
    int d4 = strlen(buf) == 200;
    free(buf);
    printf("open_memstream d1=%d d2=%d d3=%d d4=%d\n", d1, d2, d3, d4);
    return 0;
}
