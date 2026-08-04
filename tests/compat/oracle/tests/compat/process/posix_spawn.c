// posix_spawn: launch a child that re-execs this same binary in "child" mode (exit 7), with a
// file-action redirecting the child's stdout to a temp file we then read back.
// Portable POSIX -> golden verdict on every engine. argv[0] is the absolute guest path.
#include <fcntl.h>
#include <spawn.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "child") == 0) {
        if (write(1, "spawned-child\n", 14) != 14) return 8;
        return 7;
    }
    char out[128];
    snprintf(out, sizeof out, "/tmp/hl_spawn_%d", (int)getpid());

    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    posix_spawn_file_actions_addopen(&fa, 1, out, O_CREAT | O_WRONLY | O_TRUNC, 0644);

    char *cargv[] = { argv[0], (char *)"child", NULL };
    pid_t pid = 0;
    int rc = posix_spawn(&pid, argv[0], &fa, NULL, cargv, environ);
    int spawned = rc == 0 && pid > 0;
    int status = 0;
    waitpid(pid, &status, 0);
    int exit7 = WIFEXITED(status) && WEXITSTATUS(status) == 7;

    char buf[64] = {0};
    int fd = open(out, O_RDONLY);
    int n = fd >= 0 ? read(fd, buf, sizeof buf) : -1;
    int captured = n == 14 && memcmp(buf, "spawned-child\n", 14) == 0;
    if (fd >= 0) close(fd);
    posix_spawn_file_actions_destroy(&fa);
    unlink(out);
    printf("posix_spawn spawned=%d exit7=%d captured=%d\n", spawned, exit7, captured);
    return 0;
}
