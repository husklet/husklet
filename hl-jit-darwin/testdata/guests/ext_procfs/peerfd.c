// Peer /proc/<pid>/fd cross-process fidelity. A container process can inspect ANOTHER process's open-fd
// table: /proc/<pid>/fd is a listable directory of symlinks, and /proc/<pid>/fd/<N> readlinks to the fd's
// target. hl runs every guest process as its own macOS process with a PRIVATE fd table, and the
// cross-process proc registry published only comm+argv -- so a peer's /proc/<pid>/fd was advertised
// (listed as a dir entry) but its contents ENOENTed. The fix reads a peer's fd table from the host kernel
// (libproc) and serves the listing + per-fd readlink (the symlink-target view; actually OPENING a peer fd
// stays deferred -- that needs cross-process fd passing).
//
// The PARENT reads the CHILD's /proc/<pid>/fd: the directory must list the fd the child pinned, lstat of
// that entry must be a symlink, and its readlink must equal what the CHILD sees for its OWN fd (the two
// views are compared to each other, so the assertion is host-path independent). Verdict ok=1.
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#define PIN_FD 100 // the child dup2()s its marker file onto this fixed, high fd so the parent can name it.

static void nap_ms(int ms) {
    struct timespec ts = {ms / 1000, (long)(ms % 1000) * 1000 * 1000};
    nanosleep(&ts, NULL);
}

int main(void) {
    int pipefd[2];
    if (pipe(pipefd) != 0) {
        printf("peerfd ok=0 (pipe)\n");
        return 0;
    }

    pid_t child = fork();
    if (child == 0) {
        close(pipefd[0]);
        // Pin a real file onto a known fd. Its readlink target is the child's OWN view of PIN_FD.
        char marker[64];
        snprintf(marker, sizeof marker, "/tmp/.hlpeerfd_%d", (int)getpid());
        int mf = open(marker, O_RDWR | O_CREAT | O_TRUNC, 0600);
        if (mf < 0) _exit(1);
        if (mf != PIN_FD) {
            dup2(mf, PIN_FD);
            close(mf);
        }
        char self[512], selfpath[64];
        snprintf(selfpath, sizeof selfpath, "/proc/self/fd/%d", PIN_FD);
        ssize_t sl = readlink(selfpath, self, sizeof self - 1);
        if (sl < 0) sl = 0;
        self[sl] = 0;
        // Hand the child's own-view target (NUL-terminated) to the parent, then park until killed.
        if (write(pipefd[1], self, (size_t)sl + 1) < 0) _exit(1);
        for (;;) pause();
        _exit(0);
    }

    close(pipefd[1]);
    char want[512] = {0};
    ssize_t wn = read(pipefd[0], want, sizeof want - 1);
    if (wn <= 0) want[0] = 0; // child NUL-terminated its write, so `want` is a C string already

    char fddir[64], entry[80], pin[16];
    snprintf(fddir, sizeof fddir, "/proc/%d/fd", (int)child);
    snprintf(entry, sizeof entry, "/proc/%d/fd/%d", (int)child, PIN_FD);
    snprintf(pin, sizeof pin, "%d", PIN_FD);

    // 1) The peer /proc/<pid>/fd directory lists the pinned fd. Poll -- the child may not have pinned yet.
    int dir_ok = 0;
    for (int i = 0; i < 200 && !dir_ok; i++) {
        DIR *d = opendir(fddir);
        if (d) {
            struct dirent *e;
            while ((e = readdir(d))) {
                if (!strcmp(e->d_name, pin)) {
                    dir_ok = 1;
                    break;
                }
            }
            closedir(d);
        }
        if (!dir_ok) nap_ms(5);
    }

    // 2) lstat of the peer fd entry is a symlink.
    struct stat st;
    int lstat_ok = (lstat(entry, &st) == 0) && S_ISLNK(st.st_mode);

    // 3) readlink of the peer fd entry equals the child's own view of the same fd.
    char got[512] = {0};
    ssize_t gn = readlink(entry, got, sizeof got - 1);
    if (gn >= 0) got[gn] = 0;
    int readlink_ok = (gn > 0) && want[0] && !strcmp(got, want);

    kill(child, SIGKILL);
    waitpid(child, NULL, 0);

    int ok = dir_ok && lstat_ok && readlink_ok;
    printf("peerfd ok=%d dir=%d lstat=%d readlink=%d\n", ok, dir_ok, lstat_ok, readlink_ok);
    return 0;
}
