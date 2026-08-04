// Buffers straddling a PROT_NONE guard page: Linux copies what it can and only reports EFAULT
// when nothing was transferred, so a straddling read is a short read, a straddling write to a
// pipe is a short write, writev stops at the fault, and a wholly-unreadable buffer is EFAULT.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/uio.h>
#include <unistd.h>

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    char *r = mmap(NULL, ps * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    mprotect(r + ps, ps, PROT_NONE);
    memset(r, 'z', ps);
    char *straddle = r + ps - 8;
    int zfd = open("/dev/zero", O_RDONLY);
    int pfd[2];
    if (pipe(pfd) != 0) return 1;
    int nfd = pfd[1];

    ssize_t rd = read(zfd, straddle, 32);
    int erd = (rd == -1) ? errno : 0;
    ssize_t wr = write(nfd, straddle, 32);
    int ewr = (wr == -1) ? errno : 0;
    ssize_t fit = read(zfd, straddle, 8);
    ssize_t wfit = write(nfd, straddle, 8);
    struct iovec iov[2] = {{r, 8}, {straddle, 32}};
    ssize_t v = writev(nfd, iov, 2);
    int ev = (v == -1) ? errno : 0;
    char *guard = r + ps;
    ssize_t g = read(zfd, guard, 1);
    int eg = (g == -1) ? errno : 0;
    // getcwd and uname into the guard must also fault
    int ecwd = (getcwd(guard, 16) == NULL) ? errno : 0;
    char big[4096];
    int ecwd2 = (getcwd(guard, sizeof big) == NULL) ? errno : 0;
    (void)big;
    printf("rd=%zd erd=%d wr=%zd ewr=%d fit=%zd wfit=%zd v=%zd ev=%d g=%zd eg=%d ecwd=%d ecwd2=%d\n",
           rd, erd, wr, ewr, fit, wfit, v, ev, g, eg, ecwd, ecwd2);
    return 0;
}
