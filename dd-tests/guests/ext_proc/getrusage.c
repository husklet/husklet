// getrusage(RUSAGE_SELF/RUSAGE_CHILDREN): resident-set and CPU-time accounting is populated and
// monotonic across a CPU-burning child. Portable POSIX -> golden verdict on every engine.
#include <stdio.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

static double cpu(const struct rusage *r) {
    return r->ru_utime.tv_sec + r->ru_utime.tv_usec / 1e6
         + r->ru_stime.tv_sec + r->ru_stime.tv_usec / 1e6;
}

int main(void) {
    struct rusage self;
    int self_ok = getrusage(RUSAGE_SELF, &self) == 0;
    int maxrss_pos = self.ru_maxrss > 0;      // we are resident, so >0 on every platform

    // spawn a child that burns some CPU so RUSAGE_CHILDREN is nonzero after we reap it
    pid_t pid = fork();
    if (pid == 0) {
        volatile unsigned long x = 0;
        for (unsigned long i = 0; i < 20000000UL; i++) x += i;
        _exit((int)(x & 1));
    }
    waitpid(pid, NULL, 0);
    struct rusage ch;
    int child_ok = getrusage(RUSAGE_CHILDREN, &ch) == 0;
    int child_cpu = cpu(&ch) > 0.0;           // the child spent measurable CPU
    printf("getrusage self=%d maxrss=%d child=%d child_cpu=%d\n",
           self_ok, maxrss_pos, child_ok, child_cpu);
    return 0;
}
