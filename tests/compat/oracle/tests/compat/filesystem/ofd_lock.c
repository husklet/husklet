// fcntl F_OFD_SETLK: open-file-description byte-range locks conflict across fds, and a
// non-overlapping range is grantable. F_OFD_GETLK reports the blocking lock type.
// Linux -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static struct flock make(short type, off_t start, off_t len) {
    struct flock fl;
    memset(&fl, 0, sizeof fl);
    fl.l_type = type;
    fl.l_whence = SEEK_SET;
    fl.l_start = start;
    fl.l_len = len;
    return fl;
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_ofdlock_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int a = open(path, O_CREAT | O_RDWR, 0644);
    ftruncate(a, 4096);
    int b = open(path, O_RDWR);

    struct flock w = make(F_WRLCK, 0, 1024);
    int locked = fcntl(a, F_OFD_SETLK, &w) == 0;

    // Overlapping write lock from the other description is refused.
    struct flock c = make(F_WRLCK, 512, 512);
    errno = 0;
    int conflict = fcntl(b, F_OFD_SETLK, &c) != 0 && (errno == EAGAIN || errno == EACCES);

    // GETLK reports the conflict details.
    struct flock q = make(F_WRLCK, 0, 1024);
    int getlk = fcntl(b, F_OFD_GETLK, &q) == 0 && q.l_type == F_WRLCK;

    // A disjoint range is grantable.
    struct flock d = make(F_WRLCK, 2048, 512);
    int disjoint = fcntl(b, F_OFD_SETLK, &d) == 0;

    // Unlock the first range; GETLK then reports it free.
    struct flock u = make(F_UNLCK, 0, 1024);
    fcntl(a, F_OFD_SETLK, &u);
    struct flock q2 = make(F_WRLCK, 0, 512);
    int freed = fcntl(b, F_OFD_GETLK, &q2) == 0 && q2.l_type == F_UNLCK;

    close(a);
    close(b);
    unlink(path);
    rmdir(dir);
    printf("ofd-lock locked=%d conflict=%d getlk=%d disjoint=%d freed=%d\n",
           locked, conflict, getlk, disjoint, freed);
    return 0;
}
