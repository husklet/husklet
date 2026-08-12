#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static volatile unsigned long state;

static int worker(const char *release, int role) {
    pid_t original_pid = getpid();
    pid_t original_ppid = getppid();
    state = 1000003ul * (unsigned long)role;
    dprintf(STDOUT_FILENO, "READY %d %ld %ld\n", role, (long)original_pid, (long)original_ppid);
    for (;;) {
        state += (unsigned long)(role * 2 + 1);
        if (access(release, F_OK) == 0) break;
        if (errno != ENOENT) return 30 + role;
    }
    if (getpid() != original_pid || getppid() != original_ppid) return 40 + role;
    if (state <= 1000003ul * (unsigned long)role) return 50 + role;
    dprintf(STDOUT_FILENO, "RESTORED %d %ld %ld %lu\n", role, (long)getpid(), (long)getppid(), state);
    return 20 + role;
}

int main(int argc, char **argv) {
    pid_t first, second;
    int first_status, second_status;
    char output[1024];
    int fd;
    if (argc != 2) return 2;
    if (snprintf(output, sizeof output, "%s.output", argv[1]) >= (int)sizeof output) return 2;
    fd = open(output, O_WRONLY | O_CREAT | O_APPEND, 0600);
    if (fd < 0 || dup2(fd, STDOUT_FILENO) < 0 || dup2(fd, STDERR_FILENO) < 0) return 2;
    if (fd > STDERR_FILENO) close(fd);
    fd = open("/dev/null", O_RDONLY);
    if (fd < 0 || dup2(fd, STDIN_FILENO) < 0) return 2;
    if (fd > STDERR_FILENO) close(fd);
    first = fork();
    if (first < 0) return 3;
    if (first == 0) return worker(argv[1], 1);
    second = fork();
    if (second < 0) return 4;
    if (second == 0) return worker(argv[1], 2);
    {
        int result = worker(argv[1], 3);
        if (result != 23) return result;
    }
    if (waitpid(first, &first_status, 0) != first || waitpid(second, &second_status, 0) != second) return 60;
    if (!WIFEXITED(first_status) || WEXITSTATUS(first_status) != 21 || !WIFEXITED(second_status) ||
        WEXITSTATUS(second_status) != 22)
        return 61;
    dprintf(STDOUT_FILENO, "TREE-RESTORED %ld %ld\n", (long)first, (long)second);
    return 0;
}
