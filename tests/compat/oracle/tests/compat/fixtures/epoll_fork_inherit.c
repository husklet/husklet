// A fork child INHERITS the parent's epoll interest list (Linux ties it to the inherited OFDs). The child
// here does NOT re-register: it just writes to the inherited pipe and epoll_waits the inherited instance,
// expecting the inherited registration (with its original udata) to fire. dd rebuilds an EMPTY kqueue in the
// child (macOS does not inherit kqueue()s), so without re-arming the inherited interest the child saw
// nothing. Deterministic -> oracle-checked against native Linux.
#include <stdio.h>
#include <sys/epoll.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    int fds[2];
    if (pipe(fds) < 0) { perror("pipe"); return 1; }
    int ep = epoll_create1(0);
    struct epoll_event ev = {.events = EPOLLIN, .data.u64 = 0x1234ULL};
    epoll_ctl(ep, EPOLL_CTL_ADD, fds[0], &ev);

    pid_t pid = fork();
    if (pid < 0) { perror("fork"); return 1; }
    if (pid == 0) {
        // child: inherits ep + the interest list + the pipe. Do NOT re-register.
        if (write(fds[1], "z", 1) < 0) _exit(9);
        struct epoll_event out[4];
        int n = epoll_wait(ep, out, 4, 1000);
        int ok = (n >= 1 && out[0].data.u64 == 0x1234ULL);
        _exit(ok ? 7 : 8);
    }
    int st = 0;
    waitpid(pid, &st, 0);
    int child_ok = WIFEXITED(st) && WEXITSTATUS(st) == 7;
    printf("epoll_fork child_ok=%d\n", child_ok);
    close(ep);
    return 0;
}
