#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/eventfd.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef CLOSE_RANGE_UNSHARE
#define CLOSE_RANGE_UNSHARE (1U << 1)
#endif

struct state {
    int closed;
    int survivor;
    _Atomic int child;
    _Atomic int done;
};

static void *forker(void *opaque) {
    struct state *state = opaque;
    if (syscall(SYS_close_range, (unsigned)state->closed, (unsigned)state->closed,
                CLOSE_RANGE_UNSHARE) != 0) {
        atomic_store(&state->done, 1);
        return NULL;
    }
    pid_t child = fork();
    if (child == 0) {
        errno = 0;
        int closed_result = fcntl(state->closed, F_GETFD);
        int closed_errno = errno;
        int private_closed = closed_result == -1 && closed_errno == EBADF;
        int inherited_alive = fcntl(state->survivor, F_GETFD) >= 0;
        _exit(private_closed && inherited_alive ? 0 : closed_result >= 0 ? 31 : closed_errno == EBADF ? 32 : 33);
    }
    atomic_store(&state->child, (int)child);
    atomic_store(&state->done, 1);
    return NULL;
}

int main(void) {
    struct state state = {
        .closed = eventfd(0, EFD_NONBLOCK),
        .survivor = eventfd(0, EFD_NONBLOCK),
    };
    if (state.closed < 0 || state.survivor < 0) return 10;

    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    pthread_t thread;
    if (pthread_create(&thread, &attr, forker, &state) != 0) return 11;
    pthread_attr_destroy(&attr);
    while (!atomic_load(&state.done)) sched_yield();

    int child = atomic_load(&state.child);
    int status = 0;
    int child_ok = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0;
    int child_status = WIFEXITED(status) ? WEXITSTATUS(status) : -1;
    int main_closed_alive = fcntl(state.closed, F_GETFD) >= 0;
    int main_survivor_alive = fcntl(state.survivor, F_GETFD) >= 0;
    int main_close = close(state.closed) == 0 && close(state.survivor) == 0;
    printf("close_range_fork child_ok=%d child_status=%d main_closed_alive=%d main_survivor_alive=%d main_close=%d\n",
           child_ok, child_status, main_closed_alive, main_survivor_alive, main_close);
    return child_ok && main_closed_alive && main_survivor_alive && main_close ? 0 : 1;
}
