/* How a restored member ENDS, from the member's own side.
 *
 * A restored member is not a child of the host that holds it, so nothing on the host can reap it: the only
 * way the host learns how it ended is the report the member sends on its checkpoint channel on its way out.
 * This fixture parks a parent and one child across a capture, and after the restore ends the child either
 * cleanly or by a genuine CPU fault, so the two records can be told apart at the host.
 *
 * The parent survives its child deliberately. The host must be able to read the member's record while the
 * restored tree is still up, and a tree whose last process has exited is torn down. */
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>

/* Not a constant, so -O2 cannot fold the store below into a trap instruction: the fault must be a real
 * unmapped-address store taken in translated guest code, which is the path a guest SIGSEGV takes. */
static int *volatile fault_address;

static int exists(const char *path) {
    return access(path, F_OK) == 0;
}

static void join(char *out, size_t size, const char *directory, const char *name) {
    snprintf(out, size, "%s/%s", directory, name);
}

static int publish(const char *path, const char *text) {
    size_t size = strlen(text);
    int marker = open(path, O_WRONLY | O_CREAT | O_EXCL, 0600);
    if (marker < 0 || write(marker, text, size) != (ssize_t)size) return -1;
    return close(marker);
}

static void park_until(const char *path) {
    while (!exists(path)) {
        if (errno != ENOENT) return;
        usleep(1000);
    }
}

int main(int argc, char **argv) {
    if (argc != 3) return 2;
    const char *mode = argv[1], *directory = argv[2];
    char ready[1024], release[1024], result[1024], member[1024], finish[1024], parent_ready[1024];
    int descriptors[2];
    pid_t child;
    join(ready, sizeof ready, directory, "ready");
    join(release, sizeof release, directory, "release");
    join(result, sizeof result, directory, "result");
    join(member, sizeof member, directory, "member");
    join(finish, sizeof finish, directory, "finish");
    join(parent_ready, sizeof parent_ready, directory, "parent-ready");
    if (pipe(descriptors) != 0) return 3;
    child = fork();
    if (child < 0) return 4;
    if (child == 0) {
        char pid[32];
        close(descriptors[0]);
        /* The guest pid is the name the image will know this member by, and a restore re-forks it under
           exactly that number -- so publishing it here, before the capture, names the restored member too. */
        snprintf(pid, sizeof pid, "%d", (int)getpid());
        if (publish(member, pid) != 0) _exit(5);
        if (publish(ready, "R") != 0) _exit(6);
        park_until(release);
        if (strcmp(mode, "signal") == 0) {
            *fault_address = 1;
            _exit(7); /* unreachable: the store above has no guest handler and is fatal */
        }
        _exit(37);
    }
    close(descriptors[1]);
    /* Published only in the parent, and only after fork(2) has returned in it. A capture taken while a
       member is still inside fork is refused rather than waited for, and the child reaches its own marker
       first often enough that a capture gated on the child alone is intermittently refused. */
    if (publish(parent_ready, "P") != 0) return 5;
    {
        /* Blocked here across the capture, and released by the child's death closing the far end. */
        char byte = 0;
        int status = 0;
        FILE *output;
        /* Zero: the child never writes, so this returns only when its end of the pipe closes at its death. */
        if (read(descriptors[0], &byte, 1) != 0) return 8;
        if (waitpid(child, &status, 0) != child) return 11;
        output = fopen(result, "w");
        if (!output) return 9;
        fprintf(output, "signaled=%d signo=%d exited=%d code=%d\n", WIFSIGNALED(status) ? 1 : 0,
                WIFSIGNALED(status) ? WTERMSIG(status) : 0, WIFEXITED(status) ? 1 : 0,
                WIFEXITED(status) ? WEXITSTATUS(status) : 0);
        if (fclose(output) != 0) return 10;
    }
    park_until(finish);
    return 0;
}
