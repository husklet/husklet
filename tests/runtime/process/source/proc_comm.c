// procexe/selfexe.c -- the whole /proc/self/exe + readlink/readlinkat surface, differential vs the
// native oracle (#370/#317). Two modes:
//   (no args)      the oracle-diffed matrix: /proc/self/exe (readlink/open/stat/lstat/access, pid
//                  aliases, dirfd + cwd-relative spellings), /proc/self/fd/N link text (file/pipe/
//                  socket/closed), cwd/root/mounts/ns magic links, EINVAL/ENOENT/EBADF/bufsiz
//                  semantics, real-symlink readlink vs readlinkat consistency incl. truncation, and
//                  SEVEN re-exec stages (execve of /proc/self/exe, a relative path, a symlink, a
//                  shebang script whose interpreter is this binary, and execveat abs/dirfd-relative/
//                  AT_EMPTY_PATH) each asserting the child's /proc/self/exe is the absolute CANONICAL
//                  binary path. All output is boolean/normalized so it is byte-exact vs the oracle on
//                  both arches (paths and inodes never printed).
//   comm           golden (JIT-only) comm fidelity: Linux sets comm from the LAST component of the
//                  path PASSED to execve (so /proc/self/exe -> "exe", ./x -> "x", a script keeps its
//                  own name, not the interpreter's). Not oracle-diffed: under the qemu-x86 oracle the
//                  host kernel/binfmt sets comm differently, so the truth here is native-Linux
//                  semantics (verified against a native aarch64 run) enforced as a golden.
// Stages re-enter main with argv[1] = "s2:<name>" (exe check) or "s2c:<name>" (comm print); the
// expected canonical self path travels in $HL_T_EXP (execve must forward the guest environment).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

