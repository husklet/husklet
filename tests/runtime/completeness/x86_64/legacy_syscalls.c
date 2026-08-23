// x86-64 keeps a set of pre-*at syscall numbers that aarch64 never had: open(2), pipe(22),
// lchown(94), mknod(133), inotify_init(253), signalfd(282), eventfd(284). Old glibc, old Go
// runtimes and hand-written assembly still issue them directly, and a translator that only knows
// the modern *at forms answers ENOSYS -- which surfaces as an unexplained early exit rather than a
// clear error. Issue each raw number through syscall(2) and assert the real behaviour (not merely
// "not ENOSYS"), so the case cannot pass vacuously. access(21) and chmod(90) are included because
// they once shared an internal canonical number and would be conflated by anything keyed on it.
#define _GNU_SOURCE
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

static const char *failure;

// Record the first failing step so a regression names the syscall instead of just flipping a bit.
static int step(const char *name, int ok) {
    if (!ok && !failure) failure = name;
    return ok;
}

static int check(const char *name, long value) {
    return step(name, value >= 0);
}

int main(void) {
    const char *file = "/tmp/legacy_syscalls.txt";
    const char *node = "/tmp/legacy_syscalls.fifo";
    int ok = 1;

    // open(path, O_WRONLY|O_CREAT|O_TRUNC, 0644) -> a usable descriptor we then read back.
    long fd = syscall(2, file, 0x241, 0644);
    ok &= check("open", fd);
    if (fd >= 0) {
        ok &= step("open-write", write((int)fd, "legacy", 6) == 6);
        close((int)fd);
    }
    long rfd = syscall(2, file, 0, 0);
    ok &= check("open-read", rfd);
    if (rfd >= 0) {
        char buffer[8] = {0};
        ok &= step("open-readback", read((int)rfd, buffer, sizeof buffer - 1) == 6 && strcmp(buffer, "legacy") == 0);
        close((int)rfd);
    }

    // access(21) on a file that exists, and on one that does not.
    ok &= check("access", syscall(21, file, F_OK));
    ok &= step("access-absent", syscall(21, "/tmp/legacy_syscalls.absent", F_OK) < 0 && errno == ENOENT);

    // chmod(90): set 0600 and confirm the mode landed. Distinct from access despite the old
    // canonical-number collision.
    ok &= check("chmod", syscall(90, file, 0600));
    struct stat st;
    ok &= step("chmod-mode", stat(file, &st) == 0 && (st.st_mode & 0777) == 0600);

    // lchown(94) to our own ids is permitted unprivileged and must not follow a symlink.
    ok &= check("lchown", syscall(94, file, (long)getuid(), (long)getgid()));

    // pipe(22): the two descriptors must actually carry a byte.
    int fds[2] = {-1, -1};
    ok &= check("pipe", syscall(22, fds));
    if (fds[0] >= 0) {
        char byte = 'p';
        ok &= step("pipe-write", write(fds[1], &byte, 1) == 1);
        byte = 0;
        ok &= step("pipe-read", read(fds[0], &byte, 1) == 1 && byte == 'p');
        close(fds[0]);
        close(fds[1]);
    }

    // mknod(133) creating a FIFO is allowed without privilege; confirm the type.
    unlink(node);
    ok &= check("mknod", syscall(133, node, S_IFIFO | 0600, 0));
    ok &= step("mknod-type", stat(node, &st) == 0 && S_ISFIFO(st.st_mode));
    unlink(node);

    // eventfd(284): a counter seeded to 7 must read back as 7.
    long efd = syscall(284, 7);
    ok &= check("eventfd", efd);
    if (efd >= 0) {
        unsigned long long counter = 0;
        ok &=
            step("eventfd-count", read((int)efd, &counter, sizeof counter) == (ssize_t)sizeof counter && counter == 7);
        close((int)efd);
    }

    // signalfd(282, mask, sizeof mask): -1 allocates a new descriptor for the given mask.
    sigset_t mask;
    sigemptyset(&mask);
    sigaddset(&mask, SIGUSR1);
    // The raw ABI wants the kernel sigset size (_NSIG/8), not glibc's padded sigset_t.
    long sfd = syscall(282, -1, &mask, (size_t)8);
    ok &= check("signalfd", sfd);
    if (sfd >= 0) close((int)sfd);

    // inotify_init(253) takes no arguments and returns a watch descriptor.
    long ifd = syscall(253);
    ok &= check("inotify_init", ifd);
    if (ifd >= 0) close((int)ifd);

    unlink(file);
    if (ok)
        printf("legacy_syscalls ok=1\n");
    else
        printf("legacy_syscalls ok=0 first-failure=%s errno=%d\n", failure ? failure : "?", errno);
    return 0;
}
