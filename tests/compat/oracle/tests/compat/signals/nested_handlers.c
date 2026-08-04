// Nested signal handlers: while handling SIGUSR1 (which by default masks only itself), a SIGUSR2
// raised from inside is delivered immediately and its handler runs to completion before the
// SIGUSR1 handler resumes. Verify strict nesting via entry/exit ordering markers.
#include <signal.h>
#include <stdio.h>

static int trace[8];
static volatile sig_atomic_t ti;
static void put(int v) { if (ti < 8) trace[ti++] = v; }

static void h2(int s) { (void)s; put(2); /* USR2 body */ put(-2); }
static void h1(int s) {
    (void)s;
    put(1);        // enter USR1
    raise(SIGUSR2);// nested delivery
    put(-1);       // resume USR1 after USR2 fully done
}

int main(void) {
    struct sigaction sa = {0};
    sigemptyset(&sa.sa_mask);
    sa.sa_handler = h1; sigaction(SIGUSR1, &sa, NULL);
    sa.sa_handler = h2; sigaction(SIGUSR2, &sa, NULL);

    raise(SIGUSR1);
    // expected trace: 1, 2, -2, -1  (USR2 nested strictly inside USR1)
    int ok = ti == 4 && trace[0] == 1 && trace[1] == 2 && trace[2] == -2 && trace[3] == -1;
    printf("nested_handlers strict_nesting=%d\n", ok);
    return 0;
}
