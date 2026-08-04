// POSIX regex: extended vs basic, REG_ICASE, anchors, subexpression offsets, and BRE backreferences.
#include <regex.h>
#include <stdio.h>
#include <string.h>

int main(void) {
    regex_t re;
    regmatch_t m[4];

    // Extended regex with a captured group; check match offsets.
    regcomp(&re, "([0-9]+)-([0-9]+)", REG_EXTENDED);
    int e1 = regexec(&re, "ab 42-99 cd", 4, m, 0) == 0;
    int off_ok = e1 && m[0].rm_so == 3 && m[0].rm_eo == 8 &&
                 m[1].rm_so == 3 && m[1].rm_eo == 5 &&
                 m[2].rm_so == 6 && m[2].rm_eo == 8;
    regfree(&re);

    // REG_ICASE + anchors.
    regcomp(&re, "^hello$", REG_EXTENDED | REG_ICASE);
    int e2 = regexec(&re, "HELLO", 0, NULL, 0) == 0;
    int e2b = regexec(&re, "hello world", 0, NULL, 0) == REG_NOMATCH;
    regfree(&re);

    // REG_NOSUB compiles without capture support.
    regcomp(&re, "a+b", REG_EXTENDED | REG_NOSUB);
    int e3 = regexec(&re, "xaaab", 0, NULL, 0) == 0;
    regfree(&re);

    // Basic (BRE) backreference: \(.\)\1 matches a doubled char.
    regcomp(&re, "\\(.\\)\\1", 0);
    int e4 = regexec(&re, "abcc de", 0, NULL, 0) == 0;
    int e4b = regexec(&re, "abcd", 0, NULL, 0) == REG_NOMATCH;
    regfree(&re);

    // REG_NEWLINE: '.' doesn't cross newline; '$' matches before newline.
    regcomp(&re, "^b", REG_EXTENDED | REG_NEWLINE);
    int e5 = regexec(&re, "a\nbc", 0, NULL, 0) == 0;
    regfree(&re);

    printf("regexbr off=%d icase=%d nosub=%d backref=%d nobref=%d newline=%d\n",
           off_ok, e2 && e2b, e3, e4, e4b, e5);
    return 0;
}
