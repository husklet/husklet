// gettimeofday: valid microseconds (<1e6), agrees with clock_gettime(REALTIME) to within a second,
// and is non-decreasing across a short sleep. Portable POSIX -> golden verdict on every engine.
#include <stdio.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    struct timeval a;
    gettimeofday(&a, NULL);
    int usec_ok = a.tv_usec >= 0 && a.tv_usec < 1000000;

    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    long da = a.tv_sec - ts.tv_sec;
    int agrees = da >= -1 && da <= 1;

    usleep(20000);
    struct timeval b;
    gettimeofday(&b, NULL);
    long long ua = a.tv_sec * 1000000LL + a.tv_usec;
    long long ub = b.tv_sec * 1000000LL + b.tv_usec;
    int monotonic = ub >= ua;
    int positive = a.tv_sec > 1000000000L;      // clearly a real wall-clock epoch
    printf("gettimeofday usec=%d agrees=%d mono=%d positive=%d\n", usec_ok, agrees, monotonic, positive);
    return 0;
}
