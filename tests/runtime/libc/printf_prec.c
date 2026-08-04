// printf precision/width/flag edge cases with snprintf capture. Portable verdicts.
#include <stdio.h>
#include <string.h>

int main(void) {
    char b[64];
    snprintf(b, sizeof b, "%.10d", 42);
    int d1 = strcmp(b, "0000000042") == 0;
    snprintf(b, sizeof b, "%+08.2f", 3.5);
    int d2 = strcmp(b, "+0003.50") == 0;
    snprintf(b, sizeof b, "%-6d|", 12);
    int d3 = strcmp(b, "12    |") == 0;
    snprintf(b, sizeof b, "%.0f", 2.5); // round half to even -> 2
    int d4 = strcmp(b, "2") == 0;
    snprintf(b, sizeof b, "%.0f", 3.5); // -> 4
    int d5 = strcmp(b, "4") == 0;
    snprintf(b, sizeof b, "%5.3s", "abcdef"); // width 5, precision 3
    int d6 = strcmp(b, "  abc") == 0;
    snprintf(b, sizeof b, "% d", 7);
    int d7 = strcmp(b, " 7") == 0;
    printf("printf_prec d1=%d d2=%d d3=%d d4=%d d5=%d d6=%d d7=%d\n", d1, d2, d3, d4, d5, d6, d7);
    return 0;
}
