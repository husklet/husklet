// Close-on-exec across a guest execve, under the untrusted-guest SENTRY split. A guest execve stays
// worker-local (the image is re-loaded in place, not a real host execve), so the FD_CLOEXEC descriptors
// the kernel would close must be closed by hand -- and in sentry mode those are VIRTUAL fds owned by the
// sentry process. Before the fix the sentry never swept them, so a pipe write end opened O_CLOEXEC leaked
// past the child's execve: its peer never saw EOF.
//
// Probe: parent makes a CLOEXEC pipe and a PLAIN pipe, forks, and the child execve()s itself into a
// blocking "child" mode. The parent drops its own write ends, so only a CHILD-side leak can keep a pipe
// open. Correct semantics: the CLOEXEC write end is closed by the child's exec (parent reads EOF), the
// PLAIN write end survives (parent's read would block). ok=1 iff both hold. Oracle-diffed vs native, so
// the native Linux kernel's own close-on-exec behaviour is the expected value.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/resource.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

// Model the topology used by the MySQL image entrypoint without depending on a particular shell build:
// a privilege-drop helper re-execs Bash, Bash forks the last pipeline element to invoke a function, and
// that function passes a process-substitution descriptor to an exec'd client.  Descriptor operations are
// deliberately use Bash's dup2-to-high-fd operation followed by F_SETFD to make the selected descriptor
// survive exec.  The outer pipeline child also closes its temporary descriptors before exec.
static void shell_client_mode(const char *self, int fd, int open_stage) {
    if (!open_stage) {
        char fd_text[16];
        snprintf(fd_text, sizeof fd_text, "%d", fd);
        char *av[] = {(char *)self, "shell-client-open", fd_text, NULL};
        execve("/proc/self/exe", av, environ);
        _exit(127);
    }
    char config[16] = {0};
    char input[16] = {0};
    char path[32];
    snprintf(path, sizeof path, "/dev/fd/%d", fd);
    int reopened = open(path, O_RDONLY | O_CLOEXEC);
    ssize_t config_size = reopened >= 0 ? read(reopened, config, sizeof config) : -1;
    ssize_t input_size = read(STDIN_FILENO, input, sizeof input);
    if (reopened >= 0) close(reopened);
    _exit(config_size == 0 && input_size == 5 && memcmp(input, "zones", 5) == 0
              ? 0
              : 1);
}

static int shell_pipeline(const char *self) {
    int pipeline[2];
    if (pipe2(pipeline, O_CLOEXEC) != 0) return 0;

    pid_t upstream = fork();
    if (upstream == 0) {
        close(pipeline[0]);
        ssize_t size = write(pipeline[1], "zones", 5);
        close(pipeline[1]);
        _exit(size == 5 ? 0 : 1);
    }

    pid_t function = fork();
    if (function == 0) {
        close(pipeline[1]);
        if (dup2(pipeline[0], STDIN_FILENO) < 0) _exit(2);
        close(pipeline[0]);

        int substitution[2];
        if (pipe2(substitution, O_CLOEXEC) != 0) _exit(3);
        // Bash's move_to_high_fd scans down from 63 and uses dup2, rather than F_DUPFD.  Keep this path
        // distinct from the earlier generic fcntl duplication checks.
        int client_fd = dup2(substitution[0], 63);
        if (client_fd != 63 || fcntl(client_fd, F_SETFD, 0) != 0) _exit(4);
        close(substitution[0]);

        pid_t producer = fork();
        if (producer == 0) {
            close(client_fd);
            // `_mysql_passfile --dont-use-mysql-root-password` intentionally produces no bytes.  Its
            // immediate exit must not retire the sibling client's inherited read description.
            close(substitution[1]);
            _exit(0);
        }
        close(substitution[1]);
        usleep(50000); // make the empty producer's exit/reap precede the client exec deterministically

        char fd_text[16];
        snprintf(fd_text, sizeof fd_text, "%d", client_fd);
        char *av[] = {(char *)self, "shell-client", fd_text, NULL};
        execve("/proc/self/exe", av, environ);
        _exit(127);
    }

    close(pipeline[0]);
    close(pipeline[1]);
    int upstream_status = 0;
    int function_status = 0;
    waitpid(upstream, &upstream_status, 0);
    waitpid(function, &function_status, 0);
    if (!WIFEXITED(upstream_status) || WEXITSTATUS(upstream_status) != 0) return -1;
    if (!WIFEXITED(function_status)) return -2;
    return WEXITSTATUS(function_status) == 0 ? 1 : 10 + WEXITSTATUS(function_status);
}

