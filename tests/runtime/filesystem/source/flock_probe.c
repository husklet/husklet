// flock(2): an exclusive lock on one open description blocks a non-blocking lock on another.
// Portable -> deterministic golden verdict on every engine.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    enum { CHURN_DESCRIPTORS = 600 };

    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_flock_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int a = open(path, O_CREAT | O_RDWR, 0644);
    int b = open(path, O_RDWR);

    int locked = flock(a, LOCK_EX) == 0;
    errno = 0;
    int conflict = flock(b, LOCK_EX | LOCK_NB) != 0 && (errno == EWOULDBLOCK || errno == EAGAIN);
    int released = flock(a, LOCK_UN) == 0;
    int reacquire = flock(b, LOCK_EX | LOCK_NB) == 0;

    // A shared lock permits a second shared lock.
    flock(b, LOCK_UN);
    int sh_a = flock(a, LOCK_SH) == 0;
    int sh_b = flock(b, LOCK_SH | LOCK_NB) == 0;

    flock(a, LOCK_UN);
    flock(b, LOCK_UN);
    close(a);
    close(b);

    // An explicit LOCK_UN releases the open description's lock immediately,
    // even when the descriptor remains open. Exercise more descriptions than
    // the engine's broker table so a stale unlocked record cannot exhaust it.
    int churn_fds[CHURN_DESCRIPTORS];
    int same_inode_churn = 1;
    for (int i = 0; i < CHURN_DESCRIPTORS; ++i)
        churn_fds[i] = -1;
    for (int i = 0; i < CHURN_DESCRIPTORS; ++i) {
        churn_fds[i] = open(path, O_RDWR);
        if (churn_fds[i] < 0 || flock(churn_fds[i], LOCK_EX | LOCK_NB) != 0 || flock(churn_fds[i], LOCK_UN) != 0) {
            same_inode_churn = 0;
            break;
        }
    }
    for (int i = 0; i < CHURN_DESCRIPTORS; ++i) {
        if (churn_fds[i] >= 0) close(churn_fds[i]);
        churn_fds[i] = -1;
    }

    int distinct_inode_churn = 1;
    for (int i = 0; i < CHURN_DESCRIPTORS; ++i) {
        char churn_path[192];
        snprintf(churn_path, sizeof churn_path, "%s/churn-%d", dir, i);
        churn_fds[i] = open(churn_path, O_CREAT | O_RDWR, 0644);
        if (churn_fds[i] < 0 || flock(churn_fds[i], LOCK_EX | LOCK_NB) != 0 || flock(churn_fds[i], LOCK_UN) != 0) {
            distinct_inode_churn = 0;
            break;
        }
    }
    for (int i = 0; i < CHURN_DESCRIPTORS; ++i) {
        char churn_path[192];
        snprintf(churn_path, sizeof churn_path, "%s/churn-%d", dir, i);
        if (churn_fds[i] >= 0) close(churn_fds[i]);
        unlink(churn_path);
    }
    unlink(path);
    rmdir(dir);
    printf("flock-probe locked=%d conflict=%d released=%d reacquire=%d shared-a=%d shared-b=%d "
           "same-inode-churn=%d distinct-inode-churn=%d\n",
           locked, conflict, released, reacquire, sh_a, sh_b, same_inode_churn, distinct_inode_churn);
    return 0;
}
