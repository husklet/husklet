// lseek positioning and error semantics: SEEK_SET/CUR/END arithmetic, ESPIPE on a pipe,
// EINVAL on a negative resulting offset, and seeking past EOF then writing leaves a gap.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_lseek_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "0123456789", 10);

    int set_ok = lseek(fd, 4, SEEK_SET) == 4;
    int cur_ok = lseek(fd, 3, SEEK_CUR) == 7;
    int end_ok = lseek(fd, -2, SEEK_END) == 8;

    // Negative resulting offset -> EINVAL, position unchanged.
    errno = 0;
    off_t bad = lseek(fd, -100, SEEK_SET);
    int einval_ok = bad == (off_t)-1 && errno == EINVAL;

    // Seek past EOF and write creates a sparse gap; the file grows.
    lseek(fd, 20, SEEK_SET);
    write(fd, "END", 3);
    struct stat st;
    fstat(fd, &st);
    int grew = st.st_size == 23;

    // ESPIPE on a pipe.
    int pf[2];
    pipe(pf);
    errno = 0;
    off_t pr = lseek(pf[0], 0, SEEK_CUR);
    int espipe_ok = pr == (off_t)-1 && errno == ESPIPE;

    close(pf[0]); close(pf[1]);
    close(fd);
    unlink(path);
    rmdir(dir);
    printf("lseek-errno set=%d cur=%d end=%d einval=%d grew=%d espipe=%d\n",
           set_ok, cur_ok, end_ok, einval_ok, grew, espipe_ok);
    return 0;
}
