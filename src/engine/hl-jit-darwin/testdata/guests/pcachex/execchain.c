// pcachex/execchain.c -- #373b exec re-key: the busybox `sh -c tar`-shaped chain in one binary.
// The driver epoch (argv[0] basename != "pcapplet") does a deterministic compute slice, then execve's
// ITSELF with argv[0] rewritten to "pcapplet" -- the same file identity but a different argv[0] basename,
// exactly a multicall applet switch. Under HL_JIT_PCACHE=1 the engine must (a) persist the DRIVER epoch's
// arena under the DRIVER key at the exec boundary (the exec-time save -- pre-#373 that epoch was never
// cached because the process never reaches exit in it), and (b) re-key + reload for the APPLET epoch, so
// the two epochs land in TWO distinct cache files and a save keyed to the wrong binary is impossible.
// Output is a single deterministic line from the applet epoch, so the case is golden-checkable cold or
// warm; the lifecycle lane (hl-tests/tests/pcache.rs) additionally asserts the two-file protocol + HITs.
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    const char *base = argv[0];
    for (const char *p = argv[0]; *p; p++)
        if (*p == '/') base = p + 1;
    if (strcmp(base, "pcapplet") == 0) { // applet epoch (post-exec)
        volatile unsigned long h = 5381;
        for (int i = 0; i < 200000; i++) h = h * 31 + (unsigned)i;
        (void)h;
        printf("pcache execchain applet ok argc=%d\n", argc);
        return 0;
    }
    // driver epoch: translate a deterministic slice, then switch applets via execve(self)
    volatile unsigned long h = 5381;
    for (int i = 0; i < 100000; i++) h = h * 33 + (unsigned)i;
    (void)h;
    char *na[] = {"pcapplet", NULL};
    execv(argv[0], na); // argv[0] is the absolute guest path (matrix + lifecycle lane pass it)
    printf("pcache execchain exec FAILED\n");
    return 1;
}
