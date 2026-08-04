// ungetc pushback interaction with fscanf/fgetc and EOF clearing. Portable verdicts.
#include <stdio.h>
#include <string.h>

int main(void) {
    FILE *f = tmpfile();
    fputs("42rest", f);
    rewind(f);
    int c = fgetc(f); // '4'
    ungetc(c, f);
    int n = 0; fscanf(f, "%d", &n);
    int d1 = n == 42;
    // Push a non-read char then read it.
    ungetc('Z', f);
    int d2 = fgetc(f) == 'Z';
    int d3 = fgetc(f) == 'r';
    // EOF then ungetc clears EOF.
    char buf[16]; while (fgetc(f) != EOF) {}
    int d4 = feof(f) != 0;
    ungetc('x', f);
    int d5 = feof(f) == 0 && fgetc(f) == 'x';
    fclose(f);
    printf("ungetc_multi d1=%d d2=%d d3=%d d4=%d d5=%d\n", d1, d2, d3, d4, d5);
    return 0;
}
