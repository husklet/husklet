// Extended attributes round-trip: set/get/list/remove a user xattr on a temp file.
// Portable: the darwin setxattr/getxattr take an extra position+options pair, so branch on __APPLE__.
// Verdict-only (no raw errno/paths), so the same golden line must appear emulated-on-Linux and native-mac.
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#ifdef __APPLE__
#include <sys/xattr.h>
#define XSET(p,n,v,s)   setxattr(p, n, v, s, 0, 0)
#define XGET(p,n,v,s)   getxattr(p, n, v, s, 0, 0)
#define XLIST(p,b,s)    listxattr(p, b, s, 0)
#define XREMOVE(p,n)    removexattr(p, n, 0)
#define XNAME           "user.hltest"
#else
#include <sys/xattr.h>
#define XSET(p,n,v,s)   setxattr(p, n, v, s, 0)
#define XGET(p,n,v,s)   getxattr(p, n, v, s)
#define XLIST(p,b,s)    listxattr(p, b, s)
#define XREMOVE(p,n)    removexattr(p, n)
#define XNAME           "user.hltest"
#endif

int main(void) {
    char path[128];
    snprintf(path, sizeof path, "/tmp/hl_xattr_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "body", 4);
    close(fd);

    int set = XSET(path, XNAME, "hello", 5) == 0;
    char buf[64] = {0};
    long got = XGET(path, XNAME, buf, sizeof buf);
    int roundtrip = got == 5 && memcmp(buf, "hello", 5) == 0;
    char list[256] = {0};
    long ln = XLIST(path, list, sizeof list);
    // listxattr returns a NUL-separated name list; our name must appear in it.
    int listed = 0;
    for (long i = 0; i < ln; ) { if (strcmp(list + i, XNAME) == 0) { listed = 1; break; } i += strlen(list + i) + 1; }
    int removed = XREMOVE(path, XNAME) == 0;
    int gone = XGET(path, XNAME, buf, sizeof buf) < 0;
    unlink(path);
    printf("xattr set=%d roundtrip=%d listed=%d removed=%d gone=%d\n", set, roundtrip, listed, removed, gone);
    return 0;
}
