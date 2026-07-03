// /proc/self/maps + /proc/self/fd. maps: at least one region, each line "lo-hi perms offset dev inode …"
// with a 4-char perms field ending in p/s; the executable's own text must be mapped r-xp somewhere. fd:
// opendir enumerates at least 0/1/2 plus the dir fd; a readlink of an entry yields a path.
#define _GNU_SOURCE
#include <dirent.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

int main(void) {
    char b[65536];
    int n = pf_read("/proc/self/maps", b, sizeof b);
    // first line perms field
    int perms_ok = 0, has_xp = 0;
    for (const char *p = b; p && *p;) {
        unsigned long lo, hi; char perms[8] = {0};
        if (sscanf(p, "%lx-%lx %4s", &lo, &hi, perms) == 3 && strlen(perms) == 4 &&
            (perms[3] == 'p' || perms[3] == 's')) {
            perms_ok = 1;
            if (perms[2] == 'x' && perms[0] == 'r') has_xp = 1;
        }
        const char *nl = strchr(p, '\n');
        p = nl ? nl + 1 : 0;
    }
    // fd dir
    DIR *d = opendir("/proc/self/fd");
    int fdcount = 0;
    if (d) { struct dirent *e; while ((e = readdir(d))) if (e->d_name[0] != '.') fdcount++; closedir(d); }
    char lnk[256];
    ssize_t ll = readlink("/proc/self/fd/1", lnk, sizeof lnk - 1);
    int fd_link_ok = ll > 0;
    int ok = n > 0 && perms_ok && has_xp && fdcount >= 3 && fd_link_ok;
    printf("maps ok=%d\n", ok);
    return 0;
}
