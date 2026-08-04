// Thread-group teardown and exit-status encoding across the exit paths that
// shells, supervisors, and test harnesses depend on. Every child is driven by a
// deterministic pipe handshake (no sleeps) so the whole-process outcome is
// reproducible. The native aarch64 run is the behavioral oracle; both production
// engines must byte-match the reviewed golden.
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int announce_fd;
static void announce(char tag) { (void)write(announce_fd, &tag, 1); }

// exit_group(2) from a worker terminates the whole thread group with that code,
// even while sibling threads are blocked inside real host syscalls.
static int block_pipe[2];
static void *worker_block_read(void *arg) {
    (void)arg;
    announce('r');
    char scratch[16];
    (void)read(block_pipe[0], scratch, sizeof scratch); // blocks forever
    return NULL;
}
static void *worker_block_pause(void *arg) {
    (void)arg;
    announce('p');
    pause(); // blocks forever
    return NULL;
}
static void *worker_exit_group(void *arg) {
    (void)arg;
    announce('g');
    syscall(SYS_exit_group, 55);
    return NULL; // unreached
}
static void test_exit_group_teardown(void) {
    if (pipe(block_pipe) != 0) return;
    int ready[2];
    if (pipe(ready) != 0) return;
    announce_fd = ready[1];
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) {
        pthread_t a, b, c;
        pthread_create(&a, NULL, worker_block_read, NULL);
        pthread_create(&b, NULL, worker_block_pause, NULL);
        pthread_create(&c, NULL, worker_exit_group, NULL);
        for (;;) pause();
        _exit(1);
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("exit_group over blocked siblings: exited=%d code==55=%d\n",
           WIFEXITED(status), WIFEXITED(status) && WEXITSTATUS(status) == 55);
    close(block_pipe[0]);
    close(block_pipe[1]);
    close(ready[0]);
    close(ready[1]);
}

// A raw SYS_exit terminates only the calling thread; the rest of the process
// keeps running and can still make progress and exit on its own terms.
static int self_pipe[2];
static void *worker_self_exit(void *arg) {
    (void)arg;
    (void)write(self_pipe[1], "x", 1);
    syscall(SYS_exit, 0); // per-thread exit
    return NULL;
}
static void test_thread_self_exit(void) {
    if (pipe(self_pipe) != 0) return;
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) {
        pthread_t t;
        pthread_create(&t, NULL, worker_self_exit, NULL);
        char scratch;
        (void)read(self_pipe[0], &scratch, 1); // worker announced before exiting
        syscall(SYS_exit_group, 6);            // main still alive, chooses the code
        _exit(1);
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("thread self-exit leaves process alive: code==6=%d\n",
           WIFEXITED(status) && WEXITSTATUS(status) == 6);
    close(self_pipe[0]);
    close(self_pipe[1]);
}

// pthread_exit() from main keeps the process alive until the LAST thread exits;
// a worker that outlives main still reaches its return and the process exits 0.
static int last_pipe[2];
static void *worker_outlive_main(void *arg) {
    (void)arg;
    char go;
    (void)read(last_pipe[0], &go, 1); // proceed only after main has left
    return NULL;                       // last thread returns -> process exits 0
}
static void test_pthread_exit_last_thread(void) {
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) {
        if (pipe(last_pipe) != 0) _exit(2);
        pthread_t t;
        pthread_create(&t, NULL, worker_outlive_main, NULL);
        (void)write(last_pipe[1], "g", 1);
        pthread_exit(NULL); // main gone; process must persist until worker returns
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("pthread_exit main, worker is last thread: exited=%d code==0=%d\n",
           WIFEXITED(status), WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

// A fatal fault in one thread takes down the entire thread group with the
// terminating signal.
static void *worker_segv(void *arg) {
    (void)arg;
    volatile int *p = 0;
    *p = 1;
    return NULL;
}
static void test_thread_fault_group(void) {
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) {
        pthread_t t;
        pthread_create(&t, NULL, worker_segv, NULL);
        for (;;) pause();
        _exit(1);
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("thread SIGSEGV terminates group: signaled=%d sig==SEGV=%d\n",
           WIFSIGNALED(status), WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV);
}

// glibc exit()/return-from-main routes through exit_group, so a running worker
// is killed and the chosen code wins.
static void *worker_spin(void *arg) {
    (void)arg;
    for (;;) pause();
    return NULL;
}
static int spin_ready[2];
static void *worker_spin_announce(void *arg) {
    (void)write(spin_ready[1], "s", 1);
    return worker_spin(arg);
}
static void test_exit_kills_workers(void) {
    if (pipe(spin_ready) != 0) return;
    fflush(stdout);
    pid_t child = fork();
    if (child == 0) {
        pthread_t t;
        pthread_create(&t, NULL, worker_spin_announce, NULL);
        char s;
        (void)read(spin_ready[0], &s, 1); // worker is running
        exit(4);                          // exit_group under the hood
    }
    int status = 0;
    waitpid(child, &status, 0);
    printf("exit() kills running worker: exited=%d code==4=%d\n",
           WIFEXITED(status), WIFEXITED(status) && WEXITSTATUS(status) == 4);
    close(spin_ready[0]);
    close(spin_ready[1]);
}

// abort() delivers SIGABRT; exit status keeps only the low 8 bits of the code.
static void test_status_encoding(void) {
    fflush(stdout);
    pid_t a = fork();
    if (a == 0) abort();
    int status = 0;
    waitpid(a, &status, 0);
    int abort_ok = WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT;

    fflush(stdout);
    pid_t b = fork();
    if (b == 0) _exit(256 + 5);
    waitpid(b, &status, 0);
    int low_ok = WIFEXITED(status) && WEXITSTATUS(status) == 5;
    printf("abort SIGABRT=%d exit(261) low-8-bits==5=%d\n", abort_ok, low_ok);
}

int main(void) {
    test_exit_group_teardown();
    test_thread_self_exit();
    test_pthread_exit_last_thread();
    test_thread_fault_group();
    test_exit_kills_workers();
    test_status_encoding();
    return 0;
}
