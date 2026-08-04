// wordexp: field splitting, tilde/variable expansion, WRDE_NOCMD rejects command substitution.
#include <wordexp.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

int main(void) {
    setenv("HL_WE", "alpha beta", 1);
    setenv("HOME", "/home/hl", 1);

    wordexp_t we;
    int rc = wordexp("one $HL_WE two", &we, 0);
    // $HL_WE undergoes field splitting -> "one","alpha","beta","two".
    int split_ok = rc == 0 && we.we_wordc == 4 &&
                   strcmp(we.we_wordv[0], "one") == 0 &&
                   strcmp(we.we_wordv[1], "alpha") == 0 &&
                   strcmp(we.we_wordv[2], "beta") == 0 &&
                   strcmp(we.we_wordv[3], "two") == 0;
    if (rc == 0) wordfree(&we);

    wordexp_t we2;
    int rc2 = wordexp("~/x", &we2, 0);
    int tilde_ok = rc2 == 0 && we2.we_wordc == 1 && strcmp(we2.we_wordv[0], "/home/hl/x") == 0;
    if (rc2 == 0) wordfree(&we2);

    // WRDE_NOCMD rejects `$(...)` without running a shell.
    wordexp_t we3;
    int rc3 = wordexp("$(echo hi)", &we3, WRDE_NOCMD);
    int nocmd = rc3 == WRDE_CMDSUB;
    if (rc3 == 0) wordfree(&we3);

    printf("wordexpt split=%d tilde=%d nocmd=%d\n", split_ok, tilde_ok, nocmd);
    return 0;
}