static int rl(const char *p, char *out, size_t n) { // readlink + NUL-terminate; -1 on error
    ssize_t r = readlink(p, out, n - 1);
    if (r < 0) return -1;
    out[r] = 0;
    return (int)r;
}
static int rlat(int dfd, const char *p, char *out, size_t n) {
    ssize_t r = readlinkat(dfd, p, out, n - 1);
    if (r < 0) return -1;
    out[r] = 0;
    return (int)r;
}
static void read_comm(char *out, size_t n) {
    out[0] = 0;
    int f = open("/proc/self/comm", O_RDONLY);
    if (f < 0) return;
    ssize_t r = read(f, out, n - 1);
    close(f);
    if (r < 0) r = 0;
    out[r] = 0;
    char *nl = strchr(out, '\n');
    if (nl) *nl = 0;
}
// child stage: assert /proc/self/exe == $HL_T_EXP (absolute canonical), or print comm
static int stage(const char *tag) {
    const char *name = tag + (tag[2] == ':' ? 3 : 4);
    char e[PATH_MAX];
    if (!strncmp(tag, "s2c:", 4)) {
        char c[64];
        read_comm(c, sizeof c);
        printf("stage %s comm=%s\n", name, c);
        return 0;
    }
    const char *exp = getenv("HL_T_EXP");
    int ok = rl("/proc/self/exe", e, sizeof e) > 0 && exp && !strcmp(e, exp);
    printf("stage %s exe=%d\n", name, ok);
    return 0;
}
// fork + exec one stage; parent waits so output order is deterministic. kind: 0=execv(path),
// 1=chdir(dir-of-self)+execv(./base), 2=execveat(AT_FDCWD, path, 0), 3=execveat(dirfd-of-self, base, 0),
// 4=execveat(open(self), "", AT_EMPTY_PATH)
static void run_stage(int kind, const char *path, const char *self, const char *tag) {
    fflush(stdout);
    pid_t p = fork();
    if (p == 0) {
        char *na[] = {"x", (char *)tag, NULL};
        if (kind == 0) execv(path, na);
        else if (kind == 1) {
            char dir[PATH_MAX];
            snprintf(dir, sizeof dir, "%s", self);
            char *sl = strrchr(dir, '/');
            *sl = 0;
            if (chdir(dir) == 0) {
                char rel[PATH_MAX + 8];
                snprintf(rel, sizeof rel, "./%s", sl + 1);
                execv(rel, na);
            }
        } else if (kind == 2)
            syscall(SYS_execveat, AT_FDCWD, path, na, environ, 0);
        else if (kind == 3) {
            char dir[PATH_MAX];
            snprintf(dir, sizeof dir, "%s", self);
            char *sl = strrchr(dir, '/');
            *sl = 0;
            int dfd = open(dir, O_RDONLY | O_DIRECTORY);
            if (dfd >= 0) syscall(SYS_execveat, dfd, sl + 1, na, environ, 0);
        } else if (kind == 4) {
            int fd = open(self, O_RDONLY);
            if (fd >= 0) syscall(SYS_execveat, fd, "", na, environ, 0x1000 /*AT_EMPTY_PATH*/);
        }
        printf("stage %s execfail errno=%d\n", tag, errno);
        fflush(stdout);
        _exit(1);
    }
    int st;
    waitpid(p, &st, 0);
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc > 1 && !strncmp(argv[1], "s2", 2)) return stage(argv[1]);
    int commmode = argc > 1 && !strcmp(argv[1], "comm");

    char self[PATH_MAX];
    if (!realpath(argv[0], self)) {
        printf("procexe: realpath(argv0) failed\n");
        return 1;
    }
    setenv("HL_T_EXP", self, 1);
    char td[] = "/tmp/procexeXXXXXX";
    if (!mkdtemp(td)) {
        printf("procexe: mkdtemp failed\n");
        return 1;
    }
    char tdr[PATH_MAX]; // macOS /tmp is a symlink; compare against the canonical dir on both sides
    if (!realpath(td, tdr)) snprintf(tdr, sizeof tdr, "%s", td);

    char e[PATH_MAX], b[PATH_MAX], t[PATH_MAX];
    struct stat sself, s2;
    stat(self, &sself);

    if (commmode) {
        char c[64];
        read_comm(c, sizeof c);
        printf("commchk self=%s\n", c);
        run_stage(0, "/proc/self/exe", self, "s2c:proc");
        run_stage(1, NULL, self, "s2c:rel");
        char lnk[PATH_MAX + 32];
        snprintf(lnk, sizeof lnk, "%s/lnk-selfexe", tdr);
        symlink(self, lnk);
        run_stage(0, lnk, self, "s2c:lnk");
        char script[PATH_MAX + 16];
        snprintf(script, sizeof script, "%s/shb.sh", tdr);
        FILE *f = fopen(script, "w");
        if (f) {
            fprintf(f, "#!%s s2c:shb\n", self);
            fclose(f);
            chmod(script, 0755);
        }
        fflush(stdout);
        pid_t p = fork();
        if (p == 0) {
            char *na[] = {script, NULL};
            execv(script, na);
            printf("stage shb execfail errno=%d\n", errno);
            _exit(1);
        }
        int st;
        waitpid(p, &st, 0);
        // cleanup
        unlink(lnk);
        unlink(script);
        rmdir(td);
        printf("commchk done\n");
        return 0;
    }

    // ---- 1. /proc/self/exe: readlink spellings + open/stat/lstat/access through the link ----
    int n = rl("/proc/self/exe", e, sizeof e);
    int abs_ok = n > 0 && e[0] == '/';
    int canon = n > 0 && !strcmp(e, self);
    char pidp[64];
    snprintf(pidp, sizeof pidp, "/proc/%d/exe", (int)getpid());
    int alias = rl(pidp, b, sizeof b) > 0 && !strcmp(b, e);
    int tself = rl("/proc/thread-self/exe", b, sizeof b) > 0 && !strcmp(b, e);
    int dirfd_ok = 0;
    {
        int dfd = open("/proc/self", O_RDONLY | O_DIRECTORY);
        if (dfd >= 0) {
            dirfd_ok = rlat(dfd, "exe", b, sizeof b) > 0 && !strcmp(b, e);
            close(dfd);
        }
    }
    int cwdrel = 0;
    if (chdir("/") == 0) cwdrel = rlat(AT_FDCWD, "proc/self/exe", b, sizeof b) > 0 && !strcmp(b, e);
    char num[32];
    snprintf(num, sizeof num, "%d", (int)getpid());
    int selfpid = rl("/proc/self", b, sizeof b) > 0 && !strcmp(b, num);
    int open_ok = 0;
    {
        int fd = open("/proc/self/exe", O_RDONLY);
        if (fd >= 0) {
            open_ok = fstat(fd, &s2) == 0 && s2.st_dev == sself.st_dev && s2.st_ino == sself.st_ino;
            close(fd);
        }
    }
    int lst = lstat("/proc/self/exe", &s2) == 0 && S_ISLNK(s2.st_mode) && s2.st_size == 0;
    int st_ok = stat("/proc/self/exe", &s2) == 0 && s2.st_dev == sself.st_dev && s2.st_ino == sself.st_ino;
    int acc = access("/proc/self/exe", X_OK) == 0;
    printf("exe abs=%d canon=%d alias=%d tself=%d dirfd=%d cwdrel=%d selfpid=%d open=%d lstat=%d stat=%d access=%d\n",
           abs_ok, canon, alias, tself, dirfd_ok, cwdrel, selfpid, open_ok, lst, st_ok, acc);

    // ---- 2. /proc/self/fd/N link text: regular file, pipe, socket, closed fd ----
    int file_ok = 0, pipe_ok = 0, sock_ok = 0, closed_ok = 0;
    {
        int fd = open(self, O_RDONLY);
        char fp[64];
        snprintf(fp, sizeof fp, "/proc/self/fd/%d", fd);
        file_ok = fd >= 0 && rl(fp, b, sizeof b) > 0 && !strcmp(b, self);
        int pp[2];
        if (pipe(pp) == 0) {
            snprintf(fp, sizeof fp, "/proc/self/fd/%d", pp[0]);
            pipe_ok = rl(fp, b, sizeof b) > 0 && !strncmp(b, "pipe:[", 6);
            close(pp[0]);
            close(pp[1]);
        }
        int sv[2];
        if (socketpair(AF_UNIX, SOCK_STREAM, 0, sv) == 0) {
            snprintf(fp, sizeof fp, "/proc/self/fd/%d", sv[0]);
            sock_ok = rl(fp, b, sizeof b) > 0 && !strncmp(b, "socket:[", 8);
            close(sv[0]);
            close(sv[1]);
        }
        int dupped = dup(fd);
        close(dupped);
        snprintf(fp, sizeof fp, "/proc/self/fd/%d", dupped);
        closed_ok = rl(fp, b, sizeof b) < 0 && errno == ENOENT;
        close(fd);
    }
    printf("fdlink file=%d pipe=%d sock=%d closed=%d\n", file_ok, pipe_ok, sock_ok, closed_ok);

    // ---- 3. cwd/root/mounts/ns magic links + EINVAL/bufsiz semantics on /proc ----
    int cwd_ok = 0;
    if (chdir(tdr) == 0) {
        char cw[PATH_MAX];
        cwd_ok = getcwd(cw, sizeof cw) && rl("/proc/self/cwd", b, sizeof b) > 0 && !strcmp(b, cw);
    }
    int root_ok = rl("/proc/self/root", b, sizeof b) > 0 && !strcmp(b, "/");
    int mounts = rl("/proc/mounts", b, sizeof b) > 0 && !strcmp(b, "self/mounts");
    int ns_ok = rl("/proc/self/ns/net", b, sizeof b) > 0 && !strncmp(b, "net:[", 5);
    int einval = rl("/proc/self/status", b, sizeof b) < 0 && errno == EINVAL;
    int eproc = rl("/proc", b, sizeof b) < 0 && errno == EINVAL;
    int zerobuf = readlink("/proc/self/exe", b, 0) < 0 && errno == EINVAL;
    printf("magic cwd=%d root=%d mounts=%d ns=%d einval=%d eproc=%d zerobuf=%d\n", cwd_ok, root_ok,
           mounts, ns_ok, einval, eproc, zerobuf);

    // ---- 4. real symlinks: readlink vs readlinkat byte-exact, truncation, error cases ----
    // all created inside tdr; cwd moves around to prove dirfd/cwd independence (#317)
    char lp[PATH_MAX + 32];
    snprintf(lp, sizeof lp, "%s/target-rel", tdr);
    int tf = open(lp, O_CREAT | O_WRONLY, 0644);
    if (tf >= 0) close(tf);
    snprintf(lp, sizeof lp, "%s/link", tdr);
    symlink("target-rel", lp);
    int s_rel = rl(lp, b, sizeof b) == 10 && !strcmp(b, "target-rel");
    int s_dirfd = 0;
    {
        if (chdir("/") != 0) {} // ensure the engine cwd is NOT tdr: pre-#317 this made dirfd readlink fail
        int dfd = open(tdr, O_RDONLY | O_DIRECTORY);
        if (dfd >= 0) {
            s_dirfd = rlat(dfd, "link", b, sizeof b) == 10 && !strcmp(b, "target-rel");
            close(dfd);
        }
    }
    int s_cwd = chdir(tdr) == 0 && rl("link", b, sizeof b) == 10 && !strcmp(b, "target-rel");
    snprintf(lp, sizeof lp, "%s/labs", tdr);
    symlink("/no/where/abs-target", lp);
    int s_abs = rl(lp, b, sizeof b) == 20 && !strcmp(b, "/no/where/abs-target");
    char longt[301];
    memset(longt, 'a', 300);
    longt[300] = 0;
    longt[0] = '/';
    snprintf(lp, sizeof lp, "%s/llong", tdr);
    symlink(longt, lp);
    int s_long = rl(lp, t, sizeof t) == 300 && !strcmp(t, longt);
    int s_trunc = 0, s_zero = 0;
    {
        memset(b, 0x7f, sizeof b);
        ssize_t r = readlink(lp, b, 11); // truncation: returns bufsiz, no NUL, nothing past it
        s_trunc = r == 11 && !memcmp(b, longt, 11) && b[11] == 0x7f;
        s_zero = readlink(lp, b, 0) < 0 && errno == EINVAL;
    }
    snprintf(lp, sizeof lp, "%s/ldang", tdr);
    symlink("no-such-file-xyz", lp);
    int s_dangle = rl(lp, b, sizeof b) == 16 && open(lp, O_RDONLY) < 0 && errno == ENOENT;
    snprintf(lp, sizeof lp, "%s/target-rel", tdr);
    int s_efile = rl(lp, b, sizeof b) < 0 && errno == EINVAL;
    int s_edir = rl(tdr, b, sizeof b) < 0 && errno == EINVAL;
    snprintf(lp, sizeof lp, "%s/no-ent-xyz", tdr);
    int s_enoent = rl(lp, b, sizeof b) < 0 && errno == ENOENT;
    int s_ebadf = readlinkat(12345, "rel", b, sizeof b) < 0 && errno == EBADF;
    printf("sym rel=%d dirfd=%d cwd=%d abs=%d long=%d trunc=%d zero=%d dangle=%d efile=%d edir=%d enoent=%d ebadf=%d\n",
           s_rel, s_dirfd, s_cwd, s_abs, s_long, s_trunc, s_zero, s_dangle, s_efile, s_edir, s_enoent, s_ebadf);

    // ---- 5. re-exec stages: /proc/self/exe must name the new image, absolute + canonical ----
    run_stage(0, "/proc/self/exe", self, "s2:proc"); // busybox re-exec shape (#370)
    run_stage(1, NULL, self, "s2:rel");              // relative exec: the dl-origin.c assert shape
    char lnk[PATH_MAX + 32];
    snprintf(lnk, sizeof lnk, "%s/lnk-selfexe", tdr);
    symlink(self, lnk);
    run_stage(0, lnk, self, "s2:lnk"); // exec via symlink: exe = the TARGET, like Linux d_path
    char script[PATH_MAX + 16];
    snprintf(script, sizeof script, "%s/shb.sh", tdr);
    FILE *f = fopen(script, "w");
    if (f) {
        fprintf(f, "#!%s s2:shb\n", self); // interpreter = this binary; exe must name the INTERPRETER
        fclose(f);
        chmod(script, 0755);
    }
    fflush(stdout);
    pid_t sp = fork();
    if (sp == 0) {
        char *na[] = {script, NULL};
        execv(script, na);
        printf("stage shb execfail errno=%d\n", errno);
        _exit(1);
    }
    int st;
    waitpid(sp, &st, 0);
    run_stage(2, "/proc/self/exe", self, "s2:at"); // execveat(AT_FDCWD, /proc/self/exe)
    run_stage(3, NULL, self, "s2:atrel");          // execveat(dirfd, relative-basename)
    run_stage(4, NULL, self, "s2:empty");          // execveat(fd, "", AT_EMPTY_PATH) == fexecve

    // cleanup (best effort)
    char rm[PATH_MAX + 32];
    const char *ents[] = {"target-rel", "link", "labs", "llong", "ldang", "lnk-selfexe", "shb.sh", NULL};
    for (int i = 0; ents[i]; i++) {
        snprintf(rm, sizeof rm, "%s/%s", tdr, ents[i]);
        unlink(rm);
    }
    rmdir(td);
    printf("procexe done\n");
    return 0;
}
