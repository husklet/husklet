// inotify IN_MODIFY + IN_DELETE_SELF: watch a file, write to it, then delete it; read the events.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/inotify.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char path[128];
    char directory[128];
    char missing[128];
    snprintf(path, sizeof path, "/tmp/hl_inotify_%d", (int)getpid());
    snprintf(directory, sizeof directory, "/tmp/hl_inotify_dir_%d", (int)getpid());
    snprintf(missing, sizeof missing, "/tmp/hl_inotify_missing_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR, 0644);
    close(fd);
    int in = inotify_init1(0);
    errno = 0;
    if (inotify_add_watch(in, path, IN_ONLYDIR | IN_MODIFY) != -1 || errno != ENOTDIR) return 11;
    errno = 0;
    if (inotify_add_watch(in, missing, IN_MODIFY) != -1 || errno != ENOENT) return 12;
    if (mkdir(directory, 0700) != 0) return 13;
    int directory_watch = inotify_add_watch(in, directory, IN_ONLYDIR | IN_CREATE);
    if (directory_watch < 0 || inotify_rm_watch(in, directory_watch) != 0 || rmdir(directory) != 0) return 14;
    int wd = inotify_add_watch(in, path, IN_MODIFY | IN_ATTRIB | IN_DELETE_SELF);
    if (wd < 0) return 15;
    fd = open(path, O_WRONLY);
    write(fd, "data", 4);
    close(fd);
    chmod(path, 0600);
    unlink(path);
    // drain events
    char buf[4096];
    int n = read(in, buf, sizeof buf);
    int modify = 0, attrib = 0, del = 0;
    for (int o = 0; o < n;) {
        struct inotify_event *e = (struct inotify_event *)(buf + o);
        if (e->mask & IN_MODIFY) modify = 1;
        if (e->mask & IN_ATTRIB) attrib = 1;
        if (e->mask & IN_DELETE_SELF) del = 1;
        o += sizeof(struct inotify_event) + e->len;
    }
    inotify_rm_watch(in, wd);
    close(in);
    printf("inotify_modify modify=%d attrib=%d delete=%d\n", modify, attrib, del);
    return 0;
}
