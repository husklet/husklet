// scandir with a filter + alphasort yields entries in a deterministic sorted order.
#include <dirent.h>
#include <fcntl.h>
#include <unistd.h>
#include <string.h>
#include <stdlib.h>
#include <stdio.h>
#include <sys/stat.h>

static int only_dat(const struct dirent *e) {
    size_t n = strlen(e->d_name);
    return n > 4 && strcmp(e->d_name + n - 4, ".dat") == 0;
}

int main(void) {
    char dir[64];
    snprintf(dir, sizeof dir, "/tmp/hl_scan_%d", (int)getpid());
    mkdir(dir, 0755);
    const char *files[] = {"c.dat", "a.dat", "b.dat", "skip.txt"};
    for (int i = 0; i < 4; i++) {
        char p[128];
        snprintf(p, sizeof p, "%s/%s", dir, files[i]);
        int fd = open(p, O_CREAT | O_WRONLY, 0644);
        if (fd >= 0) close(fd);
    }

    struct dirent **names = NULL;
    int n = scandir(dir, &names, only_dat, alphasort);
    int count_ok = n == 3;
    int sorted = count_ok &&
                 strcmp(names[0]->d_name, "a.dat") == 0 &&
                 strcmp(names[1]->d_name, "b.dat") == 0 &&
                 strcmp(names[2]->d_name, "c.dat") == 0;
    for (int i = 0; i < n; i++) free(names[i]);
    free(names);

    for (int i = 0; i < 4; i++) {
        char p[128];
        snprintf(p, sizeof p, "%s/%s", dir, files[i]);
        unlink(p);
    }
    rmdir(dir);
    printf("scandirt count=%d sorted=%d\n", count_ok, sorted);
    return 0;
}
