// Named FIFO created via mkfifoat, then a nonblocking open + poll round-trip through fork.
// Portable POSIX -> golden verdict on every engine.
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_fifoat_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);
    int made = mkfifoat(dfd, "pipe", 0644) == 0;
    struct stat st;
    int isfifo = fstatat(dfd, "pipe", &st, 0) == 0 && S_ISFIFO(st.st_mode);

    char path[192];
    snprintf(path, sizeof path, "%s/pipe", dir);
    pid_t pid = fork();
    if (pid == 0) {
        int w = open(path, O_WRONLY);
        write(w, "fifobyte", 8);
        close(w);
        _exit(0);
    }
    int r = open(path, O_RDONLY);
    struct pollfd pfd = { .fd = r, .events = POLLIN };
    int ready = poll(&pfd, 1, 2000) == 1 && (pfd.revents & POLLIN);
    char buf[16] = {0};
    int n = read(r, buf, sizeof buf);
    int got = n == 8 && memcmp(buf, "fifobyte", 8) == 0;
    close(r);
    waitpid(pid, NULL, 0);

    unlinkat(dfd, "pipe", 0);
    close(dfd);
    rmdir(dir);
    printf("mkfifoat made=%d isfifo=%d ready=%d got=%d\n", made, isfifo, ready, got);
    return 0;
}
