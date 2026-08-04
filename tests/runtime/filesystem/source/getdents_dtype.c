// getdents64(2): directory enumeration returns every entry with a correct d_type.
// Linux -> deterministic golden verdict on every engine (entries sorted for stability).
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

struct entry { char name[64]; unsigned char type; };

static int cmp(const void *a, const void *b) {
    return strcmp(((const struct entry *)a)->name, ((const struct entry *)b)->name);
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_getdents_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);

    close(openat(dfd, "regular", O_CREAT | O_WRONLY, 0644));
    mkdirat(dfd, "subdir", 0755);
    symlinkat("regular", dfd, "symlink");
    mkfifoat(dfd, "fifo", 0644);

    DIR *d = opendir(dir);
    struct entry entries[16];
    int count = 0;
    struct dirent *de;
    while ((de = readdir(d)) != NULL) {
        if (strcmp(de->d_name, ".") == 0 || strcmp(de->d_name, "..") == 0) continue;
        snprintf(entries[count].name, sizeof entries[count].name, "%s", de->d_name);
        entries[count].type = de->d_type;
        count++;
    }
    closedir(d);
    qsort(entries, count, sizeof entries[0], cmp);

    printf("getdents-dtype count=%d", count);
    for (int i = 0; i < count; i++) {
        char t = '?';
        switch (entries[i].type) {
            case DT_REG: t = 'f'; break;
            case DT_DIR: t = 'd'; break;
            case DT_LNK: t = 'l'; break;
            case DT_FIFO: t = 'p'; break;
        }
        printf(" %s:%c", entries[i].name, t);
    }
    printf("\n");

    unlinkat(dfd, "regular", 0);
    unlinkat(dfd, "subdir", AT_REMOVEDIR);
    unlinkat(dfd, "symlink", 0);
    unlinkat(dfd, "fifo", 0);
    close(dfd);
    rmdir(dir);
    return 0;
}
