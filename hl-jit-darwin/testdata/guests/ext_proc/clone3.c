// clone3(2) (Linux 5.3+): create a child *process* (fork-shaped, no CLONE_VM) via the raw syscall,
// child _exit()s, parent waits and checks the status. Linux-only -> native oracle.
#define _GNU_SOURCE
#include <linux/sched.h>   /* struct clone_args */
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    struct clone_args args = {0};
    args.flags = 0;                    // fork-like: separate address space (COW)
    args.exit_signal = SIGCHLD;        // so waitpid() can reap it

    long pid = syscall(SYS_clone3, &args, sizeof args);
    if (pid == 0) { _exit(11); }       // child
    int made = pid > 0;
    int status = 0;
    int reaped = made && waitpid((pid_t)pid, &status, 0) == (pid_t)pid;
    int exit11 = reaped && WIFEXITED(status) && WEXITSTATUS(status) == 11;
    printf("clone3 made=%d reaped=%d exit11=%d\n", made, reaped, exit11);
    return 0;
}
