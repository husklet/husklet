// #372 positive dentry/path-resolution cache: mutation<->lookup coherence storm.
// Runs INSIDE a container rootfs (the engine's path caches only exist under a jail). Every
// name->object mutation the cache must observe (create, unlink, rename, symlink flip, hardlink,
// deep dir-chain rename, rmdir+recreate-as-file, cross-process create/unlink) is interleaved with
// lookups (stat/lstat/open/readdir) that WOULD serve a stale result if any positive path/dentry
// entry survived its mutation. O_NOFOLLOW is probed right after a follow-mode open of the same
// symlink so a canonical-target cache entry can never leak into nofollow semantics (must ELOOP).
// 200 iterations double as a soak; prints one golden line, exit 0 only if every check held.
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int fails;
static void ck(int cond, const char *what, int i) {
    if (!cond) {
        fails++;
        if (fails <= 8) fprintf(stderr, "FAIL %s iter=%d errno=%d\n", what, i, errno);
    }
}
static char base[64];
static char pa[128], pb[128];
static const char *P(const char *leaf) { // absolute path under the per-run dir (alternating buffers)
    static int flip;
    char *b = (flip ^= 1) ? pa : pb;
    snprintf(b, 128, "%s/%s", base, leaf);
    return b;
}
static int exists(const char *p) {
    struct stat st;
    return stat(p, &st) == 0;
}
static int readback(const char *p, const char *want) {
    char b[64];
    int fd = open(p, O_RDONLY);
    if (fd < 0) return 0;
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n < 0) n = 0;
    b[n] = 0;
    return strcmp(b, want) == 0;
}
static void put(const char *p, const char *s) {
    int fd = open(p, O_CREAT | O_WRONLY | O_TRUNC, 0644);
    if (fd >= 0) {
        write(fd, s, strlen(s));
        close(fd);
    }
}

int main(void) {
    snprintf(base, sizeof base, "/tmp/ds_%d", (int)getpid()); // unique per run: no stale leftovers
    ck(mkdir(base, 0755) == 0, "mkdir-base", -1);
    for (int i = 0; i < 200; i++) {
        // negative -> create: the very next lookup must see the file
        ck(!exists(P("f")), "pre-absent", i);
        put(P("f"), "one");
        ck(readback(P("f"), "one"), "create-visible", i);
        // rename: old name gone, new name resolves to the same object
        ck(rename(P("f"), P("g")) == 0, "rename", i);
        ck(!exists(P("f")), "rename-oldgone", i);
        ck(readback(P("g"), "one"), "rename-newhas", i);
        // hardlink + unlink of the original: the alias must still read
        ck(link(P("g"), P("h")) == 0, "link", i);
        ck(unlink(P("g")) == 0, "unlink-orig", i);
        ck(!exists(P("g")), "unlink-gone", i);
        ck(readback(P("h"), "one"), "link-alias", i);
        ck(unlink(P("h")) == 0, "unlink-alias", i);
        // symlink flip: the same NAME must re-resolve to the new target on every flip
        put(P("a"), "A");
        put(P("b"), "B");
        ck(symlink("a", P("l")) == 0, "symlink", i);
        ck(readback(P("l"), "A"), "sym-follow-A", i); // follow-mode open (seeds any canonical cache)
        int nf = open(P("l"), O_RDONLY | O_NOFOLLOW);  // ...which must NOT let O_NOFOLLOW succeed
        ck(nf < 0 && errno == ELOOP, "nofollow-eloop", i);
        if (nf >= 0) close(nf);
        ck(unlink(P("l")) == 0, "sym-rm", i);
        ck(symlink("b", P("l")) == 0, "sym-reflip", i);
        ck(readback(P("l"), "B"), "sym-follow-B", i);
        struct stat lst;
        ck(lstat(P("l"), &lst) == 0 && S_ISLNK(lst.st_mode), "lstat-link", i);
        unlink(P("l"));
        unlink(P("a"));
        unlink(P("b"));
        // deep dir-chain rename: every cached prefix under the old name must die with it
        mkdir(P("x"), 0755);
        mkdir(P("x/y"), 0755);
        mkdir(P("x/y/z"), 0755);
        put(P("x/y/z/w"), "deep");
        ck(readback(P("x/y/z/w"), "deep"), "deep-create", i);
        ck(rename(P("x"), P("xx")) == 0, "dir-rename", i);
        ck(!exists(P("x/y/z/w")), "dir-rename-oldgone", i);
        ck(readback(P("xx/y/z/w"), "deep"), "dir-rename-newhas", i);
        // rmdir the leaf dir and recreate the NAME as a file: the object type must flip
        ck(unlink(P("xx/y/z/w")) == 0, "deep-unlink", i);
        ck(rmdir(P("xx/y/z")) == 0, "rmdir", i);
        put(P("xx/y/z"), "now-a-file");
        struct stat ts;
        ck(stat(P("xx/y/z"), &ts) == 0 && S_ISREG(ts.st_mode), "dir-to-file", i);
        ck(unlink(P("xx/y/z")) == 0, "file-rm", i);
        ck(rmdir(P("xx/y")) == 0, "rmdir-y", i);
        ck(rmdir(P("xx")) == 0, "rmdir-xx", i);
        // cross-process: a forked child's create/unlink must be visible to the parent immediately
        if (i % 20 == 0) {
            pid_t pid = fork();
            if (pid == 0) {
                put(P("xp"), "child");
                _exit(0);
            }
            int stx;
            waitpid(pid, &stx, 0);
            ck(readback(P("xp"), "child"), "xproc-create", i);
            pid = fork();
            if (pid == 0) {
                unlink(P("xp"));
                _exit(0);
            }
            waitpid(pid, &stx, 0);
            ck(!exists(P("xp")), "xproc-unlink", i);
        }
    }
    // readdir: a removed entry must never linger in a fresh listing
    for (int i = 0; i < 8; i++) {
        char p[96];
        snprintf(p, sizeof p, "%s/r%d", base, i);
        put(p, "r");
    }
    unlink(P("r3"));
    int seen = 0, ghost = 0;
    DIR *d = opendir(base);
    struct dirent *e;
    while (d && (e = readdir(d))) {
        if (e->d_name[0] == 'r') {
            seen++;
            if (!strcmp(e->d_name, "r3")) ghost = 1;
        }
    }
    if (d) closedir(d);
    for (int i = 0; i < 8; i++) {
        char p[96];
        snprintf(p, sizeof p, "%s/r%d", base, i);
        unlink(p);
    }
    rmdir(base);
    printf("dentry-storm iters=200 readdir=%d ghost=%d fails=%d\n", seen, ghost, fails);
    return fails ? 1 : 0;
}
