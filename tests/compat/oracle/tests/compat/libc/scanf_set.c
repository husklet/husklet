// sscanf scanset, assignment suppression, %n count, width limit. Portable verdicts.
#include <stdio.h>
#include <string.h>

int main(void) {
    char word[16] = {0}; int n = 0;
    int got = sscanf("hello123", "%[a-z]%n", word, &n);
    int d1 = got == 1 && strcmp(word, "hello") == 0 && n == 5;
    int a, b;
    int g2 = sscanf("10 skip 20", "%d %*s %d", &a, &b);
    int d2 = g2 == 2 && a == 10 && b == 20;
    char four[8] = {0};
    sscanf("abcdefgh", "%4c", four); four[4] = 0;
    int d3 = strcmp(four, "abcd") == 0;
    char neg[16] = {0};
    sscanf("abc.def", "%[^.]", neg);
    int d4 = strcmp(neg, "abc") == 0;
    printf("scanf_set d1=%d d2=%d d3=%d d4=%d\n", d1, d2, d3, d4);
    return 0;
}
