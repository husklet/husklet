#include <stdio.h>
#include <signal.h>
#include <setjmp.h>
static sigjmp_buf jb;

static void h(int s) {
    (void)s;
    siglongjmp(jb, 1);
}

int main() {
    signal(SIGSEGV, h);
    if (sigsetjmp(jb, 1) == 0) {
        volatile int *p = 0;
        *p = 1;
        printf("NOFAULT\n");
    } else
        printf("RECOVERED\n");
    return 0;
}
