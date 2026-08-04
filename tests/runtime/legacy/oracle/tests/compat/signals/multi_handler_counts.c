// Distinct handlers for several signals each fire exactly once per raise and route to the correct
// handler. Confirms per-signal disposition dispatch.
#include <signal.h>
#include <stdio.h>

static volatile sig_atomic_t c1, c2, ct, ch;
static void h1(int s) { (void)s; c1++; }
static void h2(int s) { (void)s; c2++; }
static void ht(int s) { (void)s; ct++; }
static void hh(int s) { (void)s; ch++; }

int main(void) {
    signal(SIGUSR1, h1);
    signal(SIGUSR2, h2);
    signal(SIGTERM, ht);
    signal(SIGHUP, hh);

    raise(SIGUSR1); raise(SIGUSR1);
    raise(SIGUSR2);
    raise(SIGTERM); raise(SIGTERM); raise(SIGTERM);
    raise(SIGHUP);

    printf("multi_handler usr1=%d usr2=%d term=%d hup=%d\n", c1, c2, ct, ch);
    return 0;
}
