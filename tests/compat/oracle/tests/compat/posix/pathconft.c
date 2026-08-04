// pathconf/fpathconf/sysconf limits are consistent and positive for a real file.
#include <unistd.h>
#include <fcntl.h>
#include <stdio.h>

int main(void) {
    char path[64];
    snprintf(path, sizeof path, "/tmp/hl_pc_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR, 0644);
    if (fd < 0) { printf("pathconft open=0\n"); return 0; }

    long namemax = pathconf(path, _PC_NAME_MAX);
    long pathmax = pathconf(path, _PC_PATH_MAX);
    long links = fpathconf(fd, _PC_LINK_MAX);
    long fnamemax = fpathconf(fd, _PC_NAME_MAX);

    long argmax = sysconf(_SC_ARG_MAX);

    int ok = namemax >= 255 && pathmax >= 255 && links >= 1 &&
             fnamemax == namemax && argmax > 0;

    close(fd);
    unlink(path);
    printf("pathconft namemax=%d pathmax=%d links=%d match=%d limits=%d\n",
           namemax >= 255, pathmax >= 255, links >= 1, fnamemax == namemax, ok);
    return 0;
}
