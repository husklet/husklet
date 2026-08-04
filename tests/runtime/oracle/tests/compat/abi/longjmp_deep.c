/* setjmp/longjmp unwinding across several stack frames, with volatile locals that must survive the
   non-local jump and a longjmp value of 0 normalized to 1. Exercises the translator's saved-context
   restore (callee-saved regs + SP). Arch-neutral. */
#include <stdio.h>
#include <setjmp.h>

static jmp_buf env;
static int depth_reached;

static void level3(int x) {
    depth_reached = 3;
    longjmp(env, x);           /* jump value 0 must arrive as 1 */
}
static void level2(int x) { depth_reached = 2; level3(x); }
static void level1(int x) { depth_reached = 1; level2(x); }

int main(void) {
    for (int req = 0; req <= 3; req++) {
        volatile int marker = 1000 + req;
        int rc = setjmp(env);
        if (rc == 0) {
            level1(req);
        } else {
            printf("req=%d rc=%d marker=%d depth=%d\n", req, rc, marker, depth_reached);
        }
    }
    /* second buffer, nested setjmp */
    jmp_buf e2;
    volatile long sum = 0;
    if (setjmp(e2) == 0) {
        sum += 7;
        longjmp(e2, 42);
    } else {
        sum += 100;
        printf("sum=%ld\n", sum);
    }
    return 0;
}
