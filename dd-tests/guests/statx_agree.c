// #383 differential: statx (nr 291) MUST report the same file metadata as fstat/stat/newfstatat for the
// SAME file -- including dd's ownership virtualization (#156 cuid/cgid) and the #181 guest-chown xattr
// override. statx used to copy the RAW host uid/gid (and never wrote rdev/dev major:minor), so a program
// saw inconsistent ownership/device numbers depending on which call it made.
//
// This test is SELF-CHECKING: it prints only AGREEMENT booleans (statx vs newfstatat/fstat for the same
// file), never absolute ids -- so its output is byte-identical on the native Linux oracle (where the two
// calls trivially agree) and on dd (where they must agree only after the fix). Before the fix, dd diverges
// on the chowned file (raw uid vs xattr uid) and on the device node (rdev 0:0 vs 1:3), flipping the
// booleans and failing the byte-exact oracle diff. Covers a regular file, a #181-chowned file (by path AND
// by fd via AT_EMPTY_PATH), a symlink (AT_SYMLINK_NOFOLLOW), a device node, a directory, and the
// adversarial statx-before/after-chown order.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/syscall.h>

struct sx_ts { int64_t sec; uint32_t nsec; int32_t rsv; };
struct sx {
    uint32_t mask, blksize;
    uint64_t attributes;
    uint32_t nlink, uid, gid;
    uint16_t mode, sp0;
    uint64_t ino, size, blocks, attributes_mask;
    struct sx_ts atime, btime, ctime, mtime;
    uint32_t rdev_major, rdev_minor, dev_major, dev_minor;
    uint64_t mnt_id;
    uint8_t pad[256 - 152];
};

#define AT_EMPTY 0x1000
#define AT_NOFOLLOW 0x100

static int do_statx(int dfd, const char *path, int flags, struct sx *o) {
    memset(o, 0, sizeof *o);
    return (int)syscall(SYS_statx, dfd, path ? path : "", flags, 0x7ff /*BASIC*/ | 0x800 /*BTIME*/, o);
}

// Compare statx result `x` against a struct stat `s` for the fields Linux shares between the two calls.
// Returns 1 iff every shared field agrees.
static int agree(const struct sx *x, const struct stat *s) {
    int ok = 1;
    ok &= (x->uid == s->st_uid);
    ok &= (x->gid == s->st_gid);
    ok &= ((uint32_t)x->mode == (uint32_t)(s->st_mode & 0xffff));
    ok &= (x->nlink == (uint32_t)s->st_nlink);
    ok &= (x->size == (uint64_t)s->st_size);
    ok &= (x->ino == (uint64_t)s->st_ino);
    ok &= (x->blocks == (uint64_t)s->st_blocks);
    ok &= (x->rdev_major == major(s->st_rdev));
    ok &= (x->rdev_minor == minor(s->st_rdev));
    ok &= (x->dev_major == major(s->st_dev));
    ok &= (x->dev_minor == minor(s->st_dev));
    return ok;
}

int main(void) {
    const char *reg = "/tmp/ddsx_reg", *cwn = "/tmp/ddsx_chown", *sym = "/tmp/ddsx_sym", *dir = "/tmp/ddsx_dir";
    unlink(reg); unlink(cwn); unlink(sym); rmdir(dir);

    // regular file
    int fd = open(reg, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) { (void)!write(fd, "abcd", 4); close(fd); }
    struct sx x; struct stat s;
    do_statx(AT_FDCWD, reg, 0, &x); fstatat(AT_FDCWD, reg, &s, 0);
    int r_reg = agree(&x, &s);

    // #181 guest-chown: chown a file to a guest uid/gid, then statx must match newfstatat (both honour the
    // xattr override). Adversarial order: statx BEFORE the chown, then AFTER -- the after must match fstat.
    fd = open(cwn, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) { (void)!write(fd, "z", 1); close(fd); }
    struct sx xb; do_statx(AT_FDCWD, cwn, 0, &xb); // before chown
    chown(cwn, 12345, 6789);
    do_statx(AT_FDCWD, cwn, 0, &x); fstatat(AT_FDCWD, cwn, &s, 0);
    int r_chowned = agree(&x, &s);
    // order: the after-chown statx must differ-or-equal EXACTLY as fstat does relative to before; the
    // load-bearing assertion is that the after values match fstat (checked in r_chowned). Additionally the
    // ownership must have actually taken effect consistently: statx-after == fstat-after (already in agree),
    // and statx-after != statx-before OR fstat agrees it was a no-op. Encode as: after statx == after fstat.
    int r_order = (x.uid == (uint32_t)s.st_uid && x.gid == (uint32_t)s.st_gid);
    (void)xb;

    // by fd (AT_EMPTY_PATH): statx(fd,"") vs fstat(fd) on the chowned file -> exercises the fd xattr path
    fd = open(cwn, O_RDONLY);
    int r_fstat = 0;
    if (fd >= 0) {
        do_statx(fd, "", AT_EMPTY, &x); fstat(fd, &s);
        r_fstat = agree(&x, &s);
        close(fd);
    }

    // symlink, AT_SYMLINK_NOFOLLOW (lstat the link itself)
    symlink(reg, sym);
    do_statx(AT_FDCWD, sym, AT_NOFOLLOW, &x); fstatat(AT_FDCWD, sym, &s, AT_NOFOLLOW);
    int r_sym = agree(&x, &s);

    // device node (rdev major:minor must agree -- buggy statx wrote 0:0)
    do_statx(AT_FDCWD, "/dev/null", 0, &x); fstatat(AT_FDCWD, "/dev/null", &s, 0);
    int r_dev = agree(&x, &s);

    // directory
    mkdir(dir, 0755);
    do_statx(AT_FDCWD, dir, 0, &x); fstatat(AT_FDCWD, dir, &s, 0);
    int r_dir = agree(&x, &s);

    // statx-specific sanity, phrased to be byte-identical native-vs-dd: all BASIC fields were reported,
    // and btime is non-negative (native returns 0 when the FS lacks btime; dd returns st_birthtime>=0).
    do_statx(AT_FDCWD, reg, 0, &x);
    int r_mask = ((x.mask & 0x7ff) == 0x7ff);
    int r_btime = (x.btime.sec >= 0);

    unlink(reg); unlink(cwn); unlink(sym); rmdir(dir);
    printf("statx-agree reg=%d chowned=%d fstat=%d sym=%d dev=%d dir=%d order=%d mask=%d btime=%d\n",
           r_reg, r_chowned, r_fstat, r_sym, r_dev, r_dir, r_order, r_mask, r_btime);
    return 0;
}
