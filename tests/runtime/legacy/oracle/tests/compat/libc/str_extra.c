// stpcpy/stpncpy/strchrnul/strcasestr/strcasecmp/strncasecmp/strverscmp. Portable verdicts.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>

int main(void) {
    char b[16];
    char *e = stpcpy(b, "hello");
    int d1 = e == b + 5 && *e == '\0';
    char c[8]; char *e2 = stpncpy(c, "ab", 5); // pads to 5 with NUL, returns c+2
    int d2 = e2 == c + 2 && c[2] == 0 && c[4] == 0;
    const char *s = "abcdef";
    int d3 = strchrnul(s, 'c') == s + 2 && strchrnul(s, 'z') == s + 6;
    int d4 = strcasestr("Hello World", "world") != NULL;
    int d5 = strcasecmp("ABC", "abc") == 0 && strncasecmp("ABCx", "abcy", 3) == 0;
    int d6 = strverscmp("item9", "item10") < 0; // numeric-aware ordering
    printf("str_extra d1=%d d2=%d d3=%d d4=%d d5=%d d6=%d\n", d1, d2, d3, d4, d5, d6);
    return 0;
}
