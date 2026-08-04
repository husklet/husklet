// xattr errno/edge fidelity (LTP setxattr/getxattr/removexattr family) — diffed vs the native oracle.
// Verdict-only output (errno NAMES + lengths, never raw fds/pids), so hl must be byte-identical to
// native Linux (aarch64) / qemu (x86_64). Complements ext_fsx/xattr.c (the happy-path round-trip).
// Exercises: ENODATA on a missing attr (get/remove), EEXIST for XATTR_CREATE on an existing attr,
// ENODATA for XATTR_REPLACE on a missing attr, ERANGE for a too-small get/list buffer, the size-probe
// (size==0 returns the needed length), and EINVAL for XATTR_CREATE|XATTR_REPLACE together.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/xattr.h>
#include <unistd.h>

// Compact errno name for the codes this test can produce (keeps output byte-identical across arches).
static const char *en(int e) {
    switch (e) {
    case 0: return "ok";
    case ENODATA: return "ENODATA";
    case EEXIST: return "EEXIST";
    case ERANGE: return "ERANGE";
    case EINVAL: return "EINVAL";
    case ENOENT: return "ENOENT";
    case E2BIG: return "E2BIG";
    default: return "OTHER";
    }
}
// Result of a syscall that returns 0/-1: the errno name, or "ok".
static const char *rc(long r) { return r == 0 ? "ok" : en(errno); }

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_xattr_edge_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0) { perror("open"); return 1; }
    write(fd, "body", 4);
    close(fd);

    const char *N = "user.hledge";
    char buf[64];

    // 1. get before any set -> ENODATA
    printf("get_missing=%s\n", en((getxattr(path, N, buf, sizeof buf) < 0) ? errno : 0));
    // 2. remove before any set -> ENODATA
    printf("rm_missing=%s\n", rc(removexattr(path, N)));
    // 3. XATTR_REPLACE on a missing attr -> ENODATA
    printf("replace_missing=%s\n", rc(setxattr(path, N, "v", 1, XATTR_REPLACE)));
    // 4. plain set -> ok
    printf("set=%s\n", rc(setxattr(path, N, "hello", 5, 0)));
    // 5. XATTR_CREATE on the now-existing attr -> EEXIST
    printf("create_existing=%s\n", rc(setxattr(path, N, "x", 1, XATTR_CREATE)));
    // 6. get size-probe (size==0) -> returns the value length (5)
    long probe = getxattr(path, N, NULL, 0);
    printf("get_probe=%ld\n", probe);
    // 7. get into a too-small buffer -> ERANGE
    printf("get_small=%s\n", en((getxattr(path, N, buf, 1) < 0) ? errno : 0));
    // 8. list size-probe (size==0) -> total length includes our NAME + a NUL
    long lprobe = listxattr(path, NULL, 0);
    printf("list_has_name=%d\n", lprobe >= (long)strlen(N) + 1);
    // 9. list into a too-small buffer -> ERANGE
    printf("list_small=%s\n", en((listxattr(path, buf, 1) < 0) ? errno : 0));
    // 10. XATTR_CREATE|XATTR_REPLACE together -> EINVAL
    printf("create_and_replace=%s\n", rc(setxattr(path, N, "z", 1, XATTR_CREATE | XATTR_REPLACE)));
    // 11. overwrite (no flags) succeeds and round-trips the new value
    setxattr(path, N, "world!", 6, 0);
    long g2 = getxattr(path, N, buf, sizeof buf);
    printf("overwrite=%d\n", g2 == 6 && memcmp(buf, "world!", 6) == 0);
    // 12. remove the attr, then get -> ENODATA again
    printf("rm=%s\n", rc(removexattr(path, N)));
    printf("get_after_rm=%s\n", en((getxattr(path, N, buf, sizeof buf) < 0) ? errno : 0));

    unlink(path);
    return 0;
}
