// execve error semantics (errno classes only, no strings): a missing path -> ENOENT, a directory ->
// EACCES/EISDIR, and a file with the execute bit but unrecognized contents -> ENOEXEC. Each attempt
// runs in a fork so a stray successful exec cannot corrupt the harness. Derived booleans only.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

// run execve in a child; return errno (child encodes it as exit code) or -1 if exec unexpectedly worked
static int exec_expect(const char *path) {
    pid_t p = fork();
    if (p == 0) {
        char *cargv[] = { (char *)path, NULL };
        execve(path, cargv, environ);
        _exit(errno & 0x7f);   // exec failed: report errno
    }
    int st = 0;
    waitpid(p, &st, 0);
    return WIFEXITED(st) ? WEXITSTATUS(st) : -1;
}

int main(void) {
    int enoent = exec_expect("/no/such/hl-binary-xyz") == (ENOENT & 0x7f);
    int isdir = exec_expect("/tmp");
    int dir_err = isdir == (EACCES & 0x7f) || isdir == (EISDIR & 0x7f);

    // ENOEXEC: a small non-ELF, non-script file with the exec bit set
    char path[64];
    snprintf(path, sizeof path, "/tmp/hl_noexec_%d", (int)getpid());
    int fd = open(path, O_CREAT | O_WRONLY | O_TRUNC, 0755);
    int wrote = fd >= 0 && write(fd, "\x01\x02not-an-executable\x03\x04", 21) == 21;
    if (fd >= 0) close(fd);
    int noexec = exec_expect(path) == (ENOEXEC & 0x7f);
    unlink(path);

    printf("exec_errno enoent=%d dir_err=%d wrote=%d noexec=%d\n",
           enoent, dir_err, wrote, noexec);
    return 0;
}
