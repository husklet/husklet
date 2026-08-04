// alarm() returns the number of seconds remaining on any previously scheduled alarm. Setting a new
// alarm returns the previous remaining; alarm(0) cancels and returns the remaining. SIGALRM fires
// when the alarm elapses.
#define _GNU_SOURCE
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static volatile sig_atomic_t fired;
static void h(int s) { (void)s; fired = 1; }

int main(void) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGALRM, &sa, NULL);

    // No prior alarm -> returns 0.
    unsigned prev0 = alarm(100);
    int fresh = prev0 == 0;

    // Reschedule while ~100s pending -> previous remaining is ~100 (99 or 100).
    unsigned prev1 = alarm(50);
    int reported = prev1 == 100 || prev1 == 99;

    // Cancel -> returns remaining (~50), and no signal fires afterwards.
    unsigned prev2 = alarm(0);
    int cancelled = prev2 == 50 || prev2 == 49;

    // A short real alarm fires SIGALRM.
    fired = 0;
    alarm(1);
    for (int i = 0; i < 30 && !fired; i++) usleep(100 * 1000);
    int delivered = fired == 1;

    printf("alarm fresh=%d reported=%d cancelled=%d delivered=%d\n", fresh, reported, cancelled,
           delivered);
    return 0;
}