// child mode: stay alive (holding whatever fds survived our execve) until the parent kills us.
static void child_mode(int argc, char **argv) {
    if (argc > 2 && strcmp(argv[1], "check") == 0) {
        int fd = atoi(argv[2]);
        char path[32];
        struct stat direct, projected, nofollow, proc_projected, relative;
        struct statx extended;
        char link[128];
        snprintf(path, sizeof path, "/dev/fd/%d", fd);
        int reopened = open(path, O_RDONLY | O_CLOEXEC);
        char payload[8] = {0};
        ssize_t payload_size = reopened >= 0 ? read(reopened, payload, sizeof payload) : -1;
        int descriptor_ok = fcntl(fd, F_GETFD) >= 0;
        int direct_ok = fstat(fd, &direct) == 0;
        int projected_ok = stat(path, &projected) == 0;
        int nofollow_ok = fstatat(AT_FDCWD, path, &nofollow, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(nofollow.st_mode);
        char proc_path[40];
        snprintf(proc_path, sizeof proc_path, "/proc/self/fd/%d", fd);
        int proc_ok = stat(proc_path, &proc_projected) == 0;
        int proc_directory = open("/proc/self/fd", O_RDONLY | O_DIRECTORY | O_CLOEXEC);
        char leaf[16];
        snprintf(leaf, sizeof leaf, "%d", fd);
        int relative_ok = proc_directory >= 0 && fstatat(proc_directory, leaf, &relative, 0) == 0;
        if (proc_directory >= 0) close(proc_directory);
        int statx_ok = syscall(SYS_statx, AT_FDCWD, path, 0, STATX_BASIC_STATS, &extended) == 0;
        int link_ok = readlink(path, link, sizeof link) > 0;
        int access_ok = access(path, R_OK) == 0;
        int ok = descriptor_ok && direct_ok && projected_ok && nofollow_ok && proc_ok && relative_ok && statx_ok &&
                 link_ok && access_ok && reopened >= 0 &&
                 payload_size == 7 && memcmp(payload, "client\n", 7) == 0 &&
                 (direct.st_mode & S_IFMT) == (projected.st_mode & S_IFMT);
        if (!ok)
            dprintf(STDERR_FILENO,
                    "procfd-check fd=%d checks=%d%d%d%d%d%d%d%d%d open=%d read=%zd errno=%d\n", fd,
                    descriptor_ok, direct_ok, projected_ok, nofollow_ok, proc_ok, relative_ok, statx_ok, link_ok,
                    access_ok, reopened, payload_size, errno);
        if (reopened >= 0) close(reopened);
        _exit(ok ? 0 : 1);
    }
    pause();
    _exit(0);
}

// Nonblocking read of a pipe read end, polled until EOF or the budget runs out. Returns 1 iff EOF (every
// write end is closed), 0 if a write end is still open (EAGAIN throughout) or unexpected data arrived.
//
// The budget is deliberately enormous relative to the work. What is being waited for is the child's
// execve completing so the engine sweeps its CLOEXEC descriptors; that takes ~10ms here, so the previous
// 600ms reads like a comfortable 60x margin while really being an unbounded wall-clock deadline.
//
// Widening it does NOT by itself make this case reliable, and should not be mistaken for a fix: under
// heavy parallel load the case still fails with parts=01111 after the full budget, which is a genuine
// unswept descriptor rather than a slow exec (the sentry trace shows the poll loop burning every
// iteration). What the wider budget buys is separation of the two failure modes -- anything that fails
// now has actually leaked. A leaked write end never yields EOF, so a real leak still fails; it just
// takes the full budget to say so.
static int drained_to_eof(int rfd) {
    char b[8];
    for (int i = 0; i < 1000; i++) { // 1000 * 10ms = 10s
        ssize_t r = read(rfd, b, sizeof b);
        if (r == 0) return 1;
        if (r < 0 && errno == EAGAIN) {
            usleep(10000);
            continue;
        }
        return 0; // data, or an unexpected error
    }
    return 0; // never reached EOF -> a write end is still open (leak)
}

int main(int argc, char **argv) {
    if (argc > 2 && strcmp(argv[1], "shell-client") == 0) shell_client_mode(argv[0], atoi(argv[2]), 0);
    if (argc > 2 && strcmp(argv[1], "shell-client-open") == 0) shell_client_mode(argv[0], atoi(argv[2]), 1);
    if (argc == 1) {
        // Equivalent to `exec gosu mysql /usr/local/bin/docker-entrypoint.sh ...`: all subsequent shell
        // work runs in a freshly exec'd image and therefore exercises the engine's exec fd sweep first.
        char *av[] = {argv[0], "shell-reexec", NULL};
        execve("/proc/self/exe", av, environ);
        printf("cloexec_exec ok=0 reexec\n");
        return 0;
    }
    if (argc > 1 && strcmp(argv[1], "shell-reexec") != 0) child_mode(argc, argv);

    int cx[2], pl[2], cleared[2], stopped[2];
    if (pipe2(cx, O_CLOEXEC) != 0 || pipe(pl) != 0 || pipe2(cleared, O_CLOEXEC) != 0 || pipe(stopped) != 0) {
        printf("cloexec_exec ok=0 setup\n");
        return 0;
    }
    int high = fcntl(cleared[0], F_DUPFD_CLOEXEC, 63);
    int lowest = fcntl(cleared[0], F_DUPFD_CLOEXEC, 64);
    if (high != 63 || lowest != 64 || fcntl(high, F_SETFD, 0) != 0) {
        printf("cloexec_exec ok=0 setup\n");
        return 0;
    }
    close(lowest);
    struct rlimit nofile;
    errno = 0;
    int beyond = getrlimit(RLIMIT_NOFILE, &nofile) == 0 && nofile.rlim_max <= INT_MAX
                     ? fcntl(cleared[0], F_DUPFD_CLOEXEC, (int)nofile.rlim_max)
                     : -2;
    int boundary_errno = errno;
    int private_boundary = beyond == -1 && boundary_errno == EINVAL && nofile.rlim_cur <= nofile.rlim_max;
    close(cleared[0]);
    cleared[0] = high;

    // Bash process substitution first forks a writer sibling, then forks+execs the consumer that inherits
    // the cleared high descriptor. The sibling must not mutate or retire the parent's open description.
    pid_t writer = fork();
    if (writer == 0) {
        close(cleared[0]);
        ssize_t written = write(cleared[1], "client\n", 7);
        close(cleared[1]);
        _exit(written == 7 ? 0 : 1);
    }
    close(cleared[1]);
    char high_text[16];
    snprintf(high_text, sizeof high_text, "%d", high);
    pid_t checker = fork();
    if (checker == 0) {
        char *av[] = {argv[0], "check", high_text, NULL};
        execve("/proc/self/exe", av, environ);
        _exit(127);
    }
    int checker_status = 0;
    waitpid(checker, &checker_status, 0);
    int process_substitution = WIFEXITED(checker_status) && WEXITSTATUS(checker_status) == 0;
    kill(writer, SIGKILL);
    waitpid(writer, NULL, 0);
    close(cleared[0]);

    // A WUNTRACED report is not a reap. The stopped child must retain its descriptor table and therefore
    // keep this pipe writer live until it is continued, killed, and reaped.
    pid_t stopped_child = fork();
    if (stopped_child == 0) {
        close(stopped[0]);
        pause();
        _exit(0);
    }
    close(stopped[1]);
    fcntl(stopped[0], F_SETFL, O_NONBLOCK);
    kill(stopped_child, SIGSTOP);
    int stopped_status = 0;
    pid_t stopped_wait = waitpid(stopped_child, &stopped_status, WUNTRACED);
    char stopped_byte;
    ssize_t stopped_read = read(stopped[0], &stopped_byte, 1);
    int stopped_open = stopped_wait == stopped_child && WIFSTOPPED(stopped_status) && stopped_read < 0 && errno == EAGAIN;
    kill(stopped_child, SIGCONT);
    kill(stopped_child, SIGKILL);
    waitpid(stopped_child, NULL, 0);

    pid_t ch = fork();
    if (ch == 0) {
        char *av[] = {argv[0], "child", NULL};
        execve("/proc/self/exe", av, environ);
        _exit(127);
    }
    // Parent: release our own write ends so only a child-side leak can keep a pipe open.
    close(cx[1]);
    close(pl[1]);
    fcntl(cx[0], F_SETFL, O_NONBLOCK);
    fcntl(pl[0], F_SETFL, O_NONBLOCK);

    int cloexec_eof = drained_to_eof(cx[0]); // CLOEXEC write end must be swept by the child's exec -> EOF
    // The PLAIN write end must survive the child's exec: with the child still blocked, the read blocks.
    char b[8];
    ssize_t r = read(pl[0], b, sizeof b);
    int plain_open = (r < 0 && errno == EAGAIN);

    kill(ch, SIGKILL);
    waitpid(ch, NULL, 0);
    int nested_shell = shell_pipeline(argv[0]);
    int ok = cloexec_eof && plain_open && process_substitution && stopped_open && private_boundary && nested_shell == 1;
    if (!ok)
        printf("cloexec_exec ok=0 parts=%d%d%d%d%d/%d boundary=%d/%d/%llu/%llu\n", cloexec_eof, plain_open,
               process_substitution, stopped_open, private_boundary, nested_shell, beyond, boundary_errno,
               (unsigned long long)nofile.rlim_cur, (unsigned long long)nofile.rlim_max);
    else
        printf("cloexec_exec ok=1\n");
    return ok ? 0 : 1;
}
