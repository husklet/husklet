#define _GNU_SOURCE
#include <stdio.h>
#include <sys/inotify.h>
#include <unistd.h>
#include <fcntl.h>

int main() {
    int fd = inotify_init1(0);
    if (fd < 0) {
        perror("inotify");
        return 1;
    }
    inotify_add_watch(fd, "/tmp", IN_CREATE);
    int f = open("/tmp/hl-inotify", O_CREAT | O_WRONLY, 0644);
    close(f);
    char buf[4096];
    read(fd, buf, sizeof buf);
    struct inotify_event *ev = (void *)buf;
    printf("INOTIFY=%s\n", ev->len ? ev->name : "none");
    return 0;
}
