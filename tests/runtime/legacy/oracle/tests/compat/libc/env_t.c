// setenv/getenv/putenv/unsetenv/overwrite semantics. Portable verdicts.
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(void) {
    setenv("HL_TEST_A", "one", 1);
    int d1 = strcmp(getenv("HL_TEST_A"), "one") == 0;
    setenv("HL_TEST_A", "two", 0); // no overwrite
    int d2 = strcmp(getenv("HL_TEST_A"), "one") == 0;
    setenv("HL_TEST_A", "three", 1); // overwrite
    int d3 = strcmp(getenv("HL_TEST_A"), "three") == 0;
    static char kv[] = "HL_TEST_B=bee";
    putenv(kv);
    int d4 = strcmp(getenv("HL_TEST_B"), "bee") == 0;
    unsetenv("HL_TEST_A");
    int d5 = getenv("HL_TEST_A") == NULL;
    int d6 = getenv("HL_NOPE_XYZ") == NULL;
    printf("env d1=%d d2=%d d3=%d d4=%d d5=%d d6=%d\n", d1, d2, d3, d4, d5, d6);
    return 0;
}
