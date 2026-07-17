// Regression: sched_getaffinity(tid) for a NON-main guest thread must not spuriously return ESRCH.
//
// glibc's pthread_getattr_np(pthread_self()) — called by HotSpot's os::current_stack_region on every
// thread it brings up (the JVM's very first bootstrap) and by Go/others — FIRST issues
// sched_getaffinity(pd->tid, ...). hl used to validate that pid with the HOST kill(pid, 0): a guest
// thread's tid is a hl-internal id (g_next_tid, base 1000) that is not a host pid, so kill() returned
// ESRCH and hl failed the syscall -> pthread_getattr_np returned ESRCH(3) -> "java -version" aborted
// with `fatal error: pthread_getattr_np failed with error = 3`. The fix resolves a guest tid against the
// live-thread registry (thread_tid_alive) before any host probe.
//
// This exercises the exact mechanism from a spawned thread: sched_getaffinity on our own tid (raw + the
// glibc wrapper), on pid 0 (self), and via pthread_getattr_np (the JVM path). Output is deterministic
// (booleans only — no addresses/cpu-mask widths) so it golden-checks byte-for-byte on every engine.
#define _GNU_SOURCE
#include <pthread.h>
#include <sched.h>
#include <stdio.h>
#include <errno.h>
#include <sys/syscall.h>
#include <unistd.h>

static int g_tid_ok, g_self_ok, g_wrap_ok, g_getattr_ok;

static void *worker(void *arg) {
    (void)arg;
    pid_t tid = (pid_t)syscall(SYS_gettid); // our own (non-main) guest tid
    cpu_set_t set;
    // raw syscall on our tid: the path hl mis-validated with kill(guest_tid,0) -> ESRCH
    long r = syscall(SYS_sched_getaffinity, (long)tid, sizeof set, &set);
    g_tid_ok = (r >= 0);
    // pid 0 == the caller (always valid, even pre-fix)
    g_self_ok = (sched_getaffinity(0, sizeof set, &set) == 0);
    // glibc wrapper targeting our tid explicitly
    g_wrap_ok = (sched_getaffinity(tid, sizeof set, &set) == 0);
    // the JVM/HotSpot path: pthread_getattr_np -> sched_getaffinity(pd->tid) internally
    pthread_attr_t at;
    g_getattr_ok = (pthread_getattr_np(pthread_self(), &at) == 0);
    if (g_getattr_ok) pthread_attr_destroy(&at);
    return NULL;
}

int main(void) {
    pthread_t t;
    if (pthread_create(&t, NULL, worker, NULL) != 0) {
        printf("getaffinity-tid create-failed\n");
        return 1;
    }
    pthread_join(t, NULL);
    printf("getaffinity-tid tid=%d self=%d wrap=%d getattr=%d\n",
           g_tid_ok, g_self_ok, g_wrap_ok, g_getattr_ok);
    return 0;
}
