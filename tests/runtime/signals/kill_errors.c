// kill() error and probe semantics: signal 0 to self returns 0 (liveness, no delivery); kill to a
// definitely-unused PID returns -1/ESRCH; an out-of-range signal number returns -1/EINVAL.
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <unistd.h>

int main(void) {
    errno = 0;
    int self0 = kill(getpid(), 0);
    int probe_ok = self0 == 0;

    // find a pid that does not exist: use a very high pid unlikely to be live
    errno = 0;
    int r = kill(0x3fffffff, SIGTERM);
    int esrch = r == -1 && errno == ESRCH;

    errno = 0;
    int r2 = kill(getpid(), 12345); // invalid signo
    int einval = r2 == -1 && errno == EINVAL;

    printf("kill_errors probe0=%d esrch=%d einval=%d\n", probe_ok, esrch, einval);
    return 0;
}
