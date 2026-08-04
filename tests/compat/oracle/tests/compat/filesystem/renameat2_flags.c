// renameat2(2) flags: RENAME_NOREPLACE refuses to clobber; RENAME_EXCHANGE swaps atomically.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int put(int dfd, const char *name, const char *body) {
    int fd = openat(dfd, name, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd < 0) return -1;
    int ok = write(fd, body, strlen(body)) == (ssize_t)strlen(body);
    close(fd);
    return ok ? 0 : -1;
}

static int slurp(int dfd, const char *name, char *out, size_t n) {
    int fd = openat(dfd, name, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t r = read(fd, out, n - 1);
    close(fd);
    if (r < 0) return -1;
    out[r] = 0;
    return 0;
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_renameat2_%d", (int)getpid());
    mkdir(dir, 0755);
    int dfd = open(dir, O_RDONLY | O_DIRECTORY);
    put(dfd, "a", "alpha");
    put(dfd, "b", "bravo");

    // NOREPLACE onto an existing name fails with EEXIST and leaves both intact.
    errno = 0;
    int noreplace = renameat2(dfd, "a", dfd, "b", RENAME_NOREPLACE);
    int noreplace_eexist = noreplace != 0 && errno == EEXIST;

    // EXCHANGE swaps the two contents atomically.
    int exchanged = renameat2(dfd, "a", dfd, "b", RENAME_EXCHANGE) == 0;
    char va[16] = {0}, vb[16] = {0};
    slurp(dfd, "a", va, sizeof va);
    slurp(dfd, "b", vb, sizeof vb);
    int swap_ok = strcmp(va, "bravo") == 0 && strcmp(vb, "alpha") == 0;

    // NOREPLACE onto a free name succeeds.
    int noreplace_free = renameat2(dfd, "a", dfd, "c", RENAME_NOREPLACE) == 0;
    int gone = faccessat(dfd, "a", F_OK, 0) != 0;

    unlinkat(dfd, "b", 0);
    unlinkat(dfd, "c", 0);
    close(dfd);
    rmdir(dir);
    printf("renameat2-flags noreplace-eexist=%d exchange=%d swapped=%d noreplace-free=%d src-gone=%d\n",
           noreplace_eexist, exchanged, swap_ok, noreplace_free, gone);
    return 0;
}
