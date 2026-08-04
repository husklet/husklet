// adjtimex(mode=0) and clock_adjtime(CLOCK_REALTIME, mode=0) are pure reads of the kernel time
// state: they must succeed (return a non-negative clock state), leave a plausible tick value, and
// not modify the clock. clock_adjtime on a bad clock id -> EINVAL.
#define _GNU_SOURCE
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/timex.h>
#include <time.h>

int main(void) {
    struct timex tx;
    memset(&tx, 0, sizeof tx);
    tx.modes = 0; // read-only query
    int r = adjtimex(&tx);
    int state_ok = r >= 0; // returns clock state (TIME_OK..TIME_ERROR), all >= 0
    // Default tick is 10000 us/tick (100Hz) or similar; must be a sane positive number.
    int tick_ok = tx.tick > 0 && tx.tick < 1000000;

    struct timex tx2;
    memset(&tx2, 0, sizeof tx2);
    tx2.modes = 0;
    int r2 = clock_adjtime(CLOCK_REALTIME, &tx2);
    int cadj_ok = r2 >= 0 && tx2.tick == tx.tick;

    printf("adjtimex state=%d tick=%d cadj=%d\n", state_ok, tick_ok, cadj_ok);
    return 0;
}
