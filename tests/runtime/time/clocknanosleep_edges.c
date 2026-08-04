#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static pthread_t sleeper;
static volatile sig_atomic_t delivered;

static void interrupt_sleep(int signal_number) {
    (void)signal_number;
    delivered = 1;
}

static void *send_interrupt(void *opaque) {
    (void)opaque;
    usleep(20000);
    (void)pthread_kill(sleeper, SIGUSR1);
    return NULL;
}

static int absolute_sleep(clockid_t clock_id, long milliseconds) {
    struct timespec deadline;
    if (clock_gettime(clock_id, &deadline) != 0) return 0;
    deadline.tv_nsec += milliseconds * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) {
        deadline.tv_sec++;
        deadline.tv_nsec -= 1000000000L;
    }
    errno = 0;
    return syscall(SYS_clock_nanosleep, clock_id, TIMER_ABSTIME, &deadline, NULL) == 0;
}

static int raw_error(clockid_t clock_id, int flags, int expected) {
    struct timespec zero = {0, 0};
    errno = 0;
    return syscall(SYS_clock_nanosleep, clock_id, flags, &zero, NULL) == -1 && errno == expected;
}

int main(void) {
    struct sigaction action = {0};
    struct timespec deadline;
    struct timespec zero = {0, 0};
    pthread_t sender;
    int interrupted;

    action.sa_handler = interrupt_sleep;
    sigemptyset(&action.sa_mask);
    if (sigaction(SIGUSR1, &action, NULL) != 0) return 2;
    sleeper = pthread_self();
    if (clock_gettime(CLOCK_MONOTONIC, &deadline) != 0) return 3;
    deadline.tv_nsec += 200000000L;
    if (deadline.tv_nsec >= 1000000000L) {
        deadline.tv_sec++;
        deadline.tv_nsec -= 1000000000L;
    }
    if (pthread_create(&sender, NULL, send_interrupt, NULL) != 0) return 4;
    errno = 0;
    interrupted = syscall(SYS_clock_nanosleep, CLOCK_MONOTONIC, TIMER_ABSTIME, &deadline, NULL) == -1 &&
                  errno == EINTR;
    if (pthread_join(sender, NULL) != 0) return 5;

    errno = 0;
    int process_past = syscall(SYS_clock_nanosleep, CLOCK_PROCESS_CPUTIME_ID, TIMER_ABSTIME, &zero, NULL) == 0;
    printf("clocknanosleep-edges realtime=%d monotonic=%d interrupted=%d delivered=%d process-past=%d thread=%d raw=%d\n",
           absolute_sleep(CLOCK_REALTIME, 20), absolute_sleep(CLOCK_MONOTONIC, 20), interrupted,
           delivered != 0, process_past, raw_error(CLOCK_THREAD_CPUTIME_ID, TIMER_ABSTIME, EOPNOTSUPP),
           raw_error(CLOCK_MONOTONIC_RAW, TIMER_ABSTIME, EOPNOTSUPP));
    return 0;
}
