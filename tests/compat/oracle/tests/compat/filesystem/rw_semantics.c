// Core read/write/positioned-I/O contract that silent-corruption bugs hide in:
//   - Linux pwrite() HONORS O_APPEND (data lands at EOF, the supplied offset is ignored).
//   - Two O_APPEND fds on one file never overwrite each other (each seek-to-end is atomic).
//   - readv() fills iovecs in order and stops mid-iovec on a short read.
//   - fork-inherited fd shares the open-file-description offset with the parent.
//   - pread/pwrite negative offset -> EINVAL; on a pipe -> ESPIPE.
//   - read on an O_WRONLY fd / write on an O_RDONLY fd -> EBADF; read on a directory -> EISDIR.
// Deterministic boolean verdict, identical on every conforming Linux and both engine arches.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/uio.h>
#include <sys/wait.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_rwsem_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/f", dir);

    // pwrite on an O_APPEND fd ignores the offset and appends (the classic Linux behavior).
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "AAAABBBB", 8);
    close(fd);
    fd = open(path, O_WRONLY | O_APPEND);
    ssize_t pw = pwrite(fd, "zz", 2, 0);
    close(fd);
    fd = open(path, O_RDONLY);
    char b[32] = {0};
    ssize_t rr = read(fd, b, 31);
    close(fd);
    int pwrite_appends = pw == 2 && rr == 10 && memcmp(b, "AAAABBBBzz", 10) == 0;

    // Two O_APPEND fds interleave without clobbering.
    int a1 = open(path, O_WRONLY | O_APPEND | O_TRUNC);
    int a2 = open(path, O_WRONLY | O_APPEND);
    write(a1, "111", 3);
    write(a2, "222", 3);
    write(a1, "333", 3);
    close(a1);
    close(a2);
    fd = open(path, O_RDONLY);
    char c[32] = {0};
    ssize_t cl = read(fd, c, 31);
    close(fd);
    int concurrent_append = cl == 9 && memcmp(c, "111222333", 9) == 0;

    // readv fills iovecs in order and stops mid-iovec on a short read.
    fd = open(path, O_RDWR | O_TRUNC);
    write(fd, "ABCDE", 5);
    lseek(fd, 0, SEEK_SET);
    char v0[2] = {9, 9}, v1[2] = {9, 9}, v2[2] = {9, 9};
    struct iovec rv[3] = {{v0, 2}, {v1, 2}, {v2, 2}};
    ssize_t rvn = readv(fd, rv, 3);
    int readv_order = rvn == 5 && v0[0] == 'A' && v1[0] == 'C' && v2[0] == 'E' && v2[1] == 9;
    close(fd);

    // fork-inherited fd shares the OFD offset.
    fd = open(path, O_RDWR | O_TRUNC);
    write(fd, "0123456789", 10);
    lseek(fd, 0, SEEK_SET);
    pid_t pid = fork();
    if (pid == 0) {
        char cb[4];
        read(fd, cb, 4);
        _exit(0);
    }
    waitpid(pid, NULL, 0);
    int fork_shared_offset = lseek(fd, 0, SEEK_CUR) == 4;
    close(fd);

    // Positioned-I/O error edges.
    fd = open(path, O_RDWR);
    char e[4];
    errno = 0;
    int neg_off_einval = pwrite(fd, "x", 1, -1) == -1 && errno == EINVAL;
    close(fd);
    int pp[2];
    if (pipe(pp)) return 1;
    errno = 0;
    int pipe_espipe = pwrite(pp[1], "x", 1, 0) == -1 && errno == ESPIPE;
    close(pp[0]);
    close(pp[1]);

    // Wrong-mode fds and directory reads.
    int wo = open(path, O_WRONLY);
    errno = 0;
    int read_wo_ebadf = read(wo, e, 1) == -1 && errno == EBADF;
    close(wo);
    int ro = open(path, O_RDONLY);
    errno = 0;
    int write_ro_ebadf = write(ro, "x", 1) == -1 && errno == EBADF;
    close(ro);
    int dd = open(dir, O_RDONLY | O_DIRECTORY);
    errno = 0;
    int read_dir_eisdir = read(dd, e, 1) == -1 && errno == EISDIR;
    close(dd);

    unlink(path);
    rmdir(dir);
    printf("rw-semantics pwrite-appends=%d concurrent-append=%d readv-order=%d "
           "fork-offset=%d neg-einval=%d pipe-espipe=%d read-wo-ebadf=%d "
           "write-ro-ebadf=%d read-dir-eisdir=%d\n",
           pwrite_appends, concurrent_append, readv_order, fork_shared_offset,
           neg_off_einval, pipe_espipe, read_wo_ebadf, write_ro_ebadf, read_dir_eisdir);
    return 0;
}
