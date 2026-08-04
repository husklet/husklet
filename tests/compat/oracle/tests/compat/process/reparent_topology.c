// Process-tree topology: orphan reparenting to the nearest subreaper and to
// init, /proc parentage consistency after reparent, init harvesting of orphans,
// and the wait-flag rules that bound which tasks a caller may reap. Deterministic
// pipe handshakes drive every step; the native aarch64 run is the oracle and both
// production engines must byte-match the reviewed golden.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

// ppid is the 4th field of /proc/<pid>/stat, read after the final ')' so a comm
// containing spaces or parentheses cannot shift the parse.
static long proc_ppid(pid_t pid) {
    char path[64];
    snprintf(path, sizeof path, "/proc/%d/stat", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    char buffer[512];
    ssize_t n = read(fd, buffer, sizeof buffer - 1);
    close(fd);
    if (n <= 0) return -1;
    buffer[n] = 0;
    char *rparen = strrchr(buffer, ')');
    if (!rparen) return -1;
    char state;
    long ppid = -1;
    sscanf(rparen + 2, "%c %ld", &state, &ppid);
    return ppid;
}
static char proc_state(pid_t pid) {
    char path[64];
    snprintf(path, sizeof path, "/proc/%d/stat", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 'X';
    char buffer[512];
    ssize_t n = read(fd, buffer, sizeof buffer - 1);
    close(fd);
    if (n <= 0) return 'X';
    buffer[n] = 0;
    char *rparen = strrchr(buffer, ')');
    if (!rparen) return 'X';
    char state = '?';
    sscanf(rparen + 2, "%c", &state);
    return state;
}
static int proc_exists(pid_t pid) {
    char path[64];
    snprintf(path, sizeof path, "/proc/%d/stat", pid);
    int fd = open(path, O_RDONLY);
    if (fd < 0) return 0;
    close(fd);
    return 1;
}

// Nearest subreaper wins: A(subreaper) -> B(subreaper) -> C -> D; C exits, so D
// reparents to B (the nearest marked ancestor), reports B as its parent, /proc
// agrees, and B's own waitpid(-1) harvests D.
static void test_nearest_subreaper(void) {
    int report[2], go[2], bpid[2];
    if (pipe(report) || pipe(go) || pipe(bpid)) return;
    prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    fflush(stdout);
    pid_t b = fork();
    if (b == 0) {
        prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
        close(bpid[0]);
        pid_t c = fork();
        if (c == 0) {
            pid_t d = fork();
            if (d == 0) {
                close(go[1]);
                close(report[0]);
                char proceed;
                (void)read(go[0], &proceed, 1); // wait until C is reaped
                char line[32];
                int len = snprintf(line, sizeof line, "%ld\n", (long)getppid());
                (void)write(report[1], line, len);
                _exit(0);
            }
            _exit(0); // C orphans D -> nearest subreaper is B
        }
        waitpid(c, NULL, 0);
        char line[32];
        int len = snprintf(line, sizeof line, "%ld\n", (long)getpid());
        (void)write(bpid[1], line, len);   // tell A which pid B is
        pid_t harvested = waitpid(-1, NULL, 0); // B reaps reparented D
        char done[8];
        int dlen = snprintf(done, sizeof done, "%d\n", harvested > 0);
        (void)write(bpid[1], done, dlen);
        _exit(0);
    }
    close(bpid[1]);
    close(go[0]);
    close(report[1]);
    char pid_line[64];
    ssize_t n = read(bpid[0], pid_line, sizeof pid_line - 1);
    if (n < 0) n = 0;
    pid_line[n] = 0;
    long b_pid = atol(pid_line);
    (void)write(go[1], "g", 1);
    char report_line[32];
    ssize_t rn = read(report[0], report_line, sizeof report_line - 1);
    if (rn < 0) rn = 0;
    report_line[rn] = 0;
    long d_ppid = atol(report_line);
    char reap_line[16];
    ssize_t hn = read(bpid[0], reap_line, sizeof reap_line - 1);
    if (hn < 0) hn = 0;
    reap_line[hn] = 0;
    printf("nearest subreaper: D.getppid==B=%d B harvested D=%d\n",
           d_ppid == b_pid, atoi(reap_line) == 1);
    waitpid(b, NULL, 0);
}

// Double-fork under an explicit subreaper: A(subreaper) -> B(setsid) -> C; B
// exits, so the daemonized grandchild C reparents onto A. getppid and
// /proc/<pid>/stat must agree on the new parent, and A must harvest C. Using an
// explicit subreaper (rather than trusting the ambient session's reaper) keeps
// the observed parent deterministic across environments.
static void test_daemon_reparent(void) {
    int report[2], go[2];
    if (pipe(report) || pipe(go)) return;
    prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0);
    fflush(stdout);
    pid_t b = fork();
    if (b == 0) {
        setsid();
        pid_t c = fork();
        if (c == 0) {
            close(go[1]);
            close(report[0]);
            char proceed;
            (void)read(go[0], &proceed, 1);
            char line[48];
            int len = snprintf(line, sizeof line, "%d %ld\n", getpid(), (long)getppid());
            (void)write(report[1], line, len);
            _exit(0);
        }
        _exit(0);
    }
    close(go[0]);
    close(report[1]);
    waitpid(b, NULL, 0);
    (void)write(go[1], "g", 1);
    char line[64];
    ssize_t n = read(report[0], line, sizeof line - 1);
    if (n < 0) n = 0;
    line[n] = 0;
    pid_t grandchild = 0;
    long gc_ppid = 0;
    sscanf(line, "%d %ld", &grandchild, &gc_ppid);
    long proc_view = proc_ppid(grandchild);
    pid_t harvested = waitpid(grandchild, NULL, 0);
    printf("daemon grandchild: getppid==A=%d /proc ppid matches=%d subreaper harvested=%d\n",
           gc_ppid == (long)getpid(), proc_view == gc_ppid, harvested == grandchild);
}

// A caller may only wait on its own children: a grandchild and a sibling thread
// are both non-children and yield ECHILD.
static volatile long g_thread_tid;
static void *tid_reporter(void *arg) {
    (void)arg;
    g_thread_tid = syscall(SYS_gettid);
    struct timespec tick = {0, 30000000};
    nanosleep(&tick, NULL);
    return NULL;
}
static void test_wait_non_children(void) {
    int handoff[2];
    if (pipe(handoff) != 0) return;
    fflush(stdout);
    pid_t b = fork();
    if (b == 0) {
        pid_t c = fork();
        if (c == 0) {
            pause();
            _exit(0);
        }
        char line[32];
        int len = snprintf(line, sizeof line, "%d\n", c);
        (void)write(handoff[1], line, len);
        char done;
        (void)read(handoff[0], &done, 1);
        kill(c, SIGKILL);
        waitpid(c, NULL, 0);
        _exit(0);
    }
    char line[32];
    ssize_t n = read(handoff[0], line, sizeof line - 1);
    if (n < 0) n = 0;
    line[n] = 0;
    pid_t grandchild = atoi(line);
    errno = 0;
    pid_t r = waitpid(grandchild, NULL, 0);
    int grandchild_echild = (r == -1 && errno == ECHILD);
    (void)write(handoff[1], "x", 1);
    waitpid(b, NULL, 0);

    // A sibling thread's tid is not a waitable child even with __WALL.
    fflush(stdout);
    pid_t d = fork();
    if (d == 0) {
        pthread_t t;
        g_thread_tid = 0;
        pthread_create(&t, NULL, tid_reporter, NULL);
        while (g_thread_tid == 0) { }
        errno = 0;
        pid_t rr = waitpid((pid_t)g_thread_tid, NULL, __WALL | WNOHANG);
        int echild = (rr == -1 && errno == ECHILD);
        pthread_join(t, NULL);
        _exit(echild ? 0 : 1);
    }
    int status = 0;
    waitpid(d, &status, 0);
    printf("wait grandchild ECHILD=%d wait sibling-thread ECHILD=%d\n",
           grandchild_echild, WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

// Wait-flag topology: __WCLONE must not match a SIGCHLD (fork) child while __WALL
// does; a zombie shows state Z and vanishes (ENOENT) once reaped; waitpid(-pgid)
// reaps any child in the named process group.
static void test_wait_flags(void) {
    fflush(stdout);
    pid_t a = fork();
    if (a == 0) _exit(5);
    int status = 0;
    errno = 0;
    pid_t r = waitpid(a, &status, __WCLONE | WNOHANG);
    int wclone_no_match = (r == 0) || (r == -1 && errno == ECHILD);
    pid_t r2 = waitpid(a, &status, __WALL);
    printf("__WCLONE skips fork child=%d __WALL reaps code==5=%d\n",
           wclone_no_match, r2 == a && WEXITSTATUS(status) == 5);

    int gate[2];
    if (pipe(gate) != 0) return;
    fflush(stdout);
    pid_t z = fork();
    if (z == 0) {
        close(gate[1]);
        char go;
        (void)read(gate[0], &go, 1);
        _exit(3);
    }
    close(gate[0]);
    (void)write(gate[1], "x", 1);
    char st = '?';
    for (int i = 0; i < 100000; i++) {
        st = proc_state(z);
        if (st == 'Z') break;
    }
    waitpid(z, &status, 0);
    errno = 0;
    int gone = !proc_exists(z);
    printf("zombie state Z=%d reaped code==3=%d gone after reap=%d\n",
           st == 'Z', WEXITSTATUS(status) == 3, gone);

    fflush(stdout);
    pid_t g = fork();
    if (g == 0) {
        setpgid(0, 0);
        _exit(11);
    }
    setpgid(g, g);
    pid_t rg = waitpid(-g, &status, 0);
    printf("waitpid(-pgid) reaps group child=%d code==11=%d\n",
           rg == g, WEXITSTATUS(status) == 11);
}

int main(void) {
    test_nearest_subreaper();
    test_daemon_reparent();
    test_wait_non_children();
    test_wait_flags();
    return 0;
}
