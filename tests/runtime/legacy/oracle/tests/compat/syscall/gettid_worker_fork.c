// A process forked by a worker thread still starts with one process-leader
// thread. Its gettid() must equal its new getpid(), not the worker's old tid.
#define _GNU_SOURCE
#include <pthread.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int worker_ok;

static void *fork_from_worker(void *unused) {
    (void)unused;
    pid_t parent_pid = getpid();
    pid_t worker_tid = (pid_t)syscall(SYS_gettid);
    if (worker_tid == parent_pid) return NULL;

    pid_t child = fork();
    if (child == 0) {
        pid_t child_pid = getpid();
        pid_t child_tid = (pid_t)syscall(SYS_gettid);
        _exit(child_pid > 0 && child_pid != parent_pid && child_tid == child_pid && child_tid != worker_tid ? 0 : 1);
    }
    if (child < 0) return NULL;

    int status = 0;
    worker_ok = waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    return NULL;
}

int main(void) {
    pthread_t worker;
    int created = pthread_create(&worker, NULL, fork_from_worker, NULL) == 0;
    int joined = created && pthread_join(worker, NULL) == 0;
    int parent_ok = (pid_t)syscall(SYS_gettid) == getpid();
    printf("gettid_worker_fork parent=%d worker_child=%d\n", parent_ok, joined && worker_ok);
    return parent_ok && joined && worker_ok ? 0 : 1;
}
