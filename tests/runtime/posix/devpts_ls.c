// devpts directory listing (#280): when a guest allocates a pty, /dev/pts must list the live slave node
// plus the ptmx multiplexer (real devpts creates /dev/pts/N on slave allocation). Runs inside a container
// rootfs (so /dev/pts is the populated devpts mount). The host may already own ptys, so validate that
// the allocated slave and ptmx are visible without baking a host-global pty index into the golden.
#define _XOPEN_SOURCE 600
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#ifndef TIOCGPTN
#define TIOCGPTN 0x80045430
#endif

static int cmp(const void *a, const void *b) { return strcmp(*(char *const *)a, *(char *const *)b); }

int main(void) {
    int m = open("/dev/ptmx", O_RDWR | O_NOCTTY);
    if (m < 0) { printf("ls ptmx=0\n"); return 0; }
    grantpt(m);
    unlockpt(m);
    int n = -1;
    ioctl(m, TIOCGPTN, &n);
    char path[64];
    snprintf(path, sizeof path, "/dev/pts/%d", n);
    int s = open(path, O_RDWR | O_NOCTTY); // publishes /dev/pts/<n>

    DIR *d = opendir("/dev/pts");
    char *names[64];
    int cnt = 0;
    if (d) {
        struct dirent *e;
        while ((e = readdir(d)) && cnt < 64) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            names[cnt++] = strdup(e->d_name);
        }
        closedir(d);
    }
    qsort(names, cnt, sizeof names[0], cmp);
    int live = 0;
    int ptmx = 0;
    char index[32];
    snprintf(index, sizeof index, "%d", n);
    for (int i = 0; i < cnt; i++) {
        if (!strcmp(names[i], index)) live = 1;
        if (!strcmp(names[i], "ptmx")) ptmx = 1;
        free(names[i]);
    }
    printf("ls live=%d ptmx=%d\n", live, ptmx);
    if (s >= 0) close(s);
    close(m);
    return 0;
}
