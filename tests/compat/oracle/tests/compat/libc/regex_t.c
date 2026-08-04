// POSIX regcomp/regexec with subexpression capture. Portable verdicts.
#include <stdio.h>
#include <regex.h>
#include <string.h>

int main(void) {
    regex_t re;
    int c = regcomp(&re, "([0-9]+)-([0-9]+)", REG_EXTENDED);
    int d1 = c == 0;
    regmatch_t m[3];
    int e = regexec(&re, "id 42-99 end", 3, m, 0);
    int d2 = e == 0;
    int d3 = m[0].rm_so == 3 && m[0].rm_eo == 8;
    int d4 = m[1].rm_so == 3 && m[1].rm_eo == 5; // "42"
    int d5 = m[2].rm_so == 6 && m[2].rm_eo == 8; // "99"
    int d6 = regexec(&re, "no digits", 0, NULL, 0) == REG_NOMATCH;
    regfree(&re);
    printf("regex d1=%d d2=%d d3=%d d4=%d d5=%d d6=%d\n", d1, d2, d3, d4, d5, d6);
    return 0;
}
