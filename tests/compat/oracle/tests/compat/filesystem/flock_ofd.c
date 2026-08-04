// flock(2) ownership model: a BSD lock belongs to the OPEN FILE DESCRIPTION. A dup'd descriptor and a
// fork-inherited descriptor SHARE the lock (locking through them never self-conflicts), while a second
// independent open() of the same file contends. Also exercises exclusive->shared downgrade and the
// shared->exclusive upgrade that a concurrent shared holder blocks. Deterministic (pipe handshake, no
// sleeps); Linux -> a single golden verdict on native and every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int byte_write(int fd, char value) {
    ssize_t r;
    do r = write(fd, &value, 1);
    while (r < 0 && errno == EINTR);
    return r == 1;
}
static int byte_read(int fd) {
    char value;
    ssize_t r;
    do r = read(fd, &value, 1);
    while (r < 0 && errno == EINTR);
    return r == 1;
}

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_flockofd_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);

    int a = open(path, O_CREAT | O_RDWR, 0644);
    int locked = flock(a, LOCK_EX) == 0;

    // A dup shares the open file description, so it holds the SAME lock -> a non-blocking exclusive request
    // through it is granted, not a self-conflict.
    int ad = dup(a);
    int dup_shares = flock(ad, LOCK_EX | LOCK_NB) == 0;

    // Downgrade the exclusive lock to shared; a second INDEPENDENT open may then take a compatible shared lock.
    int downgrade = flock(a, LOCK_SH) == 0;
    int b = open(path, O_RDWR);
    int shared_compat = flock(b, LOCK_SH | LOCK_NB) == 0;

    // Upgrading our shared lock to exclusive is refused while the other description holds a shared lock.
    errno = 0;
    int upgrade_blocked = flock(a, LOCK_EX | LOCK_NB) != 0 && (errno == EWOULDBLOCK || errno == EAGAIN);
    // After that shared holder releases, the upgrade succeeds.
    flock(b, LOCK_UN);
    int upgrade_ok = flock(a, LOCK_EX | LOCK_NB) == 0;

    // fork: the child inherits `a` (same OFD) and opens `path` independently (a distinct OFD). Locking through
    // the inherited descriptor shares the parent's lock (granted); the independent open contends (blocked).
    int rp[2], gp[2];
    if (pipe(rp) != 0 || pipe(gp) != 0) return 1;
    pid_t pid = fork();
    if (pid == 0) {
        close(rp[0]);
        close(gp[1]);
        errno = 0;
        int shares = flock(a, LOCK_EX | LOCK_NB) == 0; // same OFD as the parent's held lock -> granted
        int cfd = open(path, O_RDWR);
        errno = 0;
        int nb = flock(cfd, LOCK_EX | LOCK_NB);
        int contends = nb < 0 && (errno == EWOULDBLOCK || errno == EAGAIN);
        char msg[2] = {shares ? '1' : '0', contends ? '1' : '0'};
        (void)byte_write(rp[1], msg[0]);
        (void)byte_write(rp[1], msg[1]);
        (void)byte_read(gp[0]);
        close(cfd);
        _exit(0);
    }
    close(rp[1]);
    close(gp[0]);
    char c0 = 0, c1 = 0;
    ssize_t n0 = read(rp[0], &c0, 1), n1 = read(rp[0], &c1, 1);
    int fork_shares = n0 == 1 && c0 == '1';
    int fork_contends = n1 == 1 && c1 == '1';
    (void)byte_write(gp[1], 'g');
    waitpid(pid, 0, 0);

    flock(a, LOCK_UN);
    close(a);
    close(ad);
    close(b);
    unlink(path);
    rmdir(dir);
    printf("flock-ofd locked=%d dup-shares=%d downgrade=%d shared-compat=%d upgrade-blocked=%d upgrade-ok=%d "
           "fork-shares=%d fork-contends=%d\n",
           locked, dup_shares, downgrade, shared_compat, upgrade_blocked, upgrade_ok, fork_shares, fork_contends);
    return 0;
}
