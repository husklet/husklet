// devpts virtualization (#227 + #280): a guest that creates its OWN pty must see it as /dev/pts/<N>
// EVERYWHERE -- the host pty device (a macOS /dev/ttysNNN or host /dev/pts/M) must never leak. Two paths:
//   (A) musl-style: open("/dev/ptmx") + ioctl(TIOCGPTN) + open("/dev/pts/N")  (#227: the slave open, which
//       has no rootfs backing file, must be intercepted ahead of the overlay resolver -- not ENOENT).
//   (B) glibc-openpty-style: ioctl(master, TIOCGPTPEER) -- the single-call slave open glibc's openpty uses.
// For each, ptsname(master), ttyname(slave) and readlink(/proc/self/fd/slave) must ALL equal /dev/pts/N,
// /dev/pts/N must stat as a char device whose st_dev/st_ino/st_rdev match fstat(slave) (what ttyname
// verifies), and none may be a leaked host /dev/ttys* path. Golden booleans (index value is nondeterministic
// -- it depends on the ctty, so we assert consistency, not the exact N). Linux engines (glibc); darwin has
// its own pty stack so this Linux/devpts shape is Linux-only.
#define _XOPEN_SOURCE 600
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <termios.h>
#include <unistd.h>

#ifndef TIOCGPTN
#define TIOCGPTN 0x80045430
#endif
#ifndef TIOCGPTPEER
#define TIOCGPTPEER 0x5441
#endif

static int is_pts(const char *p) { return p && !strncmp(p, "/dev/pts/", 9); }
static int is_hostleak(const char *p) {
    return !p || !strncmp(p, "/dev/ttys", 9) || strstr(p, "/dev/pts/ptmx") != NULL;
}
// Verify a slave fd's whole /dev/pts/N surface. Writes one boolean group; returns nothing.
static void check_slave(const char *tag, int m, int s, const char *want) {
    int open_ok = s >= 0;                                  // #227: slave open succeeded
    int ttyname_ok = 0, link_ok = 0, chr_ok = 0, idmatch = 0, agree = 0, noleak = 0;
    char link[128] = "";
    char *tn = NULL, *pn = ptsname(m);
    if (s >= 0) {
        tn = ttyname(s);
        ttyname_ok = is_pts(tn);
        char proc[64];
        snprintf(proc, sizeof proc, "/proc/self/fd/%d", s);
        ssize_t l = readlink(proc, link, sizeof link - 1);
        if (l > 0) { link[l] = 0; link_ok = is_pts(link); }
        struct stat st, fst;
        if (stat(want, &st) == 0) chr_ok = S_ISCHR(st.st_mode);
        if (fstat(s, &fst) == 0 && stat(want, &st) == 0)
            idmatch = st.st_dev == fst.st_dev && st.st_ino == fst.st_ino && st.st_rdev == fst.st_rdev;
        agree = pn && tn && !strcmp(pn, want) && !strcmp(tn, want) && !strcmp(link, want);
        noleak = !is_hostleak(pn) && !is_hostleak(tn) && !is_hostleak(link);
    }
    printf("%s open=%d ptsname=%d ttyname=%d link=%d chr=%d idmatch=%d agree=%d noleak=%d\n", tag, open_ok,
           is_pts(pn), ttyname_ok, link_ok, chr_ok, idmatch, agree, noleak);
}

int main(void) {
    // ---- (A) musl-style: /dev/ptmx + TIOCGPTN + open("/dev/pts/N") ----
    int m = open("/dev/ptmx", O_RDWR | O_NOCTTY);
    if (m < 0) { printf("A ptmx=0\nB openpty=0\n"); return 0; }
    grantpt(m);
    unlockpt(m);
    int n = -1;
    ioctl(m, TIOCGPTN, &n);
    char path[64];
    snprintf(path, sizeof path, "/dev/pts/%d", n);
    int s = open(path, O_RDWR | O_NOCTTY);
    check_slave("A", m, s, path);
    if (s >= 0) close(s);
    close(m);

    // ---- (B) glibc-openpty-style: ioctl(master, TIOCGPTPEER) opens the slave in one call ----
    int bm = open("/dev/ptmx", O_RDWR | O_NOCTTY);
    if (bm < 0) { printf("B ptmx=0\n"); return 0; }
    grantpt(bm);
    unlockpt(bm);
    int bn = -1;
    ioctl(bm, TIOCGPTN, &bn);
    char bpath[64];
    snprintf(bpath, sizeof bpath, "/dev/pts/%d", bn);
    int bs = ioctl(bm, TIOCGPTPEER, O_RDWR | O_NOCTTY);
    check_slave("B", bm, bs, bpath);
    if (bs >= 0) close(bs);
    close(bm);
    return 0;
}
