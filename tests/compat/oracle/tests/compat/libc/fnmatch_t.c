// fnmatch glob-style matching incl. PATHNAME/PERIOD flags. Portable verdicts.
#include <stdio.h>
#include <fnmatch.h>

int main(void) {
    int d1 = fnmatch("*.c", "main.c", 0) == 0;
    int d2 = fnmatch("*.c", "main.h", 0) == FNM_NOMATCH;
    int d3 = fnmatch("a?c", "abc", 0) == 0;
    int d4 = fnmatch("[a-c]x", "bx", 0) == 0;
    int d5 = fnmatch("*/foo", "a/b/foo", FNM_PATHNAME) == FNM_NOMATCH; // * won't cross '/'
    int d6 = fnmatch("*bar", ".bar", FNM_PERIOD) == FNM_NOMATCH; // leading period not matched
    printf("fnmatch d1=%d d2=%d d3=%d d4=%d d5=%d d6=%d\n", d1, d2, d3, d4, d5, d6);
    return 0;
}
