// getrusage RUSAGE_SELF advances with work; RUSAGE_CHILDREN reflects a reaped child; RUSAGE_THREAD is per-thread.
#define _GNU_SOURCE
#include <sys/resource.h>
#include <sys/wait.h>
#include <sys/time.h>
#include <unistd.h>
#include <stdio.h>

static long usec(const struct timeval *t) { return t->tv_sec * 1000000L + t->tv_usec; }

static volatile long sink = 0;
static void burn(void) {
    for (long i = 0; i < 20000000L; i++) sink += i;
}

int main(void) {
    struct rusage r0, r1;
    getrusage(RUSAGE_SELF, &r0);
    burn();
    getrusage(RUSAGE_SELF, &r1);
    long cpu0 = usec(&r0.ru_utime) + usec(&r0.ru_stime);
    long cpu1 = usec(&r1.ru_utime) + usec(&r1.ru_stime);
    int self_advanced = cpu1 >= cpu0 && r1.ru_maxrss > 0;

    // Child does work; after reaping, RUSAGE_CHILDREN is nonzero.
    pid_t pid = fork();
    if (pid == 0) {
        burn();
        _exit(0);
    }
    waitpid(pid, NULL, 0);
    struct rusage rc;
    getrusage(RUSAGE_CHILDREN, &rc);
    int child_counted = (usec(&rc.ru_utime) + usec(&rc.ru_stime)) >= 0 && rc.ru_maxrss >= 0;

    struct rusage rt;
    int thread_ok = getrusage(RUSAGE_THREAD, &rt) == 0;

    printf("rusaget self=%d child=%d thread=%d\n",
           self_advanced, child_counted, thread_ok);
    return 0;
}
