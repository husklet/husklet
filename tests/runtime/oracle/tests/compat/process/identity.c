// pid/ppid/tid identity invariants across fork and threads:
//   - the main thread's gettid()==getpid();
//   - a fork child has getppid()==parent's pid and a distinct getpid();
//   - a pthread has the same getpid() as its creator but a distinct gettid().
// Reported as booleans/counts only (no raw ids).
#define _GNU_SOURCE
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static pid_t g_main_pid;
static atomic_int distinct_tids = 0;
static pid_t main_tid;

static void *worker(void *arg) {
    (void)arg;
    pid_t pid = getpid();
    pid_t tid = (pid_t)syscall(SYS_gettid);
    if (pid == g_main_pid && tid != main_tid) atomic_fetch_add(&distinct_tids, 1);
    return NULL;
}

int main(void) {
    g_main_pid = getpid();
    main_tid = (pid_t)syscall(SYS_gettid);
    int main_tid_eq_pid = main_tid == g_main_pid;

    // fork identity
    pid_t parent = getpid();
    pid_t c = fork();
    if (c == 0) {
        int ok = getppid() == parent && getpid() != parent;
        _exit(ok ? 0 : 1);
    }
    int st = 0; waitpid(c, &st, 0);
    int fork_identity = WIFEXITED(st) && WEXITSTATUS(st) == 0;

    // thread identity: 4 threads, each with same pid but distinct tid from main
    pthread_t t[4];
    for (int i = 0; i < 4; i++) pthread_create(&t[i], NULL, worker, NULL);
    for (int i = 0; i < 4; i++) pthread_join(t[i], NULL);
    int thread_identity = atomic_load(&distinct_tids) == 4;

    printf("identity main_tid_eq_pid=%d fork_identity=%d thread_identity=%d\n",
           main_tid_eq_pid, fork_identity, thread_identity);
    return 0;
}
