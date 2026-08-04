// nanosleep is NEVER restarted by SA_RESTART; an interrupting signal makes it return -1/EINTR
// with the unslept remainder in *rem. Verify remainder is positive and less than the request.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <sys/time.h>
#include <time.h>

static volatile sig_atomic_t ran;
static void h(int s) { (void)s; ran++; }

int main(void) {
    struct sigaction sa = {0};
    sa.sa_handler = h;
    sigemptyset(&sa.sa_mask);
    sa.sa_flags = SA_RESTART; // ignored by nanosleep
    sigaction(SIGALRM, &sa, NULL);

    struct itimerval it = {{0, 0}, {0, 50 * 1000}}; // fire at 50ms
    setitimer(ITIMER_REAL, &it, NULL);

    struct timespec req = {2, 0}, rem = {0, 0};
    errno = 0;
    int r = nanosleep(&req, &rem);
    int eintr = r == -1 && errno == EINTR;
    // fired at ~50ms into a 2s sleep, so the remainder is ~1.95s: positive and below 2s
    int rem_ok = rem.tv_sec < 2;
    int rem_pos = (rem.tv_sec > 0 || rem.tv_nsec > 0);
    printf("eintr_nanosleep handler=%d eintr=%d rem_less=%d rem_positive=%d\n",
           ran == 1, eintr, rem_ok, rem_pos);
    return 0;
}
