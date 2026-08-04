// Linux gives each epoll open-file description an independent interest set.
// A dup shares that set, and closing one alias must not retire registrations
// while another alias survives.  PostgreSQL 16 exercises all three properties
// when its postmaster forks workers and each child closes inherited wait sets.
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <fcntl.h>
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

static int wait_data(int epoll, uint64_t expected) {
    struct epoll_event event = {0};
    int count = epoll_wait(epoll, &event, 1, 1000);
    return count == 1 && event.data.u64 == expected && (event.events & EPOLLIN) != 0;
}

int main(int argc, char **argv) {
    const char *exec_state = getenv("HL_EPOLL_EXEC_STATE");
    if (exec_state != NULL) {
        int epoll, read_fd, write_fd;
        if (sscanf(exec_state, "%d,%d,%d", &epoll, &read_fd, &write_fd) != 3) return 10;
        int exec_alias = fcntl(epoll, F_GETFD) >= 0 && write(write_fd, "x", 1) == 1 &&
                         wait_data(epoll, UINT64_C(0x9999aaaabbbbcccc));
        printf("epoll_exec_alias=%d\n", exec_alias);
        return !exec_alias;
    }
    int first_pipe[2], second_pipe[2];
    if (pipe(first_pipe) != 0 || pipe(second_pipe) != 0) return 1;
    int first = epoll_create1(0), second = epoll_create1(0);
    if (first < 0 || second < 0) return 2;
    struct epoll_event first_event = {.events = EPOLLIN, .data.u64 = UINT64_C(0x1111222233334444)};
    struct epoll_event second_event = {.events = EPOLLIN, .data.u64 = UINT64_C(0xaaaabbbbccccdddd)};
    if (epoll_ctl(first, EPOLL_CTL_ADD, first_pipe[0], &first_event) != 0 ||
        epoll_ctl(second, EPOLL_CTL_ADD, second_pipe[0], &second_event) != 0)
        return 3;

    // Distinct epoll instances have identical anon_inode stat identity on
    // Linux, but their registrations and user data must remain independent.
    write(first_pipe[1], "a", 1);
    write(second_pipe[1], "b", 1);
    int isolated = wait_data(first, first_event.data.u64) && wait_data(second, second_event.data.u64);

    int alias = dup(first);
    int close_range_alias = alias >= 0 && syscall(SYS_close_range, (unsigned)first, (unsigned)first, 0) == 0 &&
                            fcntl(first, F_GETFD) < 0;
    char byte;
    read(first_pipe[0], &byte, 1);
    write(first_pipe[1], "c", 1);
    int duplicate_close = alias >= 0 && wait_data(alias, first_event.data.u64);

    pid_t child = fork();
    if (child == 0) {
        close(alias); // parent alias still owns the shared epoll description
        _exit(0);
    }
    int status = 0;
    waitpid(child, &status, 0);
    read(first_pipe[0], &byte, 1);
    write(first_pipe[1], "d", 1);
    int fork_close = WIFEXITED(status) && WEXITSTATUS(status) == 0 &&
                     wait_data(alias, first_event.data.u64);

    close(alias);
    int reused = epoll_create1(0);
    struct epoll_event reused_event = {.events = EPOLLIN, .data.u64 = UINT64_C(0x5555666677778888)};
    read(first_pipe[0], &byte, 1);
    int reuse = reused >= 0 && epoll_ctl(reused, EPOLL_CTL_ADD, first_pipe[0], &reused_event) == 0;
    write(first_pipe[1], "e", 1);
    reuse = reuse && wait_data(reused, reused_event.data.u64);

    int exec_pipe[2];
    int exec_epoll = epoll_create1(EPOLL_CLOEXEC);
    int exec_alias = dup(exec_epoll);
    struct epoll_event exec_event = {.events = EPOLLIN, .data.u64 = UINT64_C(0x9999aaaabbbbcccc)};
    int exec_ready = pipe(exec_pipe) == 0 && exec_epoll >= 0 && exec_alias >= 0 &&
                     epoll_ctl(exec_epoll, EPOLL_CTL_ADD, exec_pipe[0], &exec_event) == 0;
    char state[96];
    snprintf(state, sizeof(state), "%d,%d,%d", exec_alias, exec_pipe[0], exec_pipe[1]);
    setenv("HL_EPOLL_EXEC_STATE", state, 1);

    printf("epoll_instance isolated=%d duplicate_close=%d fork_close=%d close_range=%d reuse=%d\n",
           isolated, duplicate_close, fork_close, close_range_alias, reuse);
    fflush(stdout);
    if (!isolated) return 41;
    if (!duplicate_close) return 42;
    if (!fork_close) return 43;
    if (!close_range_alias) return 44;
    if (!reuse) return 45;
    if (!exec_ready) return 46;
    char *next[] = {argv[0], NULL};
    execv(argv[0], next);
    return 5;
}
