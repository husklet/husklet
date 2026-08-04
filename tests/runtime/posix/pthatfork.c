// pthread_atfork: prepare/parent/child handlers fire in the correct order across fork().
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <sys/wait.h>

static int log_fd_pre = 0, log_fd_parent = 0, log_fd_child = 0;
static int seq = 0;
static int prep_seq = 0, parent_seq = 0;

static void prepare(void) { prep_seq = ++seq; log_fd_pre = 1; }
static void parent(void) { parent_seq = ++seq; log_fd_parent = 1; }
static void child(void) { log_fd_child = 1; }

int main(void) {
    pthread_atfork(prepare, parent, child);
    pid_t pid = fork();
    if (pid == 0) {
        // Child: only the child handler must have run here.
        _exit(log_fd_child == 1 && log_fd_pre == 1 && log_fd_parent == 0 ? 42 : 7);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    int child_ok = WIFEXITED(status) && WEXITSTATUS(status) == 42;
    // Parent: prepare ran before parent handler.
    printf("atfork prep_before_parent=%d parent_ran=%d child_ok=%d\n",
           prep_seq == 1 && parent_seq == 2, log_fd_parent, child_ok);
    return 0;
}
